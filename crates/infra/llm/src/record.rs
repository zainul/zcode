//! Record and replay provider traffic at the event level (FR-BUDGET-07, CE-DQ23).
//!
//! `RecordingLlm` wraps any client and writes each call's event stream to
//! `<dir>/NNNN.jsonl`, one [`LlmEvent`] per line, alongside the request as
//! `NNNN.request.json` for debugging. `ReplayLlm` serves those files back in
//! call order.
//!
//! Replay is keyed by **call sequence**, not by a hash of the request. A hash
//! would make a recording go stale every time a tool description, a system
//! prompt or a truncation budget changed — which is exactly what the
//! evaluation harness exists to measure the effect of. Sequence keying keeps a
//! recorded session replayable across those changes, so the harness and the
//! engine loop can be tested deterministically without a network or a key.
//!
//! Events rather than raw HTTP bodies: replay then works for every provider
//! with one format, and the provider decoders keep their own dedicated tests
//! (`parse_*_events`), which is where wire-format correctness is verified.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use domain::{
    BoxError, LlmEvent, LlmFinish, LlmFinishReason, LlmPort, LlmRequest, LlmResponse, RetryNotice,
};
use serde::{Deserialize, Serialize};

/// On-disk mirror of `domain::LlmEvent` (domain carries no serde, FR-DI-01).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum EventFile {
    Delta {
        text: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgs {
        id: String,
        arguments: String,
    },
    Retry {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        #[serde(default)]
        status: Option<u16>,
        reason: String,
    },
    LearnedContextWindow {
        model: String,
        tokens: u64,
    },
    Finish {
        reason: String,
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default)]
        cache_read_tokens: u64,
        #[serde(default)]
        cache_write_tokens: u64,
        #[serde(default)]
        cost_usd: Option<f64>,
    },
}

fn reason_to_str(r: LlmFinishReason) -> &'static str {
    match r {
        LlmFinishReason::Stop => "stop",
        LlmFinishReason::ToolUse => "tool_use",
        LlmFinishReason::Length => "length",
    }
}

fn reason_from_str(s: &str) -> LlmFinishReason {
    match s {
        "tool_use" => LlmFinishReason::ToolUse,
        "length" => LlmFinishReason::Length,
        _ => LlmFinishReason::Stop,
    }
}

impl From<&LlmEvent> for EventFile {
    fn from(ev: &LlmEvent) -> Self {
        match ev.clone() {
            LlmEvent::Delta(text) => Self::Delta { text },
            LlmEvent::ToolCallStart { id, name } => Self::ToolCallStart { id, name },
            LlmEvent::ToolCallArgs { id, arguments } => Self::ToolCallArgs { id, arguments },
            LlmEvent::Retry(n) => Self::Retry {
                attempt: n.attempt,
                max_attempts: n.max_attempts,
                delay_ms: n.delay_ms,
                status: n.status,
                reason: n.reason,
            },
            LlmEvent::LearnedContextWindow { model, tokens } => {
                Self::LearnedContextWindow { model, tokens }
            }
            LlmEvent::Finish(f) => Self::Finish {
                reason: reason_to_str(f.reason).into(),
                input_tokens: f.input_tokens,
                output_tokens: f.output_tokens,
                cache_read_tokens: f.cache_tokens,
                cache_write_tokens: 0,
                cost_usd: f.cost_usd,
            },
        }
    }
}

impl From<EventFile> for LlmEvent {
    fn from(ev: EventFile) -> Self {
        match ev {
            EventFile::Delta { text } => Self::Delta(text),
            EventFile::ToolCallStart { id, name } => Self::ToolCallStart { id, name },
            EventFile::ToolCallArgs { id, arguments } => Self::ToolCallArgs { id, arguments },
            EventFile::Retry {
                attempt,
                max_attempts,
                delay_ms,
                status,
                reason,
            } => Self::Retry(RetryNotice {
                attempt,
                max_attempts,
                delay_ms,
                status,
                reason,
            }),
            EventFile::LearnedContextWindow { model, tokens } => {
                Self::LearnedContextWindow { model, tokens }
            }
            EventFile::Finish {
                reason,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                cost_usd,
            } => Self::Finish(LlmFinish {
                reason: reason_from_str(&reason),
                input_tokens,
                output_tokens,
                cache_tokens: cache_read_tokens + cache_write_tokens,
                cost_usd,
            }),
        }
    }
}

fn call_path(dir: &Path, n: usize, suffix: &str) -> PathBuf {
    dir.join(format!("{n:04}{suffix}"))
}

/// Serves recorded calls back in order.
pub struct ReplayLlm {
    dir: PathBuf,
    next: Arc<AtomicUsize>,
}

impl ReplayLlm {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            next: Arc::new(AtomicUsize::new(1)),
        }
    }

    fn load(&self, n: usize) -> Result<Vec<LlmEvent>, BoxError> {
        let path = call_path(&self.dir, n, ".jsonl");
        let text = fs::read_to_string(&path).map_err(|e| {
            format!(
                "replay has no recording for call {n} ({}): {e}",
                path.display()
            )
        })?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, line)| {
                serde_json::from_str::<EventFile>(line)
                    .map(LlmEvent::from)
                    .map_err(|e| format!("{}:{}: {e}", path.display(), i + 1).into())
            })
            .collect()
    }
}

