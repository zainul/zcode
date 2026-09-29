//! Compaction policy (FR-CTX-03..06, technical plan CE-DQ4, §5.3).
//!
//! Pure functions over a transcript: which messages are **protected**, which
//! tool results a later event made obsolete (**supersession**, Tier 1), which
//! old outputs can be replaced by a stub (**elision**, Tier 2), what the stubs
//! say, and whether a rewritten transcript is still **valid** for every
//! provider. The orchestration — when to compact, how far, what to persist —
//! lives in `app`; everything here is stdlib-only and exhaustively testable.
//!
//! Recency is positional: the protected window is the last K *assistant
//! turns* of the transcript, not a step-number range, because step numbers
//! restart with each run while the transcript carries on across runs.

use crate::ports::{LlmMessage, LlmRole, LlmToolCall, MessageKind, Subject};

/// Tunables for one compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The most recent assistant turns (with their tool results) that are
    /// never touched (FR-CTX-03).
    pub keep_recent_steps: u32,
    /// Tool output smaller than this is not worth a stub.
    pub elide_over_tokens: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            keep_recent_steps: 6,
            elide_over_tokens: 400,
        }
    }
}

/// Tool names whose call *arguments* carry file content, and so are worth
/// eliding once written (a 300-line `write` re-sent on every later step).
pub const EDIT_TOOLS: &[&str] = &["write", "str_replace_editor", "apply_patch", "edit_symbol"];

/// What Tier 2 may replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elidable {
    /// A tool-result message.
    Result(usize),
    /// The arguments of call `call` in assistant message `msg`.
    CallArgs { msg: usize, call: usize },
}

/// Why a result was superseded, for its stub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Supersession {
    /// The same (or a wider) range of the file was read again.
    ReadAgain { path: String, step: u32 },
    /// The file was written after it was read.
    Modified { path: String, step: u32 },
    /// The same call was made again (a search, a listing, diagnostics).
    Repeated { step: u32 },
}

/// Index of the first message of the protected recent window.
fn recent_window_start(h: &[LlmMessage], keep: u32) -> usize {
    let assistants: Vec<usize> = h
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == LlmRole::Assistant)
        .map(|(i, _)| i)
        .collect();
    let keep = keep.max(1) as usize;
    if assistants.len() <= keep {
        // Everything from the first assistant turn on is recent.
        return assistants.first().copied().unwrap_or(h.len());
    }
    assistants[assistants.len() - keep]
}

/// FR-CTX-03: `true` = must not change. The system prompt, the first user
/// message (the task), the latest user message, and the recent window.
pub fn protected(h: &[LlmMessage], p: &Policy) -> Vec<bool> {
    let mut prot = vec![false; h.len()];
    if let Some(first) = h.first() {
        if first.role == LlmRole::System {
            prot[0] = true;
        }
    }
    if let Some(i) = h.iter().position(|m| m.role == LlmRole::User) {
        prot[i] = true;
    }
    if let Some(i) = h.iter().rposition(|m| m.role == LlmRole::User) {
        prot[i] = true;
    }
    for flag in prot
        .iter_mut()
        .skip(recent_window_start(h, p.keep_recent_steps))
    {
        *flag = true;
    }
    prot
}

