//! The conversation timeline: what happened, in order, with the tools that
//! did it shown inline underneath the message that called them.
//!
//! Replaces the old split panes. A separate tools pane forced the reader to
//! correlate two scrolling lists by eye; inline entries put a tool call where
//! it belongs — between the sentence that announced it and the sentence that
//! reported its result.
//!
//! # Memory
//!
//! This is the one structure that grows with a long session, so it is built to
//! stay small (NFR-PERF-03):
//!
//! * Text is `Box<str>`, not `String`: no capacity field, no growth slack. A
//!   `String` built by `push_str` typically carries ~2x its length.
//! * Every stored string is capped at ingest. A tool that returns one 100 KB
//!   line used to be retained whole, because only the *line count* was bounded.
//! * Timestamps are `u32` seconds from session start (4 bytes) rather than
//!   `SystemTime` (16), and durations are `u32` milliseconds.
//! * The entry list itself is bounded and drops from the front.
//!
//! [`Timeline::heap_bytes`] reports the total so a test can hold it to a
//! budget rather than trusting the above.

use std::time::{SystemTime, UNIX_EPOCH};

/// Entries retained. Older ones are dropped from the front.
pub const MAX_ENTRIES: usize = 400;
/// Longest message text kept for display. The full text is always in the
/// session file; this bound is about the render buffer, not the transcript.
pub const MAX_TEXT: usize = 4_000;
/// Longest tool summary kept. One line of a tool result can be enormous.
///
/// Wide enough for a failure to survive intact — the shell guard's refusal
/// carries the rule, the command, and a hint on how to fix it, and a message
/// cut before the hint is a message that cannot be acted on. At 400 entries
/// this still bounds the timeline's tool rows to ~160 KB.
pub const MAX_DETAIL: usize = 400;

/// How a tool call turned out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
    /// Refused by the mode gate or the shell guard.
    Denied,
}

impl ToolStatus {
    /// The glyph shown in the timeline gutter.
    pub fn icon(self) -> &'static str {
        match self {
            ToolStatus::Running => "◐",
            ToolStatus::Ok => "✔",
            ToolStatus::Failed => "✖",
            ToolStatus::Denied => "⊘",
        }
    }
}

/// The glyph that says what *kind* of work a tool row did.
///
/// The status icon beside it says how the call went; this one says what was
/// called, so a run of rows can be read by shape before any word is. Chosen
/// from the same geometric and dingbat blocks as the status icons, all of
/// which render one cell wide — a two-cell glyph would shift every column to
/// its right, and a timeline that only lines up sometimes is worse than one
/// with no icons at all.
pub fn tool_icon(name: &str) -> &'static str {
    let canonical = domain::canonical_tool_name(name);
    if let Some(rest) = canonical.strip_prefix("mcp__") {
        let _ = rest;
        return "⊞";
    }
    if canonical.starts_with("lsp__") {
        return "⌖";
    }
    match &*canonical {
        "read" => "◇",
        "list_dir" => "▪",
        "write" | "str_replace_editor" => "✎",
        "apply_patch" => "±",
        "shell" => "❯",
        "zcode_skill" => "✦",
        _ => "·",
    }
}

/// Severity of an engine note (a retry, a server warning).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteLevel {
    Info,
    Retry,
    Warn,
}

impl NoteLevel {
    pub fn icon(self) -> &'static str {
        match self {
            NoteLevel::Info => "·",
            NoteLevel::Retry => "↻",
            NoteLevel::Warn => "!",
        }
    }
}

/// What an entry is.
#[derive(Clone, Debug)]
pub enum EntryKind {
    User(Box<str>),
    Agent(Box<str>),
    Tool {
        name: Box<str>,
        /// One-line summary: the arguments while running, the result after.
        detail: Box<str>,
        status: ToolStatus,
        /// Wall time the call took, milliseconds. 0 while running.
        elapsed_ms: u32,
        /// Which run of consecutive calls this belongs to.
        ///
        /// Assigned once, when the call is recorded, so it survives entries
        /// being dropped from the front — a positional key would shift under
        /// the collapse state and fold the wrong group.
        run: u32,
    },
    Note {
        text: Box<str>,
        level: NoteLevel,
    },
}