impl LlmPort for ReplayLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        collect_response(self.stream(req))
    }

    fn stream(
        &mut self,
        _req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        match self.load(n) {
            Ok(events) => Box::new(events.into_iter().map(Ok)),
            Err(e) => Box::new(std::iter::once(Err(e))),
        }
    }
}

/// Wraps a client and writes every call's events and request to `dir`.
pub struct RecordingLlm {
    inner: Box<dyn LlmPort + Send>,
    dir: PathBuf,
    next: usize,
}

impl RecordingLlm {
    pub fn new(inner: Box<dyn LlmPort + Send>, dir: impl Into<PathBuf>) -> Result<Self, BoxError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        // Continue numbering after an existing recording rather than
        // overwriting it, so several processes (a TUI provider switch, say)
        // append to one session's recording.
        let next = (1..)
            .find(|n| !call_path(&dir, *n, ".jsonl").exists())
            .unwrap_or(1);
        Ok(Self { inner, dir, next })
    }
}

impl LlmPort for RecordingLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        collect_response(self.stream(req))
    }

    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let n = self.next;
        self.next += 1;
        let request = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": req.messages.len(),
            "tools": req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        });
        let _ = fs::write(
            call_path(&self.dir, n, ".request.json"),
            serde_json::to_vec_pretty(&request).unwrap_or_default(),
        );
        let path = call_path(&self.dir, n, ".jsonl");
        let mut lines = String::new();
        let events: Vec<Result<LlmEvent, BoxError>> = self.inner.stream(req).collect();
        for ev in events.iter().flatten() {
            if let Ok(line) = serde_json::to_string(&EventFile::from(ev)) {
                lines.push_str(&line);
                lines.push('\n');
            }
        }
        let _ = fs::write(path, lines);
        Box::new(events.into_iter())
    }
}

fn collect_response(
    events: Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send>,
) -> Result<LlmResponse, BoxError> {
    let mut text = String::new();
    let mut finish = None;
    for ev in events {
        match ev? {
            LlmEvent::Delta(t) => text.push_str(&t),
            LlmEvent::Finish(f) => finish = Some(f),
            _ => {}
        }
    }
    let finish = finish.ok_or("recorded call has no finish event")?;
    Ok(LlmResponse {
        text,
        finish,
        raw: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Vec<LlmEvent>);
    impl LlmPort for Scripted {
        fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
            collect_response(self.stream(req))
        }
        fn stream(
            &mut self,
            _req: &LlmRequest,
        ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
            Box::new(self.0.clone().into_iter().map(Ok))
        }
    }

    fn request() -> LlmRequest {
        LlmRequest {
            messages: Box::new([domain::LlmMessage::user("hi")]),
            tools: Box::new([]),
            model: "m".into(),
            max_tokens: 10,
            temperature: 0.0,
            images: Box::new([]),
        }
    }

    fn script() -> Vec<LlmEvent> {
        vec![
            LlmEvent::Delta("hello".into()),
            LlmEvent::ToolCallStart {
                id: "c1".into(),
                name: "read".into(),
            },
            LlmEvent::ToolCallArgs {
                id: "c1".into(),
                arguments: r#"{"path":"a"}"#.into(),
            },
            LlmEvent::Finish(LlmFinish {
                reason: LlmFinishReason::ToolUse,
                input_tokens: 120,
                output_tokens: 7,
                cache_tokens: 30,
                cost_usd: Some(0.001),
            }),
        ]
    }

    fn render(events: &[LlmEvent]) -> Vec<String> {
        events.iter().map(|e| format!("{e:?}")).collect()
    }

    #[test]
    fn a_recorded_call_replays_identically_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = RecordingLlm::new(Box::new(Scripted(script())), dir.path()).unwrap();
        let first: Vec<LlmEvent> = rec.stream(&request()).map(Result::unwrap).collect();
        let second: Vec<LlmEvent> = rec.stream(&request()).map(Result::unwrap).collect();
        assert!(dir.path().join("0001.jsonl").exists());
        assert!(dir.path().join("0002.request.json").exists());

        let mut replay = ReplayLlm::new(dir.path());
        let r1: Vec<LlmEvent> = replay.stream(&request()).map(Result::unwrap).collect();
        let r2: Vec<LlmEvent> = replay.stream(&request()).map(Result::unwrap).collect();
        assert_eq!(render(&r1), render(&first));
        assert_eq!(render(&r2), render(&second));
    }

    #[test]
    fn replay_past_the_recording_is_a_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut replay = ReplayLlm::new(dir.path());
        let err = replay.stream(&request()).next().unwrap().unwrap_err();
        assert!(err.to_string().contains("no recording for call 1"), "{err}");
    }

    #[test]
    fn recording_continues_numbering_after_an_existing_recording() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = RecordingLlm::new(Box::new(Scripted(script())), dir.path()).unwrap();
        let _ = a.stream(&request()).count();
        let mut b = RecordingLlm::new(Box::new(Scripted(script())), dir.path()).unwrap();
        let _ = b.stream(&request()).count();
        assert!(dir.path().join("0002.jsonl").exists());
    }

    #[test]
    fn send_collects_text_and_finish() {
        let mut replay_dir_llm = Scripted(script());
        let resp = replay_dir_llm.send(&request()).unwrap();
        assert_eq!(resp.text, "hello");
        assert_eq!(resp.finish.input_tokens, 120);
    }
}