fn paths_written(subject: &Subject) -> Vec<&str> {
    match subject {
        Subject::FileWrite { path } => vec![path.as_str()],
        Subject::FileWrites { paths } => paths.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Whether `later` makes `earlier` obsolete.
fn supersedes(earlier: &Subject, later: &Subject, later_step: u32) -> Option<Supersession> {
    match earlier {
        Subject::FileRange {
            path, start, end, ..
        } => {
            if let Subject::FileRange {
                path: p2,
                start: s2,
                end: e2,
                ..
            } = later
            {
                if p2 == path && s2 <= start && e2 >= end {
                    return Some(Supersession::ReadAgain {
                        path: path.clone(),
                        step: later_step,
                    });
                }
            }
            paths_written(later)
                .contains(&path.as_str())
                .then(|| Supersession::Modified {
                    path: path.clone(),
                    step: later_step,
                })
        }
        Subject::Diagnostics { path } => match later {
            Subject::Diagnostics { path: p2 } if p2 == path || (p2.is_none() && path.is_some()) => {
                Some(Supersession::Repeated { step: later_step })
            }
            _ => None,
        },
        Subject::Search { .. } | Subject::Listing { .. } => {
            (earlier == later).then_some(Supersession::Repeated { step: later_step })
        }
        Subject::FileWrite { .. } | Subject::FileWrites { .. } => None,
    }
}

/// Tier 1 (FR-CTX-05): tool results made obsolete by a later message, with
/// the reason. Already-elided results and protected ones are skipped.
pub fn superseded(h: &[LlmMessage], prot: &[bool]) -> Vec<(usize, Supersession)> {
    let mut out = Vec::new();
    for (i, m) in h.iter().enumerate() {
        if prot.get(i).copied().unwrap_or(true)
            || m.role != LlmRole::Tool
            || m.meta.kind == MessageKind::Elided
        {
            continue;
        }
        let Some(subject) = &m.meta.subject else {
            continue;
        };
        let reason = h[i + 1..].iter().find_map(|later| {
            later
                .meta
                .subject
                .as_ref()
                .and_then(|s| supersedes(subject, s, later.meta.step))
        });
        if let Some(reason) = reason {
            out.push((i, reason));
        }
    }
    out
}

/// Tier 2 (FR-CTX-06): large unprotected tool results and the large
/// arguments of old edit calls, oldest first.
pub fn elidable(h: &[LlmMessage], prot: &[bool], p: &Policy) -> Vec<Elidable> {
    let mut out = Vec::new();
    for (i, m) in h.iter().enumerate() {
        if prot.get(i).copied().unwrap_or(true) || m.meta.kind == MessageKind::Elided {
            continue;
        }
        match m.role {
            LlmRole::Tool if m.meta.tokens_est > p.elide_over_tokens => {
                out.push(Elidable::Result(i));
            }
            LlmRole::Assistant => {
                for (c, call) in m.tool_calls.iter().enumerate() {
                    let big = crate::tokens::estimate_tokens(&call.arguments)
                        > u64::from(p.elide_over_tokens);
                    if big && EDIT_TOOLS.contains(&crate::canonical_tool_name(&call.name).as_str())
                    {
                        out.push(Elidable::CallArgs { msg: i, call: c });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The call that produced tool result `i`, if it is in the transcript.
pub fn call_for(h: &[LlmMessage], i: usize) -> Option<&LlmToolCall> {
    let id = &h.get(i)?.tool_result.as_ref()?.tool_call_id;
    h[..i]
        .iter()
        .rev()
        .flat_map(|m| m.tool_calls.iter())
        .find(|c| &c.id == id)
}

fn thousands_k(tokens: u32) -> String {
    if tokens >= 1_000 {
        format!("{:.1}k", f64::from(tokens) / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn describe(m: &LlmMessage, call: Option<&LlmToolCall>) -> String {
    let tool = call.map_or("a tool", |c| c.name.as_str());
    match &m.meta.subject {
        Some(Subject::FileRange {
            path, start, end, ..
        }) => format!("{tool} {path} lines {start}-{end}"),
        Some(Subject::Listing { path }) => format!("{tool} {path}"),
        Some(Subject::Diagnostics { path: Some(p) }) => format!("{tool} {p}"),
        _ => tool.to_string(),
    }
}

/// The text a superseded result is replaced with. Deterministic: no times,
/// nothing that differs between two runs of the same history.
pub fn superseded_stub(reason: &Supersession) -> String {
    match reason {
        Supersession::ReadAgain { path, step } => {
            format!("[superseded — {path} was read again at step {step}; see that result]")
        }
        Supersession::Modified { path, step } => {
            format!("[superseded — {path} was modified at step {step}; read it again if needed]")
        }
        Supersession::Repeated { step } => {
            format!("[superseded — the same call was made again at step {step}; see that result]")
        }
    }
}

/// The text an elided result is replaced with: what it was and how to get it
/// back, so the model does not repeat work or lose the thread.
pub fn elided_stub(m: &LlmMessage, call: Option<&LlmToolCall>) -> String {
    let what = describe(m, call);
    let size = thousands_k(m.meta.tokens_est);
    let head = m
        .tool_result
        .as_ref()
        .and_then(|r| r.content.lines().find(|l| !l.trim().is_empty()))
        .map(|l| {
            let clipped: String = l.chars().take(120).collect();
            format!("; began: {clipped}")
        })
        .unwrap_or_default();
    match &m.meta.spill {
        Some(path) => format!(
            "[elided — {what} at step {}, ~{size} tokens{head}; full text: {path}]",
            m.meta.step
        ),
        None => format!(
            "[elided — {what} at step {}, ~{size} tokens{head}; re-run the call to see it again]",
            m.meta.step
        ),
    }
}

/// Transcript invariants every provider relies on (FR-CTX-04): each tool
/// result answers a call made earlier, each call is answered exactly once,
/// and results follow their call's assistant message with nothing but other
/// results in between.
pub fn validate(h: &[LlmMessage]) -> Result<(), String> {
    let mut open: Vec<&str> = Vec::new();
    for (i, m) in h.iter().enumerate() {
        match m.role {
            LlmRole::Tool => {
                let Some(r) = &m.tool_result else {
                    return Err(format!("message {i} is a tool message with no result"));
                };
                match open.iter().position(|id| *id == r.tool_call_id) {
                    Some(p) => {
                        open.remove(p);
                    }
                    None => {
                        return Err(format!(
                            "message {i} answers `{}`, which no open call made",
                            r.tool_call_id
                        ))
                    }
                }
            }
            _ => {
                if !open.is_empty() {
                    return Err(format!(
                        "message {i} interrupts unanswered call(s): {}",
                        open.join(", ")
                    ));
                }
                if m.role == LlmRole::Assistant {
                    open.extend(m.tool_calls.iter().map(|c| c.id.as_str()));
                }
            }
        }
    }
    // Calls still open at the very end are fine: the engine appends their
    // results next. Anything else is a broken transcript.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{LlmToolResult, MessageMeta};

    fn call(id: &str, name: &str, args: &str) -> LlmToolCall {
        LlmToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    fn assistant(step: u32, calls: Vec<LlmToolCall>) -> LlmMessage {
        let mut m = LlmMessage::assistant("");
        m.tool_calls = calls.into_boxed_slice();
        m.meta.step = step;
        m
    }

    fn result(id: &str, step: u32, subject: Option<Subject>, tokens: u32) -> LlmMessage {
        let mut m = LlmMessage::tool_result_message(LlmToolResult {
            tool_call_id: id.into(),
            content: "x ".repeat(tokens as usize),
        });
        m.meta = MessageMeta {
            step,
            subject,
            tokens_est: tokens,
            ..MessageMeta::default()
        };
        m
    }

    fn range(path: &str, start: u32, end: u32) -> Option<Subject> {
        Some(Subject::FileRange {
            path: path.into(),
            start,
            end,
            hash: 1,
        })
    }

    /// system, task, then `n` steps of [assistant(read), result].
    fn session(n: u32) -> Vec<LlmMessage> {
        let mut h = vec![LlmMessage::system("sys"), LlmMessage::user("task")];
        for s in 1..=n {
            let id = format!("c{s}");
            h.push(assistant(s, vec![call(&id, "read", "{}")]));
            h.push(result(&id, s, range(&format!("f{s}.rs"), 1, 10), 500));
        }
        h
    }

    #[test]
    fn protects_system_first_and_last_user_and_the_recent_window() {
        let mut h = session(10);
        h.push(LlmMessage::user("follow-up"));
        let p = Policy {
            keep_recent_steps: 3,
            ..Policy::default()
        };
        let prot = protected(&h, &p);
        assert!(prot[0] && prot[1] && prot[h.len() - 1]);
        // The last three assistant turns (and what follows them) are recent.
        let recent = recent_window_start(&h, 3);
        assert_eq!(h[recent].meta.step, 8);
        assert!(prot[recent..].iter().all(|p| *p));
        assert!(!prot[2] && !prot[3], "old steps are fair game");
    }

    #[test]
    fn a_wider_later_read_supersedes_a_narrower_earlier_one_but_not_vice_versa() {
        let mut h = session(0);
        h.push(assistant(1, vec![call("a", "read", "{}")]));
        h.push(result("a", 1, range("x.rs", 10, 20), 500));
        h.push(assistant(2, vec![call("b", "read", "{}")]));
        h.push(result("b", 2, range("x.rs", 1, 400), 500));
        h.push(assistant(3, vec![call("c", "read", "{}")]));
        h.push(result("c", 3, range("x.rs", 5, 6), 500));
        let prot = vec![false; h.len()];
        let found = superseded(&h, &prot);
        assert_eq!(
            found,
            [(
                3,
                Supersession::ReadAgain {
                    path: "x.rs".into(),
                    step: 2
                }
            )],
            "the 1-400 read is not superseded by the narrower 5-6"
        );
    }

    #[test]
    fn a_write_supersedes_earlier_reads_of_that_path_including_a_multi_file_patch() {
        let mut h = session(0);
        h.push(assistant(1, vec![call("a", "read", "{}")]));
        h.push(result("a", 1, range("x.rs", 1, 40), 500));
        h.push(assistant(2, vec![call("b", "read", "{}")]));
        h.push(result("b", 2, range("y.rs", 1, 40), 500));
        h.push(assistant(3, vec![call("c", "apply_patch", "{}")]));
        h.push(result(
            "c",
            3,
            Some(Subject::FileWrites {
                paths: vec!["y.rs".into(), "z.rs".into()],
            }),
            20,
        ));
        let found = superseded(&h, &vec![false; h.len()]);
        assert_eq!(
            found,
            [(
                5,
                Supersession::Modified {
                    path: "y.rs".into(),
                    step: 3
                }
            )]
        );
    }

    #[test]
    fn repeated_searches_and_later_diagnostics_supersede() {
        let mut h = session(0);
        let search = Some(Subject::Search { key: 7 });
        h.push(assistant(1, vec![call("a", "grep", "{}")]));
        h.push(result("a", 1, search.clone(), 500));
        h.push(assistant(2, vec![call("b", "lsp__diagnostics", "{}")]));
        h.push(result(
            "b",
            2,
            Some(Subject::Diagnostics {
                path: Some("x.rs".into()),
            }),
            50,
        ));
        h.push(assistant(3, vec![call("c", "grep", "{}")]));
        h.push(result("c", 3, search, 500));
        h.push(assistant(4, vec![call("d", "lsp__diagnostics", "{}")]));
        h.push(result(
            "d",
            4,
            Some(Subject::Diagnostics { path: None }),
            50,
        ));
        let found: Vec<usize> = superseded(&h, &vec![false; h.len()])
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        assert_eq!(found, [3, 5]);
    }

    #[test]
    fn elidable_is_oldest_first_and_skips_protected_and_small_results() {
        let mut h = session(8);
        h[5].meta.tokens_est = 10; // small: not worth a stub
        let p = Policy {
            keep_recent_steps: 2,
            elide_over_tokens: 400,
        };
        let prot = protected(&h, &p);
        let found = elidable(&h, &prot, &p);
        assert_eq!(found.first(), Some(&Elidable::Result(3)));
        assert!(!found.contains(&Elidable::Result(5)));
        let last_result = h.len() - 1;
        assert!(!found.contains(&Elidable::Result(last_result)));
    }

    #[test]
    fn large_edit_arguments_are_elidable_but_read_arguments_are_not() {
        let mut h = session(0);
        let big = format!(
            "{{\"path\":\"a.rs\",\"content\":\"{}\"}}",
            "x;".repeat(2_000)
        );
        h.push(assistant(
            1,
            vec![call("w", "write", &big), call("r", "read", &big)],
        ));
        h.push(result("w", 1, None, 5));
        h.push(result("r", 1, None, 5));
        h.push(LlmMessage::user("next"));
        let found = elidable(&h, &vec![false; h.len()], &Policy::default());
        assert_eq!(found, [Elidable::CallArgs { msg: 2, call: 0 }]);
    }

    #[test]
    fn stubs_are_deterministic_and_say_how_to_recover() {
        let mut m = result("a", 7, range("src/lib.rs", 1, 400), 9_800);
        m.tool_result.as_mut().unwrap().content = "\n  1│use std::io;\n".into();
        let c = call("a", "read", "{}");
        assert_eq!(
            elided_stub(&m, Some(&c)),
            "[elided — read src/lib.rs lines 1-400 at step 7, ~9.8k tokens; began:   1│use std::io;; \
             re-run the call to see it again]"
        );
        m.meta.spill = Some(".zcode/spill/s/a.txt".into());
        assert!(elided_stub(&m, Some(&c)).ends_with("full text: .zcode/spill/s/a.txt]"));
        assert_eq!(
            superseded_stub(&Supersession::Modified {
                path: "a.rs".into(),
                step: 9
            }),
            "[superseded — a.rs was modified at step 9; read it again if needed]"
        );
    }

    #[test]
    fn validate_accepts_parallel_results_and_rejects_orphans() {
        let mut h = session(0);
        h.push(assistant(
            1,
            vec![call("a", "read", "{}"), call("b", "read", "{}")],
        ));
        h.push(result("b", 1, None, 1));
        h.push(result("a", 1, None, 1));
        assert!(validate(&h).is_ok());
        let mut orphan = h.clone();
        orphan.push(result("zzz", 1, None, 1));
        assert!(validate(&orphan).is_err());
        let mut interrupted = session(0);
        interrupted.push(assistant(1, vec![call("a", "read", "{}")]));
        interrupted.push(LlmMessage::user("hey"));
        interrupted.push(result("a", 1, None, 1));
        assert!(validate(&interrupted).is_err());
        assert_eq!(call_for(&h, 3).map(|c| c.id.as_str()), Some("b"));
    }

    /// Seeded property test: over many random transcripts, stubbing
    /// everything Tiers 1 and 2 select keeps the transcript valid and never
    /// changes a protected message.
    #[test]
    fn tiers_one_and_two_preserve_validity_and_protected_bytes() {
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2_000 {
            let steps = 5 + (next() % 116) as u32;
            let mut h = vec![LlmMessage::system("sys"), LlmMessage::user("task")];
            for s in 1..=steps {
                let calls = 1 + (next() % 3) as usize;
                let ids: Vec<String> = (0..calls).map(|c| format!("s{s}c{c}")).collect();
                h.push(assistant(
                    s,
                    ids.iter().map(|id| call(id, "read", "{}")).collect(),
                ));
                for id in &ids {
                    let subject = match next() % 4 {
                        0 => range(&format!("f{}.rs", next() % 5), 1, 1 + (next() % 50) as u32),
                        1 => Some(Subject::FileWrite {
                            path: format!("f{}.rs", next() % 5),
                        }),
                        2 => Some(Subject::Search { key: next() % 3 }),
                        _ => None,
                    };
                    h.push(result(id, s, subject, (next() % 2_000) as u32));
                }
                if next() % 10 == 0 {
                    h.push(LlmMessage::user("more"));
                }
            }
            let p = Policy {
                keep_recent_steps: 1 + (next() % 8) as u32,
                elide_over_tokens: 400,
            };
            let prot = protected(&h, &p);
            let before: Vec<String> = h.iter().map(|m| format!("{m:?}")).collect();
            for (i, reason) in superseded(&h, &prot) {
                h[i].tool_result.as_mut().unwrap().content = superseded_stub(&reason);
                h[i].meta.kind = MessageKind::Elided;
            }
            for e in elidable(&h, &prot, &p) {
                if let Elidable::Result(i) = e {
                    let stub = elided_stub(&h[i], call_for(&h, i));
                    h[i].tool_result.as_mut().unwrap().content = stub;
                    h[i].meta.kind = MessageKind::Elided;
                }
            }
            assert!(validate(&h).is_ok());
            for (i, was) in before.iter().enumerate() {
                if prot[i] {
                    assert_eq!(&format!("{:?}", h[i]), was, "protected message {i} changed");
                }
            }
        }
    }
}