/// One thing that happened.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Milliseconds since the timeline started. Four bytes instead of the
    /// sixteen a `SystemTime` would cost, and `u32` still spans 49 days.
    ///
    /// Milliseconds, not seconds: tool durations are the difference between
    /// two of these, and at second resolution every call under a second
    /// measured as zero — which `render_duration` draws as nothing at all.
    pub at_ms: u32,
    pub kind: EntryKind,
}

impl Entry {
    /// Bytes this entry owns on the heap.
    pub fn heap_bytes(&self) -> usize {
        match &self.kind {
            EntryKind::User(t) | EntryKind::Agent(t) => t.len(),
            EntryKind::Tool { name, detail, .. } => name.len() + detail.len(),
            EntryKind::Note { text, .. } => text.len(),
        }
    }

    pub fn is_tool(&self) -> bool {
        matches!(self.kind, EntryKind::Tool { .. })
    }
}

/// A bounded, ordered log of entries plus the clock they are stamped against.
#[derive(Debug)]
pub struct Timeline {
    entries: Vec<Entry>,
    /// Unix seconds when the timeline started, for absolute timestamps.
    started_unix: u64,
    /// Local offset from UTC in seconds, so the clock matches the user's.
    utc_offset: i32,
    /// Monotonic base for `at`.
    started: std::time::Instant,
    /// Id given to tool calls recorded now. Advances whenever something that
    /// is not a tool call is pushed, so a run is exactly the calls that
    /// happened between two messages.
    run: u32,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new(local_utc_offset())
    }
}

impl Timeline {
    pub fn new(utc_offset: i32) -> Self {
        Self {
            entries: Vec::new(),
            started_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            utc_offset,
            started: std::time::Instant::now(),
            run: 0,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Mutable access, for settling a row that is still being described.
    pub fn entries_mut(&mut self) -> &mut [Entry] {
        &mut self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        // Give the memory back: `clear` alone keeps the whole capacity, and a
        // `/clear` after a long session is exactly when it should be released.
        self.entries.shrink_to_fit();
    }

    /// Milliseconds since the timeline started, saturating at ~49 days.
    fn now_offset(&self) -> u32 {
        self.started.elapsed().as_millis().min(u32::MAX as u128) as u32
    }

    fn push(&mut self, kind: EntryKind) {
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.remove(0);
        }
        // Anything that is not a call ends the current run, so the next call
        // starts a new one.
        if !matches!(kind, EntryKind::Tool { .. }) {
            self.run = self.run.wrapping_add(1);
        }
        let at_ms = self.now_offset();
        self.entries.push(Entry { at_ms, kind });
    }

    /// The run tool calls recorded right now belong to.
    pub fn current_run(&self) -> u32 {
        self.run
    }

    pub fn push_user(&mut self, text: &str) {
        self.push(EntryKind::User(clamp(text, MAX_TEXT)));
    }

    pub fn push_agent(&mut self, text: &str) {
        self.push(EntryKind::Agent(clamp(text, MAX_TEXT)));
    }

    pub fn push_note(&mut self, text: &str, level: NoteLevel) {
        self.push(EntryKind::Note {
            text: clamp(text, MAX_DETAIL),
            level,
        });
    }

    /// Record a tool call as started.
    pub fn start_tool(&mut self, name: &str, detail: &str) {
        self.push(EntryKind::Tool {
            name: clamp(name, 64),
            detail: clamp(detail, MAX_DETAIL),
            status: ToolStatus::Running,
            elapsed_ms: 0,
            run: self.run,
        });
    }

    /// Settle the most recent running call for `name`.
    ///
    /// Matching by name from the end rather than by a stored index, because
    /// the index shifts when the bound drops entries from the front — and a
    /// stale index would rewrite an unrelated row.
    /// A **successful** row keeps the invocation it was annotated with — the
    /// command, the path — rather than being overwritten by the first line of
    /// the output. `shell  ls -lah` says what happened; `shell  total 32`
    /// makes you guess. A **failure** replaces it, because then the error is
    /// the thing that needs acting on, and the row carries the tool name
    /// either way.
    ///
    /// `took_ms` is measured by the engine around the dispatch itself. The
    /// timeline cannot measure it here: events are drained in batches, so the
    /// interval between ingesting a start and ingesting its result describes
    /// the channel rather than the tool, and reads as `0ms` on a fast burst.
    pub fn finish_tool(&mut self, name: &str, detail: &str, status: ToolStatus, took_ms: u64) {
        let took = took_ms.min(u32::MAX as u64) as u32;
        for entry in self.entries.iter_mut().rev() {
            if let EntryKind::Tool {
                name: entry_name,
                detail: entry_detail,
                status: entry_status,
                elapsed_ms,
                ..
            } = &mut entry.kind
            {
                if *entry_status == ToolStatus::Running && &**entry_name == name {
                    let keep_invocation = status == ToolStatus::Ok && !entry_detail.is_empty();
                    if !keep_invocation {
                        *entry_detail = clamp(detail, MAX_DETAIL);
                    }
                    *entry_status = status;
                    *elapsed_ms = took;
                    return;
                }
            }
        }
        // No matching start (the call was denied before it ran): record it.
        self.push(EntryKind::Tool {
            name: clamp(name, 64),
            detail: clamp(detail, MAX_DETAIL),
            status,
            elapsed_ms: took,
            run: self.run,
        });
    }

    /// Wall-clock `HH:MM:SS` for an entry, in the user's local time.
    pub fn clock(&self, at_ms: u32) -> String {
        let unix = self
            .started_unix
            .saturating_add(at_ms as u64 / 1000)
            .saturating_add_signed(self.utc_offset as i64);
        let secs_of_day = unix % 86_400;
        format!(
            "{:02}:{:02}:{:02}",
            secs_of_day / 3600,
            (secs_of_day % 3600) / 60,
            secs_of_day % 60
        )
    }

    /// Wall-clock for right now, for the answer still being streamed.
    pub fn now_clock(&self) -> String {
        self.clock(self.now_offset())
    }

    /// Total heap held by the entries, for the memory budget test.
    pub fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Entry>()
            + self.entries.iter().map(Entry::heap_bytes).sum::<usize>()
    }
}

