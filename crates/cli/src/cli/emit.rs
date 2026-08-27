//! Rendering sinks for engine events (FR-IFACE-04).
//!
//! `zcode run --json` needs no emitter — the telemetry port already writes JSONL
//! to stdout. The pretty printer here is for humans; the TUI has its own
//! bridge onto its message channel (`tui::EventBridge`).

use std::io::Write;

use domain::{Emitter, UiEvent};

/// Human-readable streaming output for `zcode run` without `--json`.
pub struct PrettyEmitter<W: Write> {
    out: W,
    /// True once the model has produced text on this line, so tool banners
    /// can insert a newline only when one is actually needed.
    mid_line: bool,
}

impl<W: Write> PrettyEmitter<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            mid_line: false,
        }
    }

    fn break_line(&mut self) {
        if self.mid_line {
            let _ = writeln!(self.out);
            self.mid_line = false;
        }
    }
}

impl<W: Write> Emitter for PrettyEmitter<W> {
    fn emit(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Delta(text) => {
                let _ = write!(self.out, "{text}");
                let _ = self.out.flush();
                self.mid_line = !text.ends_with('\n');
            }
            UiEvent::ToolCallStart { name, .. } => {
                self.break_line();
                let _ = writeln!(self.out, "· {name}");
            }
            UiEvent::ToolResult {
                name,
                content,
                error,
                ..
            } => {
                self.break_line();
                match error {
                    Some(message) => {
                        let _ = writeln!(self.out, "  {name}: error: {}", sanitize(&message));
                    }
                    None => {
                        let _ = writeln!(self.out, "  {name}: {}", summarize(&content));
                    }
                }
            }
            UiEvent::Error(message) => {
                self.break_line();
                let _ = writeln!(self.out, "! {}", sanitize(&message));
            }
            UiEvent::LoopEnd { truncated, .. } => {
                self.break_line();
                if truncated {
                    let _ = writeln!(self.out, "! stopped at the turn/token cap");
                }
            }
            UiEvent::Retry(notice) => {
                self.break_line();
                // Written to stdout, not just the log: a headless run that
                // pauses for 30s should say why.
                let _ = writeln!(self.out, "↻ {}", notice.render());
            }
            UiEvent::Notice(message) => {
                self.break_line();
                let _ = writeln!(self.out, "· {}", sanitize(&message));
            }
            UiEvent::ToolCallArgs { .. } | UiEvent::Finish(_) | UiEvent::LoopStart { .. } => {}
        }
        let _ = self.out.flush();
    }
}

/// Strip terminal control sequences from tool output before printing it
/// (NFR-SEC-04): a tool result must not be able to move the cursor, clear the
/// screen, or fake UI chrome.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// One-line preview of a tool result for the pretty printer.
fn summarize(content: &str) -> String {
    const MAX: usize = 120;
    let cleaned = sanitize(content);
    let first_line = cleaned.lines().next().unwrap_or_default();
    let extra_lines = cleaned.lines().count().saturating_sub(1);
    let mut out: String = first_line.chars().take(MAX).collect();
    if first_line.chars().count() > MAX {
        out.push('…');
    }
    if extra_lines > 0 {
        out.push_str(&format!(" (+{extra_lines} more lines)"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{LlmFinish, LlmFinishReason};

    fn render(events: Vec<UiEvent>) -> String {
        let mut buffer: Vec<u8> = Vec::new();
        {
            let mut emitter = PrettyEmitter::new(&mut buffer);
            for ev in events {
                emitter.emit(ev);
            }
        }
        String::from_utf8(buffer).unwrap()
    }

    #[test]
    fn streams_deltas_verbatim() {
        let out = render(vec![
            UiEvent::Delta("hello ".into()),
            UiEvent::Delta("world".into()),
        ]);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn announces_tool_calls_and_results_on_their_own_lines() {
        let out = render(vec![
            UiEvent::Delta("thinking".into()),
            UiEvent::ToolCallStart {
                id: "c1".into(),
                name: "read".into(),
            },
            UiEvent::ToolResult {
                tool_call_id: "c1".into(),
                name: "read".into(),
                content: "line one\nline two\nline three".into(),
                error: None,
                elapsed_ms: 12,
            },
        ]);
        assert!(out.contains("thinking\n· read\n"));
        assert!(out.contains("read: line one (+2 more lines)"));
    }

    #[test]
    fn shows_tool_errors() {
        let out = render(vec![UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "shell".into(),
            content: String::new(),
            error: Some("blocked by allowlist".into()),
            elapsed_ms: 12,
        }]);
        assert!(out.contains("shell: error: blocked by allowlist"));
    }

    #[test]
    fn reports_cap_truncation() {
        let out = render(vec![UiEvent::LoopEnd {
            steps: 20,
            finish_reason: LlmFinishReason::Length,
            truncated: true,
        }]);
        assert!(out.contains("turn/token cap"));
    }

    #[test]
    fn quiet_events_render_nothing() {
        let out = render(vec![
            UiEvent::LoopStart {
                step: 1,
                max_turns: 20,
            },
            UiEvent::ToolCallArgs {
                id: "c1".into(),
                arguments: "{}".into(),
            },
            UiEvent::Finish(LlmFinish {
                reason: LlmFinishReason::Stop,
                input_tokens: 1,
                output_tokens: 1,
                cache_tokens: 0,
            }),
        ]);
        assert!(out.is_empty());
    }

    #[test]
    fn a_retry_is_reported_so_a_pause_is_explicable() {
        // A headless run that stops for 30s must say why, not look hung.
        let out = render(vec![UiEvent::Retry(domain::RetryNotice {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 2_000,
            status: Some(429),
            reason: "rate limited by the provider".into(),
        })]);
        assert!(out.contains("rate limited"), "{out}");
        assert!(out.contains("429"), "{out}");
    }

    #[test]
    fn notices_are_printed_and_sanitised() {
        let out = render(vec![UiEvent::Notice("\u{1b}[2Jheads up".into())]);
        assert!(out.contains("heads up"));
        assert!(!out.contains('\u{1b}'));
    }

    #[test]
    fn strips_terminal_escapes_from_tool_output() {
        // A tool result must not be able to repaint the terminal (NFR-SEC-04).
        let out = render(vec![UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "shell".into(),
            content: "\u{1b}[2Jcleared\u{7}".into(),
            error: None,
            elapsed_ms: 12,
        }]);
        assert!(!out.contains('\u{1b}'));
        assert!(!out.contains('\u{7}'));
        assert!(out.contains("cleared"));
    }

    #[test]
    fn sanitize_keeps_newlines_and_tabs() {
        assert_eq!(sanitize("a\tb\nc\u{1b}d"), "a\tb\ncd");
    }
}