/// Columns a tab advances to. Fixed rather than true tab stops, because the
/// text is about to be wrapped to a pane and predictable width matters more
/// than matching an editor.
const TAB_WIDTH: usize = 4;

/// Replace tabs with spaces.
///
/// A tab is one `char`, so ratatui writes it into one cell — but the terminal
/// advances the cursor to the next tab stop. The two models disagree
/// permanently, and the result is a screen where every line after a
/// tab-indented one is overwritten mid-render. Tab-indented source (all Go,
/// most Makefiles) hits this immediately, so tabs never reach the renderer.
pub fn expand_tabs(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\t') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut column = 0usize;
    for c in text.chars() {
        match c {
            '\t' => {
                let pad = TAB_WIDTH - (column % TAB_WIDTH);
                out.extend(std::iter::repeat_n(' ', pad));
                column += pad;
            }
            '\n' => {
                out.push('\n');
                column = 0;
            }
            other => {
                out.push(other);
                column += 1;
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Truncate on a char boundary and hand back an exactly-sized allocation.
///
/// `Box<str>` rather than `String`: the timeline holds thousands of these and
/// a `String`'s spare capacity is pure overhead once the text stops growing.
fn clamp(text: &str, max: usize) -> Box<str> {
    let expanded = expand_tabs(text);
    let trimmed = expanded.trim_end();
    if trimmed.len() <= max {
        return trimmed.into();
    }
    let mut end = max.saturating_sub(1);
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 3);
    out.push_str(&trimmed[..end]);
    out.push('…');
    out.into_boxed_str()
}

/// Render a duration for the timeline's right-hand column.
pub fn render_duration(ms: u32) -> String {
    const MINUTE: u32 = 60_000;
    const HOUR: u32 = 60 * MINUTE;
    if ms == 0 {
        String::new()
    } else if ms < 1000 {
        // Most calls land here; rounding them to `0s` would say nothing.
        format!("{ms}ms")
    } else if ms < MINUTE {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < HOUR {
        format!("{}m{:02}s", ms / MINUTE, (ms % MINUTE) / 1000)
    } else {
        // A `u32` of milliseconds runs out at 49 days, so hours is the last
        // unit worth carrying — and the seconds are noise by then.
        format!("{}h{:02}m", ms / HOUR, (ms % HOUR) / MINUTE)
    }
}

/// The local offset from UTC, in seconds.
///
/// `std` has no timezone database, and pulling in `chrono` or `time` for one
/// number would be a poor trade in a project whose point is a small dependency
/// footprint. `date +%z` is POSIX, costs one process once at startup, and
/// falls back to UTC anywhere it is missing (Windows) — a clock an hour wrong
/// would be worse than one honestly at UTC, so the parse is strict.
pub fn local_utc_offset() -> i32 {
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_utc_offset(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or(0)
}

/// Parse `+0530` / `-0800` into seconds east of UTC.
fn parse_utc_offset(raw: &str) -> Option<i32> {
    let bytes = raw.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i32 = raw.get(1..3)?.parse().ok()?;
    let minutes: i32 = raw.get(3..5)?.parse().ok()?;
    if hours > 14 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline() -> Timeline {
        Timeline::new(0)
    }

    #[test]
    fn entries_land_in_order() {
        let mut t = timeline();
        t.push_user("do the thing");
        t.push_agent("doing it");
        assert_eq!(t.entries().len(), 2);
        assert!(matches!(t.entries()[0].kind, EntryKind::User(_)));
        assert!(matches!(t.entries()[1].kind, EntryKind::Agent(_)));
    }

    #[test]
    fn a_tool_result_settles_its_own_start_rather_than_appending() {
        let mut t = timeline();
        t.start_tool("read", "path=main.go");
        t.finish_tool("read", "package main", ToolStatus::Ok, 12);
        assert_eq!(t.entries().len(), 1, "a call is one row, not two");
        let EntryKind::Tool { status, detail, .. } = &t.entries()[0].kind else {
            panic!("not a tool entry");
        };
        assert_eq!(*status, ToolStatus::Ok);
        // The invocation survives: the row says what was read, not what the
        // file happened to start with.
        assert_eq!(&**detail, "path=main.go");
    }

    #[test]
    fn a_failure_replaces_the_invocation_with_the_error() {
        let mut t = timeline();
        t.start_tool("shell", "asked");
        t.finish_tool(
            "shell",
            "sh: asked: command not found",
            ToolStatus::Failed,
            72,
        );
        let EntryKind::Tool { detail, .. } = &t.entries()[0].kind else {
            panic!("not a tool entry");
        };
        assert_eq!(&**detail, "sh: asked: command not found");
    }

    #[test]
    fn a_call_with_no_invocation_falls_back_to_its_result() {
        // Not every tool reports arguments worth showing; the row must not be
        // left blank.
        let mut t = timeline();
        t.start_tool("read", "");
        t.finish_tool("read", "package main", ToolStatus::Ok, 12);
        let EntryKind::Tool { detail, .. } = &t.entries()[0].kind else {
            panic!("not a tool entry");
        };
        assert_eq!(&**detail, "package main");
    }

    #[test]
    fn concurrent_calls_settle_the_right_row() {
        let mut t = timeline();
        t.start_tool("read", "a");
        t.start_tool("shell", "b");
        t.finish_tool("read", "done a", ToolStatus::Ok, 12);
        let statuses: Vec<ToolStatus> = t
            .entries()
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Tool { status, .. } => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(statuses, vec![ToolStatus::Ok, ToolStatus::Running]);
    }

    #[test]
    fn a_tool_call_reports_how_long_it_took() {
        // Regression: `at` was in seconds, so every call under a second
        // measured as 0ms and the duration column rendered empty.
        let mut t = timeline();
        t.start_tool("shell", "go build ./...");
        std::thread::sleep(std::time::Duration::from_millis(12));
        t.finish_tool("shell", "ok", ToolStatus::Ok, 12);
        let EntryKind::Tool { elapsed_ms, .. } = &t.entries()[0].kind else {
            panic!("not a tool entry");
        };
        assert!(*elapsed_ms >= 10, "measured {elapsed_ms}ms");
        assert!(*elapsed_ms < 5_000, "measured {elapsed_ms}ms");
        assert!(!render_duration(*elapsed_ms).is_empty());
    }

    #[test]
    fn a_result_without_a_start_still_appears() {
        // A tool denied by the mode gate never starts.
        let mut t = timeline();
        t.finish_tool("apply_patch", "denied", ToolStatus::Denied, 12);
        assert_eq!(t.entries().len(), 1);
    }

    // ---- memory -----------------------------------------------------------

    #[test]
    fn the_entry_list_is_bounded_and_drops_the_oldest() {
        let mut t = timeline();
        for i in 0..(MAX_ENTRIES + 100) {
            t.push_agent(&format!("line {i}"));
        }
        assert_eq!(t.entries().len(), MAX_ENTRIES);
        let EntryKind::Agent(first) = &t.entries()[0].kind else {
            panic!()
        };
        assert_eq!(&**first, "line 100");
    }

    #[test]
    fn one_enormous_tool_line_cannot_be_retained_whole() {
        // Regression: only the line *count* was bounded, so a tool returning a
        // single 100 KB line kept all of it.
        let mut t = timeline();
        let huge = "x".repeat(100_000);
        t.finish_tool("shell", &huge, ToolStatus::Ok, 12);
        assert!(t.heap_bytes() < 1_000, "retained {} bytes", t.heap_bytes());
    }

    #[test]
    fn an_enormous_message_is_clamped_too() {
        let mut t = timeline();
        t.push_agent(&"y".repeat(1_000_000));
        assert!(t.heap_bytes() < MAX_TEXT + 1_000);
    }

    #[test]
    fn a_full_timeline_stays_within_its_memory_budget() {
        // The worst case a bounded timeline can reach: every entry full.
        let mut t = timeline();
        let text = "z".repeat(MAX_TEXT * 2);
        let detail = "d".repeat(MAX_DETAIL * 2);
        for i in 0..MAX_ENTRIES {
            if i % 2 == 0 {
                t.push_agent(&text);
            } else {
                t.finish_tool("shell", &detail, ToolStatus::Ok, 12);
            }
        }
        let bytes = t.heap_bytes();
        // 200 messages at 4 KB plus 200 tool rows at 200 B is ~850 KB; the
        // budget leaves room for the Vec itself and nothing more.
        assert!(
            bytes < 1_000_000,
            "a saturated timeline holds {bytes} bytes"
        );
    }

    #[test]
    fn clearing_gives_the_memory_back() {
        let mut t = timeline();
        for _ in 0..MAX_ENTRIES {
            t.push_agent(&"z".repeat(MAX_TEXT));
        }
        assert!(t.heap_bytes() > 100_000);
        t.clear();
        assert!(t.heap_bytes() < 1_000, "held {} bytes", t.heap_bytes());
    }

    #[test]
    fn stored_text_has_no_spare_capacity() {
        // `Box<str>` is the point: a String would carry growth slack.
        assert_eq!(std::mem::size_of::<Box<str>>(), 16);
        assert_eq!(std::mem::size_of::<String>(), 24);
        assert_eq!(clamp("hello", 100).len(), 5);
    }

    // ---- formatting -------------------------------------------------------

    #[test]
    fn clamping_respects_char_boundaries() {
        // Slicing a multi-byte character in half panics.
        let text = "héllo wörld — a long line with é and ö and …";
        for max in 1..text.len() {
            let out = clamp(text, max);
            assert!(out.len() <= max + 4, "max {max}: {} bytes", out.len());
        }
    }

    #[test]
    fn tabs_never_reach_the_renderer() {
        // Regression: ratatui puts a tab in one cell, the terminal advances to
        // the next tab stop, and every following line is drawn over. Go source
        // is tab-indented, so this corrupted the screen on the first file read.
        let mut t = timeline();
        t.push_agent("func main() {\n\tfmt.Println(\"hi\")\n}");
        let EntryKind::Agent(text) = &t.entries()[0].kind else {
            panic!()
        };
        assert!(!text.contains('\t'), "{text:?}");
        assert!(text.contains("    fmt.Println"), "{text:?}");
    }

    #[test]
    fn tab_expansion_aligns_to_stops_and_resets_per_line() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(expand_tabs("ab\tx"), "ab  x");
        assert_eq!(expand_tabs("abc\tx"), "abc x");
        assert_eq!(expand_tabs("abcd\tx"), "abcd    x");
        // A newline restarts the column count.
        assert_eq!(expand_tabs("abcd\n\tx"), "abcd\n    x");
        // Text without tabs is not copied at all.
        assert!(matches!(
            expand_tabs("no tabs here"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn clamping_marks_what_it_cut() {
        assert_eq!(&*clamp("abcdefghij", 5), "abcd…");
        assert_eq!(&*clamp("abc", 5), "abc");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(render_duration(0), "");
        assert_eq!(render_duration(12), "12ms");
        assert_eq!(render_duration(1_200), "1.2s");
        assert_eq!(render_duration(65_000), "1m05s");
        assert_eq!(render_duration(59_999), "60.0s");
        assert_eq!(render_duration(2 * 60_000), "2m00s");
        assert_eq!(render_duration(3_600_000), "1h00m");
        assert_eq!(render_duration(3_600_000 + 25 * 60_000), "1h25m");
        assert_eq!(render_duration(u32::MAX), "1193h02m");
    }

    /// Every duration carries a unit, and the unit is one of the four we
    /// document. A row reading a bare number would be ambiguous.
    #[test]
    fn every_duration_names_its_unit() {
        for ms in [
            1u32,
            999,
            1_000,
            59_999,
            60_000,
            3_599_999,
            3_600_000,
            u32::MAX,
        ] {
            let rendered = render_duration(ms);
            assert!(
                rendered.ends_with("ms") || rendered.ends_with('s') || rendered.ends_with('m'),
                "{ms} rendered as {rendered:?}"
            );
        }
    }

    #[test]
    fn the_clock_is_wall_time_in_the_local_zone() {
        let mut t = Timeline::new(0);
        t.started_unix = 1_700_000_000; // 2023-11-14T22:13:20Z
        assert_eq!(t.clock(0), "22:13:20");
        assert_eq!(t.clock(100_000), "22:15:00");

        // The same instant, five and a half hours east.
        let mut t = Timeline::new(5 * 3600 + 1800);
        t.started_unix = 1_700_000_000;
        assert_eq!(t.clock(0), "03:43:20");
    }

    #[test]
    fn the_live_clock_tracks_the_entry_clock() {
        let t = timeline();
        // Nothing has happened yet, so "now" is the start.
        assert_eq!(t.now_clock(), t.clock(0));
    }

    #[test]
    fn the_clock_wraps_across_midnight() {
        let mut t = Timeline::new(0);
        t.started_unix = 1_699_999_999 - (1_699_999_999 % 86_400) + 86_399;
        assert_eq!(t.clock(0), "23:59:59");
        assert_eq!(t.clock(1_000), "00:00:00");
    }

    #[test]
    fn utc_offsets_parse_in_both_directions() {
        assert_eq!(parse_utc_offset("+0000"), Some(0));
        assert_eq!(parse_utc_offset("+0700"), Some(7 * 3600));
        assert_eq!(parse_utc_offset("-0800"), Some(-8 * 3600));
        assert_eq!(parse_utc_offset("+0530"), Some(5 * 3600 + 1800));
        assert_eq!(parse_utc_offset("-0330"), Some(-(3 * 3600 + 1800)));
    }

    #[test]
    fn a_nonsense_offset_falls_back_to_utc_rather_than_lying() {
        for bad in ["", "abc", "+99", "0700", "+2500", "+0099", "++0700"] {
            assert_eq!(parse_utc_offset(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_real_offset_is_plausible() {
        // Whatever this machine says, it has to be a real zone.
        let offset = local_utc_offset();
        assert!((-14 * 3600..=14 * 3600).contains(&offset), "{offset}");
        assert_eq!(offset % 900, 0, "zones are quarter-hour multiples");
    }
}
