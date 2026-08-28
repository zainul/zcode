//! Interactive terminal UI (FR-IFACE-02/04/05, DQ8).
//!
//! The engine loop is synchronous and blocking (DQ4), so it runs on a
//! dedicated `std::thread` and streams `EngineMsg`s back over a single `mpsc`
//! channel. The main thread does nothing but render, which keeps the UI
//! responsive while a turn is in flight.
//!
//! One channel, not two: events and the turn's result used to arrive on
//! separate channels, so a `try_recv` on the result could win a race against
//! trailing deltas and commit an answer before its own last words had been
//! folded in.
//!
//! The timeline is bounded and every stored string is capped at ingest, so a
//! runaway tool output cannot grow the process without limit — see
//! `timeline`'s module docs for the accounting (NFR-PERF-03).

mod command;
mod input;
mod timeline;
mod wrap;

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use super::clipboard;
use app::{AgentLoop, ExecutionRequest, ExecutionResult};
use domain::{AgentMode, CancelFlag, Cost, UiEvent};
use infra_config::Config;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Terminal, TerminalOptions};

use self::command::SlashCommand;
use self::input::Input;
use self::timeline::{EntryKind, NoteLevel, Timeline, ToolStatus};
use super::emit::sanitize;
use super::logging::LogRedirect;
use super::wire;

/// Frame budget. Short enough for a smooth spinner, long enough to idle cheap.
const TICK: Duration = Duration::from_millis(80);
/// Spinner frames, one per `SPINNER_PERIOD`.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_PERIOD: Duration = Duration::from_millis(90);
/// A drag-selection over the conversation, in screen cells.
///
/// Held in screen coordinates rather than timeline positions because that is
/// what the user is pointing at: they select what they can see, and what they
/// can see is the wrapped, clipped, right-aligned result of a render. Mapping
/// back to entries would mean copying text that is not on the screen.
///
/// The consequence is that a selection does not survive scrolling or new
/// output, so both clear it — a highlight left behind after the rows beneath
/// it moved would be pointing at the wrong words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the drag started, (column, row).
    pub anchor: (u16, u16),
    /// Where the pointer is now.
    pub head: (u16, u16),
    /// True while the button is still down.
    pub dragging: bool,
}

impl Selection {
    fn new(at: (u16, u16)) -> Self {
        Self {
            anchor: at,
            head: at,
            dragging: true,
        }
    }

    /// The selection as (first, last) in reading order, so the caller does not
    /// have to care which way the drag went.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let (a, b) = (self.anchor, self.head);
        // Compare by row first: a drag up-and-left and one down-and-right are
        // the same span read forwards.
        if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// Whether anything is actually covered. A click without a drag selects
    /// nothing, and must not clear the clipboard.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Rows one wheel notch moves the conversation. Terminals themselves scroll
/// three, so matching it is what makes the pane feel native.
const WHEEL_ROWS: u16 = 3;
/// Rows PageUp/PageDown move the conversation.
const PAGE_ROWS: u16 = 10;

/// Rows the prompt may grow to before it scrolls internally.
const MAX_INPUT_ROWS: u16 = 10;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What the UI is doing right now. Drives the spinner, the status colour, and
/// which keys do what.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Working {
        since: Instant,
        step: u64,
        max: u64,
    },
    /// Provider asked us to back off; we are sleeping, not stuck.
    /// Provider asked us to back off; we are sleeping, not stuck. Carries the
    /// step it interrupted so the bar can go back to reporting progress the
    /// moment output resumes.
    RateLimited {
        since: Instant,
        step: u64,
        max: u64,
        detail: String,
    },
    Cancelling,
    Failed(String),
}

impl Phase {
    fn is_busy(&self) -> bool {
        matches!(
            self,
            Phase::Working { .. } | Phase::RateLimited { .. } | Phase::Cancelling
        )
    }
}

/// Running totals for the whole TUI session, across turns.
#[derive(Debug, Default, Clone)]
pub struct Totals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
    pub steps: u64,
    pub turns: u64,
    pub cost: Cost,
    turn: TurnUsage,
}

/// What the turn in flight has reported so far.
///
/// Kept so each report can add only the *difference*: the engine sends running
/// totals for the current turn, so adding them whole would count step 1 seven
/// times over by step 7.
#[derive(Debug, Default, Clone, Copy)]
struct TurnUsage {
    input: u64,
    output: u64,
    cache: u64,
    cost_usd: f64,
}

impl Totals {
    /// Fold in usage reported mid-turn, so the display moves while the money
    /// is being spent rather than only once the turn settles.
    fn set_turn_usage(&mut self, usage: &domain::LlmFinish) {
        self.input_tokens += usage.input_tokens.saturating_sub(self.turn.input);
        self.output_tokens += usage.output_tokens.saturating_sub(self.turn.output);
        self.cache_tokens += usage.cache_tokens.saturating_sub(self.turn.cache);
        self.turn.input = usage.input_tokens;
        self.turn.output = usage.output_tokens;
        self.turn.cache = usage.cache_tokens;

        if let Some(reported) = usage.cost_usd {
            let delta = reported - self.turn.cost_usd;
            if delta > 0.0 {
                self.cost.add(Cost::from_reported_usd(delta));
            }
            self.turn.cost_usd = reported;
        }
    }

    fn record(&mut self, result: &ExecutionResult) {
        // Reconcile against what the turn already reported rather than adding
        // it again — the mid-turn reports and the final result describe the
        // same tokens.
        self.input_tokens += result.input_tokens.saturating_sub(self.turn.input);
        self.output_tokens += result.output_tokens.saturating_sub(self.turn.output);
        self.cache_tokens += result.cache_tokens.saturating_sub(self.turn.cache);
        self.steps += result.steps;
        self.turns += 1;
        // A cost the provider reported has already been counted and is exact;
        // the local estimate must not be added on top of it.
        if self.turn.cost_usd <= 0.0 {
            self.cost.add(result.cost);
        }
        self.turn = TurnUsage::default();
    }
}

/// What the renderer knows. Deliberately plain strings: markdown rendering is
/// out of scope for v0.2 (PRD §6 #6).
pub struct TuiState {
    /// The single ordered log of the conversation and the tools that served
    /// it. Replaces the old transcript/tools pane pair.
    pub timeline: Timeline,
    pub input: Input,
    /// Text streamed for the in-flight answer, not yet committed.
    pub streaming: String,
    pub phase: Phase,
    pub mode: AgentMode,
    pub provider: String,
    pub model: String,
    pub session_id: Option<String>,
    pub session_dir: String,
    pub totals: Totals,
    pub tool_names: Vec<String>,
    /// Names from the config's `providers` array, for `/provider`.
    pub providers: Vec<String>,
    /// Runs the user has explicitly folded or unfolded, by run id.
    ///
    /// An override, not the state: a run is expanded by default while a call
    /// in it is still running, and folded once it settles, so the work in
    /// flight is always visible without anyone asking. This map records only
    /// the runs someone has since disagreed with.
    pub run_override: std::collections::HashMap<u32, bool>,
    /// Screen rows carrying a run header, and the run each one folds.
    ///
    /// Recorded during the draw, because only the draw knows which rows are
    /// on screen after wrapping and scrolling — the same reason `max_scroll`
    /// lives there.
    pub header_rows: Vec<(u16, u32)>,
    /// Rows scrolled back from the tail; 0 means "following the tail".
    pub scrollback: u16,
    /// The drag in progress, or the one just finished.
    pub selection: Option<Selection>,
    /// The text under `selection`, lifted from the frame that drew it.
    ///
    /// Extracted during the draw because that is the only place the rendered
    /// cells exist; bounded by the pane, so a screenful, not a session.
    pub selected_text: String,
    /// The conversation pane's area on the last frame, to tell a drag over the
    /// transcript from one over the prompt.
    pub pane: Rect,
    /// The largest `scrollback` the last frame could actually honour.
    ///
    /// Scrolling has to be clamped against the *rendered* height, which only
    /// the draw knows — and letting `scrollback` run past it is not harmless:
    /// the view stops at the top while the counter keeps climbing, so the
    /// same number of PageDowns does nothing on the way back. That reads
    /// exactly like a pane that will not scroll.
    pub max_scroll: u16,
    /// Prompts sent this session, newest last, for `Up`/`Down` recall.
    pub history: Vec<String>,
    /// Index into `history`; equal to `history.len()` while editing the live
    /// prompt, and pointing at a recalled entry otherwise.
    pub history_cursor: usize,
    /// The in-progress draft, stashed when history recall begins so it can be
    /// restored when the user returns to the bottom with `Down`.
    pub history_draft: String,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            timeline: Timeline::default(),
            input: Input::default(),
            streaming: String::new(),
            phase: Phase::Idle,
            mode: AgentMode::default(),
            provider: String::new(),
            model: String::new(),
            session_id: None,
            session_dir: String::new(),
            totals: Totals::default(),
            tool_names: Vec::new(),
            providers: Vec::new(),
            run_override: std::collections::HashMap::new(),
            header_rows: Vec::new(),
            selection: None,
            selected_text: String::new(),
            pane: Rect::new(0, 0, 0, 0),
            scrollback: 0,
            max_scroll: 0,
            history: Vec::new(),
            history_cursor: 0,
            history_draft: String::new(),
        }
    }
}

impl TuiState {
    /// The run whose header occupies screen row `row`, if any.
    pub fn header_at(&self, row: u16) -> Option<u32> {
        self.header_rows
            .iter()
            .find(|(at, _)| *at == row)
            .map(|(_, run)| *run)
    }

    /// Whether a run of tool calls is shown open.
    ///
    /// The default follows the work — open while a call is running, folded
    /// once they have all settled — and an explicit toggle overrides it.
    pub fn run_is_expanded(&self, run: u32, summary: RunSummary) -> bool {
        self.run_override
            .get(&run)
            .copied()
            .unwrap_or_else(|| summary.expanded_by_default())
    }

    /// Fold or unfold one run.
    pub fn toggle_run(&mut self, run: u32) {
        let entries = self.timeline.entries();
        let start = entries
            .iter()
            .position(|e| entry_run(e) == Some(run))
            .unwrap_or(0);
        let now = self.run_is_expanded(run, run_summary(entries, start));
        self.run_override.insert(run, !now);
        // The rows underneath a fold have just moved.
        self.clear_selection();
    }

    /// Whether every run on screen is currently folded, so `Ctrl-T` knows
    /// which way to go.
    fn every_run_folded(&self) -> bool {
        let entries = self.timeline.entries();
        let mut seen = false;
        for (i, entry) in entries.iter().enumerate() {
            let Some(run) = entry_run(entry) else {
                continue;
            };
            if i > 0 && entries[i - 1].is_tool() {
                continue;
            }
            seen = true;
            if self.run_is_expanded(run, run_summary(entries, i)) {
                return false;
            }
        }
        seen
    }

    /// Fold or unfold every run at once.
    fn set_all_runs(&mut self, expanded: bool) {
        let runs: Vec<u32> = self
            .timeline
            .entries()
            .iter()
            .filter_map(entry_run)
            .collect();
        for run in runs {
            self.run_override.insert(run, expanded);
        }
        self.clear_selection();
    }

    /// Where to draw the caret when the pointer is in the conversation.
    ///
    /// `None` means the prompt keeps it, which is every case except an active
    /// or just-finished selection.
    pub fn caret_in_conversation(&self) -> Option<(u16, u16)> {
        let sel = self.selection?;
        // The head, not the anchor: it is the end you are still moving.
        Some(sel.head)
    }

    /// Begin a drag, if it started inside the conversation.
    fn start_selection(&mut self, col: u16, row: u16) {
        if inside(self.pane, col, row) {
            self.selection = Some(Selection::new((col, row)));
            self.selected_text.clear();
        }
    }

    /// Extend the drag. The pointer is clamped into the pane rather than
    /// ignored when it leaves, so dragging off the edge selects to the edge
    /// instead of freezing halfway.
    fn extend_selection(&mut self, col: u16, row: u16) {
        let pane = self.pane;
        if let Some(sel) = self.selection.as_mut() {
            if sel.dragging {
                sel.head = clamp_into(pane, col, row);
            }
        }
    }

    /// Finish the drag and put what it covered on the clipboard.
    ///
    /// Copying on release is the behaviour of a terminal's own selection, and
    /// the alternative — select now, press something later — means the
    /// highlight has to survive output arriving underneath it, which it
    /// cannot.
    fn finish_selection(&mut self, out: &mut impl std::io::Write) {
        let Some(sel) = self.selection.as_mut() else {
            return;
        };
        sel.dragging = false;
        if sel.is_empty() {
            // A click that did not move is a click, not a selection. On a run
            // header it folds; anywhere else it dismisses.
            let (_, row) = sel.anchor;
            self.selection = None;
            if let Some(run) = self.header_at(row) {
                self.toggle_run(run);
            }
            return;
        }
        let text = std::mem::take(&mut self.selected_text);
        if text.trim().is_empty() {
            // A drag across blank space selected nothing. Saying so would be
            // a warning about a non-event.
            self.selection = None;
            return;
        }
        let lines = text.lines().count().max(1);
        match clipboard::copy(&text, out) {
            Ok(how) => self.push_note(
                &format!("{lines} line(s) {}", how.describe()),
                NoteLevel::Info,
            ),
            Err(why) => self.push_note(&format!("could not copy: {why}"), NoteLevel::Warn),
        }
        self.selected_text = text;
    }

    /// Drop the highlight. Anything that moves the rows underneath it has to
    /// call this, or it would be pointing at different words than it did.
    fn clear_selection(&mut self) {
        self.selection = None;
        self.selected_text.clear();
    }

    /// Scroll back by `rows`, stopping at the oldest line the last frame drew.
    ///
    /// Clamping here rather than only in the renderer is what makes the pane
    /// feel connected to the input: an unclamped counter keeps rising after
    /// the view has stopped, and then swallows the first N scrolls back down.
    pub fn scroll_up(&mut self, rows: u16) {
        self.scrollback = self.scrollback.saturating_add(rows).min(self.max_scroll);
        self.clear_selection();
    }

    pub fn scroll_down(&mut self, rows: u16) {
        self.scrollback = self.scrollback.saturating_sub(rows);
        self.clear_selection();
    }

    /// Engine or UI commentary, shown as a note row.
    fn push_note(&mut self, text: &str, level: NoteLevel) {
        self.timeline.push_note(text, level);
    }

    /// Multi-line UI output (`/help`, `/mode`) as one agent-style entry.
    fn push_lines(&mut self, lines: Vec<String>) {
        self.timeline.push_agent(&lines.join("\n"));
    }

    pub fn busy(&self) -> bool {
        self.phase.is_busy()
    }

    /// Fold one engine event into the view.
    pub fn apply(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Delta(text) => {
                // Output means the backoff is over. Without this the bar keeps
                // saying "retrying in 1.0s" for the whole answer that followed.
                self.resume_from_backoff();
                // Tabs are expanded here as well as at ingest: the streaming
                // buffer is rendered directly, before it ever reaches the
                // timeline.
                self.streaming
                    .push_str(&timeline::expand_tabs(&sanitize(&text)));
            }
            UiEvent::ToolCallStart { name, .. } => {
                self.resume_from_backoff();
                // Streamed prose is committed first so the tool row lands
                // *under* the sentence that announced it, which is the whole
                // point of an inline timeline.
                self.commit_streaming();
                self.timeline.start_tool(&name, "");
            }
            UiEvent::ToolCallArgs { arguments, .. } => {
                // Arguments arrive in fragments; the first is enough to say
                // what the call is about, and keeping only that bounds it.
                self.annotate_running_tool(&arguments);
            }
            UiEvent::ToolResult {
                name,
                content,
                error,
                elapsed_ms,
                ..
            } => {
                let (detail, status) = match error {
                    Some(message) => (sanitize(&message), ToolStatus::Failed),
                    None => (first_line(&sanitize(&content)), ToolStatus::Ok),
                };
                // The engine timed the call; the gap between the two events
                // reaching this thread is a fact about the channel, not the
                // tool, and on a fast burst it reads as 0ms.
                self.timeline
                    .finish_tool(&name, &detail, status, elapsed_ms);
            }
            UiEvent::Retry(notice) => {
                // The wait already happened inside the client, but the user is
                // still staring at the screen: say why nothing is moving.
                let detail = notice.render();
                self.push_note(&detail, NoteLevel::Retry);
                let (since, step, max) = match &self.phase {
                    Phase::Working { since, step, max } => (*since, *step, *max),
                    Phase::RateLimited {
                        since, step, max, ..
                    } => (*since, *step, *max),
                    _ => (Instant::now(), 0, 0),
                };
                self.phase = Phase::RateLimited {
                    since,
                    step,
                    max,
                    detail,
                };
            }
            UiEvent::Notice(message) => self.push_note(&sanitize(&message), NoteLevel::Info),
            UiEvent::LoopStart { step, max_turns } => {
                self.phase = Phase::Working {
                    since: match &self.phase {
                        // Keep the elapsed clock running across steps of one turn.
                        Phase::Working { since, .. } => *since,
                        _ => Instant::now(),
                    },
                    step,
                    max: max_turns,
                };
            }
            UiEvent::Error(message) => {
                let message = sanitize(&message);
                // A mode refusal reads better as a settled tool row than as a
                // free-floating warning.
                match denied_tool(&message) {
                    Some(name) => {
                        // The row is already labelled with the tool, so keep
                        // only the reason: "apply_patch  planning mode is
                        // read-only" reads better than repeating the name.
                        let reason = message
                            .split_once(" denied: ")
                            .map(|(_, why)| why.to_string())
                            .unwrap_or_else(|| message.clone());
                        let name = name.to_string();
                        // Refused before dispatch, so there is nothing to time.
                        self.timeline
                            .finish_tool(&name, &reason, ToolStatus::Denied, 0);
                    }
                    None => self.push_note(&message, NoteLevel::Warn),
                }
            }
            UiEvent::LoopEnd { truncated, .. } => {
                if truncated {
                    self.push_note("stopped at the turn/token cap", NoteLevel::Warn);
                }
            }
            UiEvent::Finish(_) => {}
            // Usage so far in the turn in flight. Replaces rather than adds:
            // the engine sends running totals for this turn, so accumulating
            // them here would count step 1 seven times by step 7.
            UiEvent::Usage(usage) => self.totals.set_turn_usage(&usage),
        }
    }

    /// Leave the rate-limited state once the provider starts answering again,
    /// keeping the turn's elapsed clock running.
    fn resume_from_backoff(&mut self) {
        if let Phase::RateLimited {
            since, step, max, ..
        } = &self.phase
        {
            self.phase = Phase::Working {
                since: *since,
                step: *step,
                max: *max,
            };
        }
    }

    /// Move the in-flight streamed text into the timeline as an entry.
    fn commit_streaming(&mut self) {
        if self.streaming.trim().is_empty() {
            self.streaming.clear();
            return;
        }
        self.timeline.push_agent(&self.streaming);
        // `clear` keeps the buffer's capacity, which is what we want between
        // fragments of one turn — but a single huge answer would otherwise
        // hold that peak for the rest of the session.
        self.streaming.clear();
        if self.streaming.capacity() > 8 * 1024 {
            self.streaming.shrink_to(4 * 1024);
        }
    }

    /// Attach the first fragment of a call's arguments to its running row.
    /// Record what a running call was asked to do, from its argument stream.
    ///
    /// This is what the row shows once the call succeeds, so it is worth
    /// getting right: the *first* fragment usually carries the command or the
    /// path, and later fragments are the rest of a JSON object nobody wants
    /// to read.
    /// Record what a running call was asked to do, from its argument stream.
    ///
    /// This is what the row shows once the call succeeds, so it is worth
    /// getting right: the first fragment usually carries the command or the
    /// path, and later fragments are the rest of a JSON object nobody wants
    /// to read.
    fn annotate_running_tool(&mut self, arguments: &str) {
        let summary = summarize_arguments(arguments);
        if summary.is_empty() {
            return;
        }
        for entry in self.timeline.entries_mut().iter_mut().rev() {
            if let EntryKind::Tool {
                detail,
                status: ToolStatus::Running,
                ..
            } = &mut entry.kind
            {
                if detail.is_empty() {
                    *detail = summary.into_boxed_str();
                }
                return;
            }
        }
    }

    /// Commit the streamed answer to the timeline at the end of a turn.
    pub fn finish_turn(&mut self, outcome: Result<ExecutionResult, String>) {
        let had_text = !self.streaming.trim().is_empty();
        self.commit_streaming();
        match outcome {
            Ok(result) => {
                if !had_text && !result.final_text.trim().is_empty() {
                    self.timeline.push_agent(result.final_text.trim_end());
                }
                self.session_id = Some(result.session_id.clone());
                self.totals.record(&result);
                self.phase = Phase::Idle;
            }
            Err(message) => {
                // A provider failure is the thing the user most needs to see,
                // so it goes in the timeline *and* colours the status bar
                // until the next turn starts.
                let message = sanitize(&message);
                self.timeline.push_note(&message, NoteLevel::Warn);
                self.phase = Phase::Failed(first_line(&message));
            }
        }
        self.scrollback = 0;
        // The rows the highlight covered have just moved.
        self.clear_selection();
    }

    /// The status line: what the agent is doing, and what it has cost.
    ///
    /// Assembled to fit `width`. A long model id (`openrouter/poolside/
    /// laguna-s-2.1:free` is 39 characters) would otherwise push the cost —
    /// the thing the user asked to see — off the right edge of an 80- or
    /// 100-column terminal. Detail is shed from the least important end until
    /// it fits; state, mode, and cost always survive.
    pub fn status_spans(&self, frame: usize, width: usize) -> Vec<Span<'static>> {
        let (symbol, text, style) = self.phase_display(frame);
        let cost = self.totals.cost.render();

        // Richest first; the first rendering that fits is the one drawn.
        for detail in [
            Detail::Full,
            Detail::ShortModel,
            Detail::ModelOnly,
            Detail::NoCache,
            Detail::NoTokens,
            Detail::Minimal,
        ] {
            let spans = self.assemble(&symbol, &text, style, &cost, detail);
            if span_width(&spans) <= width {
                return spans;
            }
        }
        // Narrower than even the minimal form: keep the state symbol and the
        // cost, and let the terminal clip the rest.
        self.assemble(&symbol, &text, style, &cost, Detail::Minimal)
    }

    /// The symbol, wording, and colour for the current phase.
    fn phase_display(&self, frame: usize) -> (String, String, Style) {
        match &self.phase {
            Phase::Idle => (
                "●".to_string(),
                "ready".to_string(),
                Style::default().fg(Color::Green),
            ),
            Phase::Working { since, step, max } => (
                SPINNER[frame % SPINNER.len()].to_string(),
                format!(
                    "working · step {step}/{max} · {}",
                    render_elapsed(since.elapsed())
                ),
                Style::default().fg(Color::Cyan),
            ),
            Phase::RateLimited { since, detail, .. } => (
                SPINNER[frame % SPINNER.len()].to_string(),
                format!("{detail} · {}", render_elapsed(since.elapsed())),
                Style::default().fg(Color::Yellow),
            ),
            Phase::Cancelling => (
                "◌".to_string(),
                "cancelling…".to_string(),
                Style::default().fg(Color::Yellow),
            ),
            Phase::Failed(message) => (
                "✖".to_string(),
                format!("error: {message}"),
                Style::default().fg(Color::Red),
            ),
        }
    }

    fn assemble(
        &self,
        symbol: &str,
        text: &str,
        style: Style,
        cost: &str,
        detail: Detail,
    ) -> Vec<Span<'static>> {
        let mut spans = vec![
            Span::styled(format!(" {symbol} "), style),
            Span::styled(text.to_string(), style),
            Span::raw(SEP),
            Span::styled(
                format!("mode {}", self.mode.as_str()),
                Style::default()
                    .fg(mode_colour(self.mode))
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        if let Some(model) = self.model_label(detail) {
            spans.push(Span::raw(SEP));
            spans.push(Span::raw(model));
        }

        if detail.shows_tokens() {
            spans.push(Span::raw(SEP));
            let mut tokens = format!(
                "{} in / {} out",
                self.totals.input_tokens, self.totals.output_tokens
            );
            if detail.shows_cache() && self.totals.cache_tokens > 0 {
                tokens.push_str(&format!(" / {} cached", self.totals.cache_tokens));
            }
            spans.push(Span::raw(tokens));
        }

        // The cost is never dropped: showing it is the point.
        spans.push(Span::raw(SEP));
        spans.push(Span::styled(
            cost.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ));

        if self.scrollback > 0 && detail.shows_tokens() {
            spans.push(Span::styled(
                format!("{SEP}scrolled ↑{}", self.scrollback),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans
    }

    /// How to render `provider/model` at this level of detail.
    fn model_label(&self, detail: Detail) -> Option<String> {
        let short = || {
            // `openrouter/poolside/laguna-s-2.1:free` → `laguna-s-2.1:free`
            self.model
                .rsplit('/')
                .next()
                .unwrap_or(&self.model)
                .to_string()
        };
        match detail {
            Detail::Full => Some(format!("{}/{}", self.provider, self.model)),
            Detail::ShortModel => Some(format!("{}/{}", self.provider, short())),
            Detail::ModelOnly | Detail::NoCache | Detail::NoTokens => Some(short()),
            Detail::Minimal => None,
        }
    }

    /// The cost breakdown shown by `/cost`.
    pub fn cost_lines(&self) -> Vec<String> {
        let c = &self.totals.cost;
        let mut out = vec![
            format!(
                "session: {} turn(s), {} step(s)",
                self.totals.turns, self.totals.steps
            ),
            format!(
                "tokens:  {} in, {} out, {} cached",
                self.totals.input_tokens, self.totals.output_tokens, self.totals.cache_tokens
            ),
        ];
        if c.priced {
            out.push(format!(
                "cost:    {} (input {:.4}, output {:.4}, cache {:.4}) — estimated from list \
                 prices, not a bill",
                c.render(),
                c.input_usd,
                c.output_usd,
                c.cache_usd
            ));
        } else {
            out.push(format!(
                "cost:    n/a — no rate known for `{}`; add a [[pricing]] entry to price it",
                self.model
            ));
        }
        out
    }
}

/// Separator between status-bar fields.
const SEP: &str = "  │  ";

/// How much of the status bar to render. Ordered richest to sparsest.
#[derive(Clone, Copy, PartialEq)]
enum Detail {
    Full,
    ShortModel,
    ModelOnly,
    NoCache,
    NoTokens,
    Minimal,
}

impl Detail {
    fn shows_tokens(self) -> bool {
        !matches!(self, Detail::NoTokens | Detail::Minimal)
    }

    fn shows_cache(self) -> bool {
        matches!(self, Detail::Full | Detail::ShortModel | Detail::ModelOnly)
    }
}

/// Printable width of a rendered line, in characters.
fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

fn mode_colour(mode: AgentMode) -> Color {
    match mode {
        AgentMode::Planning => Color::Blue,
        AgentMode::Editing => Color::Magenta,
        AgentMode::Auto => Color::Yellow,
    }
}

/// The tool named in a mode-refusal message, if it is one.
///
/// Matching the engine's own wording (``tool `x` denied: …``) rather than a
/// shared constant, because the message crosses a port boundary as prose.
fn denied_tool(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("tool `")?;
    let (name, tail) = rest.split_once('`')?;
    tail.starts_with(" denied").then_some(name)
}

/// One line describing what a tool call is about, from its JSON arguments.
///
/// The raw arguments can be a whole file; the timeline shows the values that
/// identify the call — a path, a command — and nothing else.
fn summarize_arguments(arguments: &str) -> String {
    const KEYS: &[&str] = &["path", "command", "file_path", "name", "query", "pattern"];
    let trimmed = arguments.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    for key in KEYS {
        // A hand-rolled scan rather than a JSON parse: arguments arrive in
        // fragments, so the text is usually not valid JSON yet.
        let needle = format!("\"{key}\":");
        if let Some(start) = trimmed.find(&needle) {
            let after = trimmed[start + needle.len()..].trim_start();
            if let Some(value) = after.strip_prefix('"') {
                if let Some(end) = value.find('"') {
                    return value[..end].to_string();
                }
            }
        }
    }
    // No recognised key: show the raw fragment, bounded.
    trimmed.trim_matches(|c| c == '{' || c == '}').to_string()
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_string()
}

/// Hand `text` to the clipboard and say what happened.
///
/// Reports rather than assumes: the previous version wrote an OSC 52 escape
/// and announced success, which on a terminal that ignores the sequence —
/// macOS Terminal.app among them — meant "copied" and an unchanged clipboard.
fn copy_and_report(state: &mut TuiState, text: &str, what: &str) {
    match clipboard::copy(text, &mut io::stdout()) {
        Ok(how) => state.push_note(&format!("{what} {}", how.describe()), NoteLevel::Info),
        Err(why) => state.push_note(&format!("could not copy {what}: {why}"), NoteLevel::Warn),
    }
}

/// The most recent assistant message, or the in-flight stream if one is showing.
fn last_agent_text(state: &TuiState) -> Option<String> {
    if !state.streaming.trim().is_empty() {
        return Some(state.streaming.clone());
    }
    for entry in state.timeline.entries().iter().rev() {
        if let EntryKind::Agent(text) = &entry.kind {
            return Some(text.to_string());
        }
    }
    None
}

/// Copy the last assistant answer to the clipboard and tell the user.
fn copy_last_answer(state: &mut TuiState) {
    match last_agent_text(state) {
        Some(text) => copy_and_report(state, &text, "the last answer —"),
        None => state.push_note("nothing to copy yet", NoteLevel::Info),
    }
}

fn render_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

// ---------------------------------------------------------------------------
// Engine thread
// ---------------------------------------------------------------------------

/// Messages the renderer sends to the engine thread.
enum Command {
    Run(String),
    SetMode(AgentMode),
    NewSession,
    /// Point the loop at a different configured provider, by name.
    SwitchProvider(String),
}

/// Everything the engine thread sends back, on one ordered channel.
enum EngineMsg {
    /// Startup succeeded; here is what the model can call.
    Ready(Vec<String>),
    /// Startup failed — the reason, shown instead of a blank screen.
    Fatal(String),
    Event(UiEvent),
    Done(Box<Result<ExecutionResult, String>>),
    /// Tool list after a mode change.
    Tools(Vec<String>),
    /// The provider actually in use, after a successful switch.
    Provider {
        name: String,
        kind: String,
        model: String,
        priced: bool,
    },
}

/// Launch the interactive UI. Returns once the user quits; the worker thread
/// is asked to stop and joined so no MCP/LSP child outlives the process
/// (NFR-REL-04).
pub fn run_tui(
    cfg: Config,
    cancel: CancelFlag,
    session_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (msg_tx, msg_rx) = mpsc::channel::<EngineMsg>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

    // Take stderr away from the logger for as long as the alternate screen is
    // up: a `log::warn!` from a failing MCP or LSP server would otherwise
    // paint straight over the prompt box. The guard restores stderr on every
    // exit path.
    let (log_tx, log_rx) = mpsc::channel::<String>();
    let _log_guard = LogRedirect::to(log_tx);

    let worker_cfg = cfg.clone();
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || {
        engine_thread(worker_cfg, worker_cancel, session_id, msg_tx, cmd_rx);
    });

    let result = render_loop(&cfg, &cancel, &msg_rx, &log_rx, &cmd_tx);

    // Dropping the command sender ends the worker's recv loop.
    drop(cmd_tx);
    cancel.trigger();
    let _ = worker.join();
    result
}

/// Owns the `App` for the whole session so MCP/LSP servers are started once
/// and reused across turns.
fn engine_thread(
    mut cfg: Config,
    cancel: CancelFlag,
    resume: Option<String>,
    msg_tx: Sender<EngineMsg>,
    cmd_rx: Receiver<Command>,
) {
    // Telemetry goes to a sink, never stdout: the alternate screen owns it.
    let mut app = match wire(&cfg, Box::new(io::sink())) {
        Ok(app) => app,
        Err(e) => {
            let _ = msg_tx.send(EngineMsg::Fatal(e.to_string()));
            return;
        }
    };
    app.set_emitter(Box::new(EventBridge(msg_tx.clone())));
    app.set_cancel(cancel.clone());
    app.set_pricing(cfg.price_table());

    let ctx = cfg.to_agent_context();
    // One session spans the whole REPL, so context carries across turns.
    let mut session_id: Option<String> = resume;
    let mut mode = cfg.mode;
    let _ = msg_tx.send(EngineMsg::Ready(tool_names(&app, mode)));

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Command::SetMode(next) => {
                mode = next;
                let _ = msg_tx.send(EngineMsg::Tools(tool_names(&app, mode)));
            }
            Command::NewSession => session_id = None,
            Command::SwitchProvider(name) => {
                // Build the new client *before* installing it: a typo or a
                // missing key must leave the working provider in place, not
                // strand the session with nothing to talk to.
                match cfg
                    .with_provider(&name)
                    .map_err(|e| e.to_string())
                    .and_then(|next| {
                        super::build_llm(&next)
                            .map(|llm| (next, llm))
                            .map_err(|e| e.to_string())
                    }) {
                    Ok((next, llm)) => {
                        app.set_llm(llm);
                        app.set_pricing(next.price_table());
                        let priced = next.price_table().knows(&next.model);
                        let _ = msg_tx.send(EngineMsg::Provider {
                            name: next.provider_name.clone(),
                            kind: next.provider.as_str().to_string(),
                            model: next.model.clone(),
                            priced,
                        });
                        cfg = next;
                    }
                    Err(message) => {
                        let _ = msg_tx.send(EngineMsg::Event(UiEvent::Error(message)));
                    }
                }
            }
            Command::Run(prompt) => {
                let mut req = ExecutionRequest::new(prompt);
                req.mode = mode;
                req.session_id = session_id.clone();
                req.max_turns = cfg.max_turns;
                req.max_tokens = cfg.max_tokens;
                req.max_tool_output_chars = cfg.max_tool_output_chars;

                let outcome = app.execute(&ctx, req);
                // A cancelled turn must not poison the next one.
                cancel.reset();
                let outcome = match outcome {
                    Ok(result) => {
                        session_id = Some(result.session_id.clone());
                        Ok(result)
                    }
                    Err(e) => Err(e.to_string()),
                };
                let _ = msg_tx.send(EngineMsg::Done(Box::new(outcome)));
            }
        }
    }
}

fn tool_names(app: &app::App, mode: AgentMode) -> Vec<String> {
    let mut names: Vec<String> = app
        .tool_specs_for(mode)
        .iter()
        .map(|s| s.name.clone())
        .collect();
    names.sort();
    names
}

/// Adapts the engine's `Emitter` port onto the unified message channel.
///
/// A closed channel (the user quit) is ignored so the worker thread can wind
/// down on its own terms rather than panicking mid-turn.
struct EventBridge(Sender<EngineMsg>);

impl domain::Emitter for EventBridge {
    fn emit(&mut self, ev: UiEvent) {
        let _ = self.0.send(EngineMsg::Event(ev));
    }
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

/// Restores the terminal even if rendering panics or returns early.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        // Without this, a multi-line paste arrives as a burst of individual
        // key events — which the poll loop drops under load, and which turns
        // every embedded newline into a premature "send".
        io::stdout().execute(EnableBracketedPaste)?;
        // Without this the wheel is handled by the terminal's own scrollback,
        // which the alternate screen has none of — so the conversation simply
        // does not scroll, whatever the user does with the mouse.
        //
        // The cost is that a terminal reporting mouse events no longer selects
        // text with a plain drag; every terminal keeps that on a modifier
        // (Shift, or Option on macOS Terminal), and `/help` says so.
        io::stdout().execute(EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = io::stdout().execute(DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

// ---------------------------------------------------------------------------
// Render loop
// ---------------------------------------------------------------------------

fn render_loop(
    cfg: &Config,
    cancel: &CancelFlag,
    msg_rx: &Receiver<EngineMsg>,
    log_rx: &Receiver<String>,
    cmd_tx: &Sender<Command>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _guard = TerminalGuard::enter()?;
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal: Terminal<_> = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: ratatui::Viewport::Fullscreen,
        },
    )?;

    let mut state = TuiState {
        mode: cfg.mode,
        // The name the user selected it by, which is what `/provider` lists
        // and what they would type to come back to it.
        provider: cfg.provider_name.clone(),
        providers: cfg.provider_names(),
        model: cfg.model.clone(),
        // Seed `priced` from the table so an unspent session on a known model
        // reads "$0.00" rather than "n/a" — the latter should mean "we have no
        // rate for this model", and only that. `knows` rather than `lookup`,
        // so a free route is recognised too.
        totals: Totals {
            cost: Cost {
                priced: cfg.price_table().knows(&cfg.model),
                ..Default::default()
            },
            ..Default::default()
        },
        session_dir: cfg
            .working_dir
            .join(".zcode")
            .join("sessions")
            .display()
            .to_string(),
        ..Default::default()
    };
    state
        .timeline
        .push_agent("Ready when you are. Type /help for commands.");

    let started = Instant::now();
    loop {
        // Drain everything the engine produced since the last frame, in order.
        while let Ok(msg) = msg_rx.try_recv() {
            match msg {
                EngineMsg::Event(ev) => state.apply(ev),
                EngineMsg::Done(outcome) => state.finish_turn(*outcome),
                EngineMsg::Ready(tools) | EngineMsg::Tools(tools) => state.tool_names = tools,
                EngineMsg::Provider {
                    name,
                    kind,
                    model,
                    priced,
                } => {
                    let label = if name == kind {
                        name.clone()
                    } else {
                        format!("{name} ({kind})")
                    };
                    state.provider = name;
                    state.model = model.clone();
                    // The old provider's rates say nothing about the new one;
                    // start it honest rather than carrying `n/a` or a price
                    // that belonged to a different endpoint.
                    state.totals.cost.priced = priced;
                    state.push_note(
                        &format!("provider: {label} · model: {model}"),
                        NoteLevel::Info,
                    );
                }
                EngineMsg::Fatal(message) => {
                    // Nothing can run; show why rather than an empty prompt.
                    state.push_note(&format!("startup failed: {message}"), NoteLevel::Warn);
                    state.phase = Phase::Failed(first_line(&message));
                }
            }
        }

        // Log records diverted from stderr become ordinary tool-pane lines,
        // where they are legible and subject to the same 500-line bound.
        while let Ok(line) = log_rx.try_recv() {
            state.push_note(&sanitize(&line), NoteLevel::Warn);
        }

        let frame_index = (started.elapsed().as_millis() / SPINNER_PERIOD.as_millis()) as usize;
        terminal.draw(|frame| draw(frame, &mut state, frame_index))?;

        if !event::poll(TICK)? {
            continue;
        }
        match event::read()? {
            Event::Paste(text) => {
                // The whole clipboard, at the caret, newlines and all.
                state.input.insert_str(&text);
            }
            // The wheel scrolls the conversation. Three rows a notch is what
            // terminals themselves use, so it matches every other pane the
            // user scrolls today.
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => state.scroll_up(WHEEL_ROWS),
                MouseEventKind::ScrollDown => state.scroll_down(WHEEL_ROWS),
                // Drag to select, release to copy — the same gesture the
                // terminal would have given us if we were not capturing the
                // mouse in order to scroll.
                MouseEventKind::Down(MouseButton::Left) => {
                    state.start_selection(mouse.column, mouse.row)
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    state.extend_selection(mouse.column, mouse.row)
                }
                MouseEventKind::Up(MouseButton::Left) => state.finish_selection(&mut io::stdout()),
                _ => {}
            },
            Event::Key(key) => {
                // Windows reports both press and release; act on press only.
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if handle_key(&mut state, key, cancel, cmd_tx) == Flow::Quit {
                    break;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

#[derive(PartialEq)]
enum Flow {
    Continue,
    Quit,
}

fn handle_key(
    state: &mut TuiState,
    key: event::KeyEvent,
    cancel: &CancelFlag,
    cmd_tx: &Sender<Command>,
) -> Flow {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        // Ctrl-C interrupts the turn; a second one (or one while idle) quits.
        KeyCode::Char('c') if ctrl => {
            if state.busy() {
                cancel.trigger();
                state.phase = Phase::Cancelling;
                return Flow::Continue;
            }
            return Flow::Quit;
        }
        KeyCode::Char('d') if ctrl && state.input.is_empty() => return Flow::Quit,
        KeyCode::Esc => {
            // Dismissing a highlight is the least destructive thing Esc can
            // mean, so it goes first: cancelling a turn because you wanted to
            // clear a selection would be an expensive misunderstanding.
            if state.selection.is_some() {
                state.clear_selection();
            } else if state.busy() {
                cancel.trigger();
                state.phase = Phase::Cancelling;
            } else if !state.input.is_empty() {
                state.input.clear();
            }
        }
        // Alt-Enter / Ctrl-J insert a newline; plain Enter sends.
        KeyCode::Enter if alt || shift || ctrl => state.input.insert_char('\n'),
        KeyCode::Char('j') if ctrl => state.input.insert_char('\n'),
        KeyCode::Enter => return submit(state, cmd_tx, cancel),
        KeyCode::BackTab => {
            let next = state.mode.next();
            set_mode(state, next, cmd_tx);
        }

        // -- editing ---------------------------------------------------------
        KeyCode::Backspace => state.input.backspace(),
        KeyCode::Delete => state.input.delete(),
        KeyCode::Left if ctrl || alt => state.input.word_left(),
        KeyCode::Right if ctrl || alt => state.input.word_right(),
        KeyCode::Left => state.input.left(),
        KeyCode::Right => state.input.right(),
        // Ctrl jumps the conversation to either end; bare Home/End stay with
        // the prompt, where a text cursor is what people expect them to move.
        KeyCode::Home if ctrl => state.scrollback = state.max_scroll,
        KeyCode::End if ctrl => state.scrollback = 0,
        KeyCode::Home => state.input.home(),
        KeyCode::End => state.input.end(),
        KeyCode::Char('a') if ctrl => state.input.home(),
        KeyCode::Char('e') if ctrl => state.input.end(),
        KeyCode::Char('w') if ctrl => state.input.kill_word(),
        KeyCode::Char('u') if ctrl => state.input.kill_to_start(),
        KeyCode::Char('k') if ctrl => state.input.kill_to_end(),
        KeyCode::Char('l') if ctrl => state.timeline.clear(),
        // Fold every run of tool calls, or open them all again. The mouse
        // folds one at a time; this is the "show me only the conversation"
        // and "show me everything" pair.
        KeyCode::Char('t') if ctrl => {
            let all_folded = state.every_run_folded();
            state.set_all_runs(all_folded);
        }

        // -- scrolling -------------------------------------------------------
        KeyCode::PageUp => state.scroll_up(PAGE_ROWS),
        KeyCode::PageDown => state.scroll_down(PAGE_ROWS),
        KeyCode::Up if ctrl || alt => state.scroll_up(1),
        KeyCode::Down if ctrl || alt => state.scroll_down(1),
        // Bare arrows recall prompt history; the modifiers above still scroll
        // the conversation when the caret is in the prompt.
        KeyCode::Up => history_prev(state),
        KeyCode::Down => history_next(state),

        KeyCode::Char('y') if ctrl => copy_last_answer(state),
        KeyCode::Char(c) => {
            // Typing is prompt work, so the caret comes back to the prompt and
            // the highlight goes: it would otherwise sit over the transcript
            // while the text lands somewhere else.
            state.clear_selection();
            // Typing while a recalled line is shown clears the recall pointer —
            // the edited text becomes the live draft, as in a shell.
            if state.history_cursor != state.history.len() {
                state.history_cursor = state.history.len();
            }
            state.input.insert_char(c)
        }
        _ => {}
    }
    Flow::Continue
}

/// Walk back through past prompts with `Up` (FR-IFACE-05: history recall).
fn history_prev(state: &mut TuiState) {
    if state.history.is_empty() {
        return;
    }
    if state.history_cursor == state.history.len() {
        state.history_draft = state.input.text().to_string();
    }
    if state.history_cursor > 0 {
        state.history_cursor -= 1;
        state.input.set(state.history[state.history_cursor].clone());
    }
}

/// Walk forward again with `Down`; reaching the bottom restores the draft.
fn history_next(state: &mut TuiState) {
    if state.history_cursor == state.history.len() {
        return;
    }
    state.history_cursor += 1;
    if state.history_cursor == state.history.len() {
        state.input.set(std::mem::take(&mut state.history_draft));
    } else {
        state.input.set(state.history[state.history_cursor].clone());
    }
}

/// Record a sent prompt so it can be recalled. Consecutive duplicates are dropped
/// so `Up` always surfaces a distinct prompt.
fn record_history(state: &mut TuiState, prompt: &str) {
    if prompt.trim().is_empty() {
        return;
    }
    if state.history.last().is_none_or(|l| l != prompt) {
        state.history.push(prompt.to_string());
    }
    state.history_cursor = state.history.len();
    state.history_draft.clear();
}

/// Handle Enter: run a slash command, or send the prompt to the engine.
fn submit(state: &mut TuiState, cmd_tx: &Sender<Command>, cancel: &CancelFlag) -> Flow {
    let line = state.input.text().trim().to_string();
    if line.is_empty() {
        return Flow::Continue;
    }

    if let Some(cmd) = command::parse(&line) {
        state.input.clear();
        return run_command(state, cmd, cmd_tx, cancel);
    }

    if state.busy() {
        // Queuing would silently reorder work; say so instead.
        state.push_note(
            "still working — Esc to cancel, then resend",
            NoteLevel::Warn,
        );
        return Flow::Continue;
    }

    let prompt = state.input.take();
    record_history(state, &prompt);
    state.timeline.push_user(&prompt);
    state.scrollback = 0;
    state.phase = Phase::Working {
        since: Instant::now(),
        step: 1,
        max: 0,
    };
    if cmd_tx.send(Command::Run(prompt)).is_err() {
        return Flow::Quit;
    }
    Flow::Continue
}

fn run_command(
    state: &mut TuiState,
    cmd: SlashCommand,
    cmd_tx: &Sender<Command>,
    cancel: &CancelFlag,
) -> Flow {
    match cmd {
        SlashCommand::Exit => return Flow::Quit,
        SlashCommand::Help => state.push_lines(command::help_lines()),
        SlashCommand::Mode(Some(mode)) => set_mode(state, mode, cmd_tx),
        SlashCommand::Mode(None) => {
            let mut lines = vec![format!(
                "mode: {} — {}",
                state.mode.as_str(),
                state.mode.summary()
            )];
            for mode in AgentMode::all() {
                let marker = if *mode == state.mode { "▸" } else { " " };
                lines.push(format!(
                    "  {marker} {:<9} {}",
                    mode.as_str(),
                    mode.summary()
                ));
            }
            lines.push("  (/mode <name>, or Shift-Tab to cycle)".to_string());
            state.push_lines(lines);
        }
        SlashCommand::Clear => {
            state.timeline.clear();
            state.scrollback = 0;
        }
        SlashCommand::New => {
            if state.busy() {
                state.push_note("finish or cancel the current turn first", NoteLevel::Warn);
                return Flow::Continue;
            }
            let _ = cmd_tx.send(Command::NewSession);
            state.session_id = None;
            state.totals = Totals {
                cost: Cost {
                    priced: state.totals.cost.priced,
                    ..Default::default()
                },
                ..Default::default()
            };
            state.timeline.clear();
            state
                .timeline
                .push_agent("New session — the model's context is empty.");
        }
        SlashCommand::Cost => {
            let lines = state.cost_lines();
            state.push_lines(lines);
        }
        SlashCommand::Model => state.push_lines(vec![
            format!("provider: {}", state.provider),
            format!("model:    {}", state.model),
            "(`zcode config` shows every layer and its source)".to_string(),
        ]),
        SlashCommand::Provider(None) => {
            if state.providers.is_empty() {
                state.push_lines(vec![
                    format!("provider: {}", state.provider),
                    format!("model:    {}", state.model),
                    String::new(),
                    "no `providers` array in the config — add one to switch between".to_string(),
                    "endpoints here, e.g.".to_string(),
                    "  \"providers\": [{ \"name\": \"local\", \"kind\": \"ollama\" }]".to_string(),
                ]);
            } else {
                let mut lines = vec![format!("{} provider(s) configured:", state.providers.len())];
                for name in &state.providers {
                    let marker = if *name == state.provider { "▸" } else { " " };
                    lines.push(format!("  {marker} {name}"));
                }
                lines.push("  (/provider <name> to switch)".to_string());
                state.push_lines(lines);
            }
        }
        SlashCommand::Provider(Some(name)) => {
            if state.busy() {
                state.push_note("finish or cancel the current turn first", NoteLevel::Warn);
                return Flow::Continue;
            }
            // The engine owns the client, and it is the only thing that can
            // say whether the new one built — so the switch is reported from
            // there, not guessed here.
            let _ = cmd_tx.send(Command::SwitchProvider(name));
        }
        SlashCommand::Session => match state.session_id.clone() {
            Some(id) => {
                let file = format!("{}/{id}.json", state.session_dir);
                state.push_lines(vec![format!("session: {id}"), format!("file:    {file}")]);
            }
            None => state
                .timeline
                .push_agent("session: not started yet — send a prompt first"),
        },
        SlashCommand::Tools => {
            let mut lines = vec![format!(
                "{} tool(s) available in {} mode:",
                state.tool_names.len(),
                state.mode.as_str()
            )];
            lines.extend(state.tool_names.iter().map(|n| format!("  {n}")));
            state.push_lines(lines);
        }
        SlashCommand::Stop => {
            if state.busy() {
                cancel.trigger();
                state.phase = Phase::Cancelling;
            } else {
                state.push_note("nothing to stop", NoteLevel::Info);
            }
        }
        SlashCommand::Copy(arg) => {
            let text = match arg.as_deref() {
                Some("all") => state
                    .timeline
                    .entries()
                    .iter()
                    .rev()
                    .filter_map(|e| match &e.kind {
                        EntryKind::User(t) | EntryKind::Agent(t) => Some(t.to_string()),
                        EntryKind::Note { text, .. } => Some(text.to_string()),
                        EntryKind::Tool { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => match last_agent_text(state) {
                    Some(t) => t,
                    None => {
                        state.push_note("nothing to copy yet", NoteLevel::Info);
                        return Flow::Continue;
                    }
                },
            };
            copy_and_report(state, &text, "—");
        }
        SlashCommand::Unknown(name) => state.push_note(
            &format!("unknown command `{name}` — /help lists them all"),
            NoteLevel::Warn,
        ),
    }
    state.scrollback = 0;
    Flow::Continue
}

fn set_mode(state: &mut TuiState, mode: AgentMode, cmd_tx: &Sender<Command>) {
    state.mode = mode;
    let _ = cmd_tx.send(Command::SetMode(mode));
    state.push_note(
        &format!("mode: {} — {}", mode.as_str(), mode.summary()),
        NoteLevel::Info,
    );
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &mut TuiState, frame_index: usize) {
    let area = frame.area();
    let input_rows = input_height(state, area.width);
    // Three regions now, not four: the tools pane is gone, its content folded
    // into the conversation where each call belongs.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(input_rows),
            Constraint::Length(1),
        ])
        .split(area);

    draw_conversation(frame, state, chunks[0]);
    draw_input(frame, state, chunks[1]);

    let status = Paragraph::new(Line::from(
        state.status_spans(frame_index, area.width as usize),
    ));
    frame.render_widget(status, chunks[2]);
}

/// Render the timeline.
///
/// Wrapping happens here, once, and the scroll offset is computed from the
/// rows this produces — the widget must not re-wrap, or the offset would not
/// match what is drawn.
fn draw_conversation(frame: &mut ratatui::Frame, state: &mut TuiState, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;

    // Two passes: count the rows without building them, then build only the
    // ones on screen. A 400-entry timeline is a few thousand rows, and
    // materialising all of them twelve times a second is pure churn.
    let total = render_timeline(state, width, Some((0, 0))).0;
    // The height only exists here, so this is the only place that can say how
    // far back the conversation actually goes. Recording it lets the key and
    // wheel handlers stop at the top instead of counting past it, and clamping
    // now keeps the title honest when the window is resized taller.
    state.max_scroll = total.saturating_sub(height).min(u16::MAX as usize) as u16;
    state.scrollback = state.scrollback.min(state.max_scroll);
    let offset = tail_offset(total, height, state.scrollback) as usize;
    let (_, rows, headers) = render_timeline(state, width, Some((offset, height)));
    // Screen rows, for the click handler: the pane's border, then the row's
    // position within the window.
    state.header_rows = headers
        .into_iter()
        .map(|(index, run)| (area.y + 1 + index as u16, run))
        .collect();

    let title = match state.scrollback {
        0 => " conversation ".to_string(),
        n => format!(" conversation (scrolled ↑{n}, PageDown to follow) "),
    };
    let widget = Paragraph::new(rows).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(widget, area);

    state.pane = area;
    paint_selection(frame, state, area);
}

/// Highlight the selection and lift its text out of the frame.
///
/// Done against the rendered buffer rather than the `Line`s, for two reasons:
/// the styling survives whatever colours the rows already carry, and the cells
/// are the only place the *final* text exists — after wrapping, clipping and
/// right-alignment. Copying anything else would hand over text the user cannot
/// see on screen.
fn paint_selection(frame: &mut ratatui::Frame, state: &mut TuiState, area: Rect) {
    let Some(sel) = state.selection else {
        return;
    };
    let ((from_col, from_row), (to_col, to_row)) = sel.ordered();
    let (left, right) = (area.x + 1, area.x + area.width.saturating_sub(2));
    let highlight = Style::default()
        .bg(Color::White)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);

    let mut text = String::new();
    let buffer = frame.buffer_mut();
    for row in from_row..=to_row {
        // Only the first and last rows are partial; the rest span the pane,
        // which is how a terminal's own selection reads.
        let start = if row == from_row { from_col } else { left };
        let end = if row == to_row { to_col } else { right };
        let mut line = String::new();
        for col in start..=end.min(right) {
            let cell = &mut buffer[(col, row)];
            line.push_str(cell.symbol());
            cell.set_style(highlight);
        }
        // Trailing padding is layout, not content — nobody wants to paste the
        // gap between a tool name and its duration.
        text.push_str(line.trim_end());
        if row < to_row {
            text.push('\n');
        }
    }
    state.selected_text = text;
}

/// Turn the timeline into styled, wrapped rows.
///
/// The shape, and why: a speaker header carries the timestamp, the body is
/// indented under it, and a run of tool calls is drawn as a bracketed group
/// so the eye can see where the model's work started and stopped.
///
/// ```text
/// 14:23:01  you
///   fix the build
///
/// 14:23:02  zcode
///   I'll check the compiler output first.
///
///   tools used
///   ├ ✔ 14:23:02  read      main.go                        12ms
///   └ ✔ 14:23:05  shell     go build ./...                  1.2s
/// ```
pub fn timeline_rows(state: &TuiState, width: usize) -> Vec<Line<'static>> {
    render_timeline(state, width, None).1
}

/// Total rows the timeline occupies, and the rows inside `window`.
///
/// One walker, two jobs. Every entry contributes its *height* — computed
/// without allocating — and only entries that intersect the window are
/// actually built. A 400-entry timeline is a few thousand rows; materialising
/// all of them twelve times a second, to show thirty, is pure churn.
///
/// `window: None` builds everything, which is what the tests want.
fn render_timeline(
    state: &TuiState,
    width: usize,
    window: Option<(usize, usize)>,
) -> (usize, Vec<Line<'static>>, Vec<(usize, u32)>) {
    let entries = state.timeline.entries();
    let body_width = width.saturating_sub(2).max(8);
    // Measured once for the whole frame: both walkers must agree on it or the
    // counted height would not match the rows built.
    let name_col = tool_name_col(entries);
    let mut rows: Vec<Line> = Vec::new();
    // (index into `rows`, run id) for every header actually built, so a click
    // can be turned back into the run it landed on.
    let mut headers: Vec<(usize, u32)> = Vec::new();
    let mut total = 0usize;
    // Whether the row before this entry was blank, so a gap is never doubled
    // and never opens the pane.
    let mut needs_gap = false;

    let take = |group: Vec<Line<'static>>, total: &mut usize, rows: &mut Vec<Line<'static>>| {
        for line in group {
            if let Some((skip, count)) = window {
                if *total >= skip && rows.len() < count {
                    rows.push(line);
                }
            } else {
                rows.push(line);
            }
            *total += 1;
        }
    };

    for (i, entry) in entries.iter().enumerate() {
        let previous_was_tool = i > 0 && entries[i - 1].is_tool();
        let next_is_tool = entries.get(i + 1).is_some_and(|e| e.is_tool());
        let gap = needs_gap && entry_opens_a_block(entry);
        // A run is folded or open as a whole, so the decision is made once at
        // its first entry and carried by every entry after it.
        let fold = entry_run(entry).map(|run| {
            let start = if previous_was_tool {
                run_start(entries, i)
            } else {
                i
            };
            let summary = run_summary(entries, start);
            Fold {
                run,
                summary,
                expanded: state.run_is_expanded(run, summary),
                is_header: !previous_was_tool,
            }
        });
        let height = gap as usize + entry_height(entry, body_width, width, fold, name_col);

        // Skip entries entirely above the window: count them, build nothing.
        if let Some((skip, count)) = window {
            if total + height <= skip || rows.len() >= count {
                total += height;
                needs_gap = true;
                continue;
            }
        }

        let mut group: Vec<Line> = Vec::with_capacity(height);
        if gap {
            group.push(Line::from(String::new()));
        }
        entry_rows(
            state,
            entry,
            body_width,
            width,
            fold,
            next_is_tool,
            name_col,
            &mut group,
        );
        // The header is the first row of the group, after any gap.
        let header_at = fold
            .filter(|f| f.is_header)
            .map(|f| (rows.len() + usize::from(gap), f.run));
        let before = rows.len();
        take(group, &mut total, &mut rows);
        if let Some((index, run)) = header_at {
            // Only if it actually landed inside the window.
            if rows.len() > before && index < rows.len() {
                headers.push((index, run));
            }
        }
        needs_gap = true;
    }

    // The answer being streamed right now, under a live header.
    if !state.streaming.is_empty() {
        let mut group: Vec<Line> = Vec::new();
        if needs_gap {
            group.push(Line::from(String::new()));
        }
        group.push(speaker_line(
            &state.timeline.now_clock(),
            "zcode",
            Color::Cyan,
        ));
        push_body(&mut group, &state.streaming, body_width, Style::default());
        take(group, &mut total, &mut rows);
    }

    (total, rows, headers)
}

/// What a run of consecutive tool calls amounts to, for its header.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunSummary {
    calls: usize,
    failed: usize,
    running: usize,
    elapsed_ms: u32,
}

impl RunSummary {
    /// Whether this run is shown open when nobody has said otherwise.
    ///
    /// Two things stay visible without being asked for:
    ///
    /// * **Work in flight** — folding a call away while it is still happening
    ///   hides the only thing on screen that is changing.
    /// * **A failure** — the error is the text you have to act on, and a
    ///   header reading "1 failed" tells you something went wrong while hiding
    ///   what. Counting it and folding it would be the worst of both.
    ///
    /// Everything else folds once it settles, which is what keeps a long
    /// session readable.
    pub fn expanded_by_default(&self) -> bool {
        self.running > 0 || self.failed > 0
    }
}

/// Summarise the run beginning at `start`, which must be a tool entry.
fn run_summary(entries: &[timeline::Entry], start: usize) -> RunSummary {
    let mut summary = RunSummary::default();
    for entry in entries[start..].iter() {
        let EntryKind::Tool {
            status, elapsed_ms, ..
        } = &entry.kind
        else {
            break;
        };
        summary.calls += 1;
        summary.elapsed_ms = summary.elapsed_ms.saturating_add(*elapsed_ms);
        match status {
            ToolStatus::Running => summary.running += 1,
            ToolStatus::Failed | ToolStatus::Denied => summary.failed += 1,
            ToolStatus::Ok => {}
        }
    }
    summary
}

/// The run id of a tool entry, or `None` for anything else.
fn entry_run(entry: &timeline::Entry) -> Option<u32> {
    match entry.kind {
        EntryKind::Tool { run, .. } => Some(run),
        _ => None,
    }
}

/// The header that opens a run, and folds it.
fn run_header(summary: RunSummary, expanded: bool) -> Line<'static> {
    let marker = if expanded { "▾" } else { "▸" };
    let mut text = format!("  {marker} tools used");
    if !expanded {
        // Folded, the header is all there is, so it has to carry enough to
        // decide whether opening it is worth doing.
        text.push_str(&format!(
            " · {} call{}",
            summary.calls,
            if summary.calls == 1 { "" } else { "s" }
        ));
        let elapsed = timeline::render_duration(summary.elapsed_ms);
        if !elapsed.is_empty() {
            text.push_str(&format!(" · {elapsed}"));
        }
        if summary.failed > 0 {
            text.push_str(&format!(" · {} failed", summary.failed));
        }
    }
    let colour = if summary.failed > 0 {
        Color::Red
    } else {
        Color::DarkGray
    };
    Line::styled(text, Style::default().fg(colour))
}

/// Whether an entry starts a new block, and so wants a blank line above it.
/// Tool rows and notes belong to the message before them.
fn entry_opens_a_block(entry: &timeline::Entry) -> bool {
    matches!(entry.kind, EntryKind::User(_) | EntryKind::Agent(_))
}

/// Rows an entry occupies, without building them.
///
/// Must agree with [`entry_rows`] exactly, or the scroll window would be
/// placed against a height the renderer does not produce. `rows_match_height`
/// pins that for every entry kind.
/// How a run of tool calls is being shown on this frame.
#[derive(Debug, Clone, Copy)]
struct Fold {
    run: u32,
    summary: RunSummary,
    expanded: bool,
    /// This entry is the first of its run, so it draws the header.
    is_header: bool,
}

/// Walk back to the first entry of the run containing `i`.
fn run_start(entries: &[timeline::Entry], i: usize) -> usize {
    let mut start = i;
    while start > 0 && entries[start - 1].is_tool() {
        start -= 1;
    }
    start
}

fn entry_height(
    entry: &timeline::Entry,
    body_width: usize,
    width: usize,
    fold: Option<Fold>,
    name_col: usize,
) -> usize {
    match &entry.kind {
        EntryKind::User(text) | EntryKind::Agent(text) => {
            1 + text
                .split('\n')
                .map(|l| wrap::height(l, body_width))
                .sum::<usize>()
        }
        // A label for the run, one row for the call itself, and — when the
        // call failed with more to say than fits — the wrapped message below.
        EntryKind::Tool {
            name,
            detail,
            status,
            elapsed_ms,
            ..
        } => {
            let fold = fold.unwrap_or(Fold {
                run: 0,
                summary: RunSummary::default(),
                expanded: true,
                is_header: true,
            });
            let header = usize::from(fold.is_header);
            if !fold.expanded {
                // Folded, the run is exactly its header — and only the first
                // entry draws that.
                return header;
            }
            let room = tool_detail_room(name, *elapsed_ms, width, name_col);
            let below = if detail_wraps_below(detail, *status, room) {
                detail_height(detail, width)
            } else {
                0
            };
            header + 1 + below
        }
        EntryKind::Note { text, .. } => wrap::height(text, body_width.saturating_sub(2)),
    }
}

/// Build the rows for one entry.
#[allow(clippy::too_many_arguments)]
fn entry_rows(
    state: &TuiState,
    entry: &timeline::Entry,
    body_width: usize,
    width: usize,
    fold: Option<Fold>,
    next_is_tool: bool,
    name_col: usize,
    out: &mut Vec<Line<'static>>,
) {
    match &entry.kind {
        EntryKind::User(text) => {
            out.push(speaker_line(
                &state.timeline.clock(entry.at_ms),
                "you",
                Color::Green,
            ));
            push_body(out, text, body_width, Style::default());
        }
        EntryKind::Agent(text) => {
            out.push(speaker_line(
                &state.timeline.clock(entry.at_ms),
                "zcode",
                Color::Cyan,
            ));
            push_body(out, text, body_width, Style::default());
        }
        EntryKind::Tool {
            name,
            detail,
            status,
            elapsed_ms,
            ..
        } => {
            let fold = fold.unwrap_or(Fold {
                run: 0,
                summary: RunSummary::default(),
                expanded: true,
                is_header: true,
            });
            // Head the run with a label, so the group reads as one block — and
            // so there is something to click to fold it.
            if fold.is_header {
                out.push(run_header(fold.summary, fold.expanded));
            }
            if !fold.expanded {
                return;
            }
            let branch = if next_is_tool { "├" } else { "└" };
            let room = tool_detail_room(name, *elapsed_ms, width, name_col);
            let below = detail_wraps_below(detail, *status, room);
            out.push(tool_line(
                branch,
                *status,
                &state.timeline.clock(entry.at_ms),
                name,
                if below { "" } else { detail },
                *elapsed_ms,
                width,
                name_col,
            ));
            if below {
                push_detail(out, detail, width, *status);
            }
        }
        EntryKind::Note { text, level } => {
            let colour = match level {
                NoteLevel::Info => Color::Blue,
                NoteLevel::Retry => Color::Yellow,
                NoteLevel::Warn => Color::Red,
            };
            let prefix = format!("  {} ", level.icon());
            for (n, wrapped) in wrap::wrap(text, body_width.saturating_sub(2))
                .into_iter()
                .enumerate()
            {
                let head = if n == 0 {
                    prefix.clone()
                } else {
                    "    ".into()
                };
                out.push(Line::from(vec![
                    Span::styled(head, Style::default().fg(colour)),
                    Span::styled(wrapped, Style::default().fg(colour)),
                ]));
            }
        }
    }
}

fn speaker_line(clock: &str, who: &str, colour: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{clock}  "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            who.to_string(),
            Style::default().fg(colour).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn push_body(rows: &mut Vec<Line<'static>>, text: &str, width: usize, style: Style) {
    for line in text.split('\n') {
        for wrapped in wrap::wrap(line, width) {
            rows.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wrapped, style),
            ]));
        }
    }
}

/// Width of the tool-name column, before the detail begins.
/// Bounds on the tool-name column.
///
/// It used to be a flat 18, which put fourteen blank cells between `read` and
/// what it read. The column exists so a run of rows lines up, and it only has
/// to be as wide as the widest name actually on screen — so it is measured
/// per frame, floored so a lone `read` still reads as a column, and capped so
/// one `mcp__some_server__some_tool` cannot push the detail off the row.
const TOOL_NAME_MIN: usize = 6;
const TOOL_NAME_MAX: usize = 22;

/// Width of the icon and the space after it, ahead of the name.
const TOOL_ICON_COL: usize = 2;

/// The name column for this frame: the widest tool name in the timeline,
/// within bounds. Computed once and passed down, so the two walkers that must
/// agree on row heights are measuring the same thing.
fn tool_name_col(entries: &[timeline::Entry]) -> usize {
    entries
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::Tool { name, .. } => Some(name.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(TOOL_NAME_MIN)
        .clamp(TOOL_NAME_MIN, TOOL_NAME_MAX)
}

/// `HH:MM:SS`. [`timeline::Timeline::clock`] is fixed width, which is what
/// lets [`entry_height`] size a tool row without building it.
const TOOL_CLOCK_WIDTH: usize = 8;

/// Indent of a failure message wrapped below its tool row: `"  └ ✖ "`, so the
/// text starts where the clock does and still reads as part of the row.
const TOOL_DETAIL_INDENT: usize = 6;

/// Characters left for the detail on the tool row itself.
fn tool_detail_room(name: &str, elapsed_ms: u32, width: usize, name_col: usize) -> usize {
    let head = TOOL_DETAIL_INDENT + TOOL_CLOCK_WIDTH + 2;
    // A name past the cap pushes the column out rather than being cut; the
    // detail gets whatever is left.
    let names = TOOL_ICON_COL + name.chars().count().max(name_col) + 1;
    let duration = timeline::render_duration(elapsed_ms).chars().count();
    width.saturating_sub(head + names + duration + 1)
}

/// Whether a tool row's detail has to move to its own rows underneath.
///
/// A successful call's detail is an index entry — the first line of what came
/// back — and clipping it to the row is the point. A failure's detail is a
/// message someone has to act on: `command blocked by the shell allowlist
/// (`shell_allowed` in zcode.json/zcode.toml): cd /works…` names neither the
/// rule that refused it nor the command that was refused. So when a failure
/// does not fit, it is wrapped below in full instead of being truncated.
fn detail_wraps_below(detail: &str, status: ToolStatus, room: usize) -> bool {
    matches!(status, ToolStatus::Failed | ToolStatus::Denied)
        && !detail.is_empty()
        && (detail.contains('\n') || detail.chars().count() > room)
}

/// Rows [`push_detail`] will produce, without building them.
fn detail_height(detail: &str, width: usize) -> usize {
    let inner = detail_width(width);
    detail
        .split('\n')
        .map(|line| wrap::height(line, inner))
        .sum()
}

fn detail_width(width: usize) -> usize {
    width.saturating_sub(TOOL_DETAIL_INDENT).max(8)
}

/// The wrapped rows of a failure message, under its tool row.
fn push_detail(out: &mut Vec<Line<'static>>, detail: &str, width: usize, status: ToolStatus) {
    let colour = match status {
        ToolStatus::Denied => Color::Magenta,
        _ => Color::Red,
    };
    let inner = detail_width(width);
    for line in detail.split('\n') {
        for wrapped in wrap::wrap(line, inner) {
            out.push(Line::from(vec![
                Span::raw(" ".repeat(TOOL_DETAIL_INDENT)),
                Span::styled(wrapped, Style::default().fg(colour)),
            ]));
        }
    }
}

/// One tool row: branch glyph, status icon, time, name, detail, duration.
///
/// The duration is right-aligned into whatever space is left. A *successful*
/// detail is truncated to fit rather than wrapped — that row is an index
/// entry, not the output itself; a failure that does not fit is handed to
/// [`push_detail`] instead and reaches this function empty.
#[allow(clippy::too_many_arguments)]
fn tool_line(
    branch: &str,
    status: ToolStatus,
    clock: &str,
    name: &str,
    detail: &str,
    elapsed_ms: u32,
    width: usize,
    name_col: usize,
) -> Line<'static> {
    let colour = match status {
        ToolStatus::Running => Color::Yellow,
        ToolStatus::Ok => Color::Green,
        ToolStatus::Failed => Color::Red,
        ToolStatus::Denied => Color::Magenta,
    };
    let duration = timeline::render_duration(elapsed_ms);
    // "  ├ ✔ 14:23:02  " + name column + detail + right-aligned duration.
    let head = format!("  {branch} {} {clock}  ", status.icon());
    let name_col = format!(
        "{icon} {name:<width$} ",
        icon = timeline::tool_icon(name),
        width = name_col
    );
    let used = head.chars().count() + name_col.chars().count() + duration.chars().count() + 1;
    let room = width.saturating_sub(used);
    let detail = clip(detail, room);
    let pad = room.saturating_sub(detail.chars().count());

    Line::from(vec![
        Span::styled(head, Style::default().fg(Color::DarkGray)),
        Span::styled(name_col, Style::default().fg(colour)),
        Span::raw(detail),
        Span::raw(" ".repeat(pad)),
        Span::styled(duration, Style::default().fg(Color::DarkGray)),
    ])
}

/// Truncate to `max` characters with an ellipsis, on a char boundary.
fn clip(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn draw_input(frame: &mut ratatui::Frame, state: &TuiState, area: Rect) {
    let width = area.width.saturating_sub(4) as usize; // borders + "> "
    let visible = area.height.saturating_sub(2) as usize;
    let rows = wrap::wrap(state.input.text(), width.max(1));
    let (cursor_row, cursor_col) = cursor_position(state, width.max(1));
    // Keep the caret on screen when the prompt is taller than the box.
    let offset = cursor_row.saturating_sub(visible.saturating_sub(1));

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let marker = if i == 0 { "> " } else { "  " };
            Line::from(vec![
                Span::styled(marker, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(row.clone()),
            ])
        })
        .collect();

    let hint = if state.busy() {
        " Esc cancels "
    } else {
        " Enter sends · Alt-Enter newline · /help "
    };
    let widget = Paragraph::new(lines)
        .scroll((offset as u16, 0))
        .block(Block::default().borders(Borders::ALL).title(hint));
    frame.render_widget(widget, area);

    // A visible caret: the single most-missed piece of terminal-app etiquette.
    //
    // It follows the pointer. Clicking into the conversation puts it on the
    // cell you clicked, so the thing you are about to select from is the thing
    // showing a cursor; typing, or Esc, brings it back to the prompt. Leaving
    // it blinking in the prompt while you drag through the transcript points
    // at the wrong place entirely.
    if let Some(at) = state.caret_in_conversation() {
        frame.set_cursor_position(Position::new(at.0, at.1));
        return;
    }
    let x = area.x + 1 + 2 + cursor_col as u16;
    let y = area.y + 1 + (cursor_row - offset) as u16;
    frame.set_cursor_position(Position::new(
        x.min(area.x + area.width.saturating_sub(2)),
        y.min(area.y + area.height.saturating_sub(2)),
    ));
}

/// Where the caret sits once the prompt is wrapped to `width`.
fn cursor_position(state: &TuiState, width: usize) -> (usize, usize) {
    let (logical_line, col) = state.input.line_col();
    let mut row = 0usize;
    for (i, line) in state.input.text().split('\n').enumerate() {
        if i == logical_line {
            // Rows consumed by the caret's own column within this line.
            return (row + col / width, col % width);
        }
        row += wrap::wrap(line, width).len();
    }
    (row, 0)
}

/// Rows the prompt box needs: enough for the text, within bounds.
fn input_height(state: &TuiState, total_width: u16) -> u16 {
    let width = total_width.saturating_sub(4).max(1) as usize;
    let rows = wrap::wrap(state.input.text(), width).len() as u16;
    rows.clamp(1, MAX_INPUT_ROWS) + 2 // borders
}

/// Whether a screen cell is inside `area`'s border.
fn inside(area: Rect, col: u16, row: u16) -> bool {
    col > area.x
        && row > area.y
        && col < area.x.saturating_add(area.width).saturating_sub(1)
        && row < area.y.saturating_add(area.height).saturating_sub(1)
}

/// The nearest cell inside `area`'s border, so a drag off the edge selects to
/// the edge rather than stopping where the pointer left.
fn clamp_into(area: Rect, col: u16, row: u16) -> (u16, u16) {
    let left = area.x.saturating_add(1);
    let top = area.y.saturating_add(1);
    let right = area
        .x
        .saturating_add(area.width)
        .saturating_sub(2)
        .max(left);
    let bottom = area
        .y
        .saturating_add(area.height)
        .saturating_sub(2)
        .max(top);
    (col.clamp(left, right), row.clamp(top, bottom))
}

/// Scroll offset that keeps the newest lines visible, honouring scrollback.
fn tail_offset(total_rows: usize, visible_rows: usize, scrollback: u16) -> u16 {
    let max_offset = total_rows.saturating_sub(visible_rows) as u16;
    max_offset.saturating_sub(scrollback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{LlmFinishReason, RetryNotice, UiEvent};

    fn result(steps: u64, text: &str) -> ExecutionResult {
        ExecutionResult {
            session_id: "s1".into(),
            final_text: text.into(),
            steps,
            finish_reason: LlmFinishReason::Stop,
            truncated: false,
            input_tokens: 7,
            output_tokens: 3,
            cache_tokens: 0,
            cost: Cost {
                output_usd: 0.5,
                priced: true,
                ..Default::default()
            },
        }
    }

    fn busy_state() -> TuiState {
        TuiState {
            phase: Phase::Working {
                since: Instant::now(),
                step: 1,
                max: 20,
            },
            ..Default::default()
        }
    }

    /// The timeline as plain text, the way the screen reads it.
    fn text_of(state: &TuiState) -> String {
        timeline_rows(state, 100)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn agent_texts(state: &TuiState) -> Vec<String> {
        state
            .timeline
            .entries()
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Agent(t) => Some(t.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn deltas_accumulate_then_commit_on_finish() {
        let mut state = TuiState::default();
        state.apply(UiEvent::Delta("hel".into()));
        state.apply(UiEvent::Delta("lo".into()));
        assert_eq!(state.streaming, "hello");

        let mut state = TuiState {
            streaming: state.streaming,
            ..busy_state()
        };
        state.finish_turn(Ok(result(1, "hello")));
        assert!(state.streaming.is_empty());
        assert_eq!(agent_texts(&state), vec!["hello".to_string()]);
        assert!(!state.busy());
    }

    #[test]
    fn non_streaming_answer_still_lands_in_the_timeline() {
        let mut state = TuiState::default();
        state.finish_turn(Ok(result(1, "final only")));
        assert_eq!(agent_texts(&state), vec!["final only".to_string()]);
    }

    #[test]
    fn engine_errors_are_shown_not_swallowed() {
        let mut state = busy_state();
        state.finish_turn(Err("openrouter request failed (429): rate limited".into()));
        assert!(text_of(&state).contains("429"));
        assert!(!state.busy());
        // …and the status bar keeps saying so until the next turn.
        assert!(matches!(state.phase, Phase::Failed(_)));
        let text: String = state
            .status_spans(0, 200)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("error"), "{text}");
    }

    #[test]
    fn a_tool_call_is_one_timeline_row_under_the_message_that_made_it() {
        let mut state = TuiState::default();
        state.apply(UiEvent::Delta("Let me look at the file.".into()));
        state.apply(UiEvent::ToolCallStart {
            id: "c1".into(),
            name: "read".into(),
        });
        state.apply(UiEvent::ToolCallArgs {
            id: "c1".into(),
            arguments: r#"{"path":"main.go"}"#.into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "read".into(),
            content: "package main\nimport \"fmt\"".into(),
            error: None,
            elapsed_ms: 12,
        });

        // The prose was committed first, so the tool sits under it.
        assert_eq!(
            agent_texts(&state),
            vec!["Let me look at the file.".to_string()]
        );
        open_runs(&mut state);
        let text = text_of(&state);
        let prose = text.find("Let me look").expect("prose is shown");
        let tools = text.find("tools used").expect("the group is labelled");
        let row = text.find("read").expect("the call is shown");
        assert!(prose < tools && tools < row, "{text}");
        assert!(
            text.contains('✔'),
            "a settled call shows its status: {text}"
        );
        // One row, not a start and a finish.
        assert_eq!(text.matches("read ").count(), 1, "{text}");
    }

    #[test]
    fn a_run_of_tools_is_drawn_as_one_bracketed_group() {
        let mut state = TuiState::default();
        for (id, name) in [("c1", "read"), ("c2", "list_dir"), ("c3", "shell")] {
            state.apply(UiEvent::ToolCallStart {
                id: id.into(),
                name: name.into(),
            });
            state.apply(UiEvent::ToolResult {
                tool_call_id: id.into(),
                name: name.into(),
                content: "ok".into(),
                error: None,
                elapsed_ms: 12,
            });
        }
        open_runs(&mut state);
        let text = text_of(&state);
        // One header for the run, and the last row closes the bracket.
        assert_eq!(text.matches("tools used").count(), 1, "{text}");
        assert_eq!(text.matches('├').count(), 2, "{text}");
        assert_eq!(text.matches('└').count(), 1, "{text}");
    }

    #[test]
    fn a_failing_tool_is_marked_as_failed() {
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolCallStart {
            id: "c1".into(),
            name: "shell".into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "shell".into(),
            content: String::new(),
            error: Some("blocked by the allowlist".into()),
            elapsed_ms: 12,
        });
        open_runs(&mut state);
        let text = text_of(&state);
        assert!(text.contains('✖'), "{text}");
        assert!(text.contains("blocked by the allowlist"), "{text}");
    }

    #[test]
    fn a_denied_tool_settles_its_row_rather_than_floating_free() {
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolCallStart {
            id: "c1".into(),
            name: "apply_patch".into(),
        });
        state.apply(UiEvent::Error(
            "tool `apply_patch` denied: planning mode is read-only".into(),
        ));
        open_runs(&mut state);
        let text = text_of(&state);
        assert!(text.contains('⊘'), "{text}");
        assert_eq!(text.matches("apply_patch").count(), 1, "{text}");
    }

    #[test]
    fn every_row_carries_a_timestamp() {
        let mut state = TuiState::default();
        state.timeline.push_user("hello");
        state.apply(UiEvent::ToolCallStart {
            id: "c1".into(),
            name: "read".into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "read".into(),
            content: "ok".into(),
            error: None,
            elapsed_ms: 12,
        });
        open_runs(&mut state);
        let clock = regex_free_clock_count(&text_of(&state));
        assert!(
            clock >= 2,
            "expected a clock on the message and the tool row"
        );
    }

    /// Count `HH:MM:SS` stamps without pulling in a regex. A stamp can sit
    /// anywhere on the line: at the head of a speaker row, mid-row on a tool.
    fn regex_free_clock_count(text: &str) -> usize {
        fn has_clock(line: &str) -> bool {
            let bytes = line.as_bytes();
            bytes.windows(8).any(|w| {
                w.iter().enumerate().all(|(i, b)| {
                    if i == 2 || i == 5 {
                        *b == b':'
                    } else {
                        b.is_ascii_digit()
                    }
                })
            })
        }
        text.lines().filter(|l| has_clock(l)).count()
    }

    #[test]
    fn tool_output_escapes_are_stripped_before_rendering() {
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "shell".into(),
            content: "\u{1b}[2Jwiped".into(),
            error: None,
            elapsed_ms: 12,
        });
        assert!(!text_of(&state).contains('\u{1b}'));
        // Streamed model text is sanitised too.
        state.apply(UiEvent::Delta("\u{1b}[31mred".into()));
        assert!(!state.streaming.contains('\u{1b}'));
    }

    #[test]
    fn the_timeline_is_bounded() {
        let mut state = TuiState::default();
        for i in 0..(timeline::MAX_ENTRIES + 50) {
            state.timeline.push_agent(&format!("line {i}"));
        }
        assert_eq!(state.timeline.entries().len(), timeline::MAX_ENTRIES);
        // The oldest entries are the ones dropped.
        assert_eq!(agent_texts(&state)[0], "line 50");
    }

    #[test]
    fn a_long_session_stays_within_its_memory_budget() {
        // The point of the whole restructure: one bounded list, every string
        // capped at ingest, and no second pane holding a parallel copy.
        let mut state = TuiState::default();
        for i in 0..2_000 {
            state.timeline.push_user(&"u".repeat(500));
            state.timeline.push_agent(&"a".repeat(2_000));
            state.apply(UiEvent::ToolCallStart {
                id: format!("c{i}"),
                name: "shell".into(),
            });
            state.apply(UiEvent::ToolResult {
                tool_call_id: format!("c{i}"),
                name: "shell".into(),
                // A tool that returns one enormous line.
                content: "x".repeat(200_000),
                error: None,
                elapsed_ms: 12,
            });
        }
        let bytes = state.timeline.heap_bytes();
        assert!(
            bytes < 1_000_000,
            "8000 entries later the timeline holds {bytes} bytes"
        );
    }

    #[test]
    fn a_huge_streamed_answer_does_not_hold_its_peak() {
        let mut state = TuiState::default();
        state.apply(UiEvent::Delta("z".repeat(200_000)));
        state.finish_turn(Ok(result(1, "")));
        assert!(
            state.streaming.capacity() <= 8 * 1024,
            "kept a {}-byte buffer",
            state.streaming.capacity()
        );
    }

    // ---- progress, retries, failures --------------------------------------

    #[test]
    fn loop_start_reports_progress_and_spins() {
        let mut state = TuiState::default();
        state.apply(UiEvent::LoopStart {
            step: 2,
            max_turns: 20,
        });
        assert!(state.busy());
        let text: String = state
            .status_spans(0, 200)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("step 2/20"), "{text}");
        // The spinner glyph advances with the frame index.
        let a = state.status_spans(0, 200)[0].content.to_string();
        let b = state.status_spans(1, 200)[0].content.to_string();
        assert_ne!(a, b);
    }

    #[test]
    fn the_elapsed_clock_survives_a_multi_step_turn() {
        let mut state = TuiState::default();
        state.apply(UiEvent::LoopStart {
            step: 1,
            max_turns: 20,
        });
        let Phase::Working { since: first, .. } = state.phase.clone() else {
            panic!("not working");
        };
        state.apply(UiEvent::LoopStart {
            step: 2,
            max_turns: 20,
        });
        let Phase::Working { since: second, .. } = state.phase.clone() else {
            panic!("not working");
        };
        assert_eq!(first, second, "the timer must not restart each step");
    }

    #[test]
    fn a_rate_limit_is_visible_rather_than_looking_like_a_hang() {
        let mut state = busy_state();
        state.apply(UiEvent::Retry(RetryNotice {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 2_000,
            status: Some(429),
            reason: "rate limited by the provider".into(),
        }));
        assert!(matches!(state.phase, Phase::RateLimited { .. }));
        assert!(text_of(&state).contains("429"), "{}", text_of(&state));
        let text: String = state
            .status_spans(0, 200)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("rate limited"), "{text}");
        assert!(state.busy());
    }

    #[test]
    fn the_backoff_state_clears_when_output_resumes() {
        // Regression: the bar kept saying "retrying in 1.0s" for the whole
        // answer that followed the retry — claiming we were waiting while
        // tokens were arriving.
        let mut state = TuiState::default();
        state.apply(UiEvent::LoopStart {
            step: 2,
            max_turns: 12,
        });
        state.apply(UiEvent::Retry(RetryNotice {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 30_000,
            status: Some(429),
            reason: "rate limited by the provider".into(),
        }));
        assert!(matches!(state.phase, Phase::RateLimited { .. }));

        state.apply(UiEvent::Delta("the answer".into()));
        let Phase::Working { step, max, .. } = state.phase else {
            panic!("still rate limited after output resumed: {:?}", state.phase);
        };
        // …and it resumes the step it interrupted, not a fresh one.
        assert_eq!((step, max), (2, 12));
        let text: String = state
            .status_spans(0, 200)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("step 2/12"), "{text}");
        assert!(!text.contains("rate limited"), "{text}");
    }

    #[test]
    fn a_tool_call_also_clears_the_backoff_state() {
        let mut state = busy_state();
        state.apply(UiEvent::Retry(RetryNotice {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 30_000,
            status: Some(429),
            reason: "rate limited by the provider".into(),
        }));
        state.apply(UiEvent::ToolCallStart {
            id: "c1".into(),
            name: "read".into(),
        });
        assert!(matches!(state.phase, Phase::Working { .. }));
    }

    #[test]
    fn the_retry_note_survives_the_phase_returning_to_working() {
        // The status bar moves on; the timeline keeps the record.
        let mut state = busy_state();
        state.apply(UiEvent::Retry(RetryNotice {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 30_000,
            status: Some(429),
            reason: "rate limited by the provider".into(),
        }));
        state.apply(UiEvent::Delta("answer".into()));
        assert!(text_of(&state).contains("429"), "{}", text_of(&state));
    }

    #[test]
    fn a_tool_row_reports_its_duration() {
        let mut state = TuiState::default();
        state.timeline.start_tool("shell", "go build ./...");
        std::thread::sleep(std::time::Duration::from_millis(12));
        state
            .timeline
            .finish_tool("shell", "ok", ToolStatus::Ok, 12);
        let text = text_of(&state);
        assert!(text.contains("ms") || text.contains('s'), "{text}");
    }

    #[test]
    fn the_status_bar_carries_mode_model_and_cost() {
        let mut state = TuiState {
            provider: "openrouter".into(),
            model: "openai/gpt-4o-mini".into(),
            mode: AgentMode::Planning,
            ..Default::default()
        };
        state.finish_turn(Ok(result(1, "done")));
        let text: String = state
            .status_spans(0, 200)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("mode planning"), "{text}");
        assert!(text.contains("openrouter/openai/gpt-4o-mini"), "{text}");
        assert!(text.contains("7 in / 3 out"), "{text}");
        assert!(text.contains("$0.50"), "{text}");
    }

    #[test]
    fn totals_accumulate_over_turns() {
        let mut state = TuiState::default();
        state.finish_turn(Ok(result(1, "a")));
        state.finish_turn(Ok(result(2, "b")));
        assert_eq!(state.totals.turns, 2);
        assert_eq!(state.totals.steps, 3);
        assert_eq!(state.totals.input_tokens, 14);
        assert!((state.totals.cost.total_usd() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_known_model_with_nothing_spent_shows_zero_not_unknown() {
        // "n/a" must mean "no rate for this model", not "nothing spent yet".
        let state = TuiState {
            totals: Totals {
                cost: Cost {
                    priced: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let text: String = state
            .status_spans(0, 200)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("$0.00"), "{text}");
    }

    #[test]
    fn an_unpriced_model_says_so_rather_than_claiming_zero() {
        let state = TuiState {
            model: "private-1".into(),
            ..Default::default()
        };
        let text = state.cost_lines().join("\n");
        assert!(text.contains("n/a"), "{text}");
        assert!(text.contains("[[pricing]]"), "{text}");
    }

    /// Render a status bar at `width` and return it as plain text.
    fn status_text(state: &TuiState, width: usize) -> String {
        state
            .status_spans(0, width)
            .iter()
            .map(|s| s.content.to_string())
            .collect()
    }

    #[test]
    fn the_status_bar_sheds_detail_to_keep_the_cost_visible() {
        // Regression: `openrouter/poolside/laguna-s-2.1:free` pushed the cost
        // off the right edge of a 100-column terminal.
        let mut state = TuiState {
            provider: "openrouter".into(),
            model: "poolside/laguna-s-2.1:free".into(),
            ..Default::default()
        };
        state.totals.cache_tokens = 2496;
        state.finish_turn(Ok(result(1, "done")));

        for width in [40, 60, 80, 100, 120, 200] {
            let text = status_text(&state, width);
            assert!(
                text.chars().count() <= width,
                "width {width}: {} chars: {text:?}",
                text.chars().count()
            );
            assert!(
                text.contains("$0.50"),
                "width {width} lost the cost: {text}"
            );
            assert!(text.contains("mode"), "width {width} lost the mode: {text}");
            assert!(
                text.contains("ready"),
                "width {width} lost the state: {text}"
            );
        }
    }

    #[test]
    fn a_wide_terminal_still_gets_every_field() {
        let mut state = TuiState {
            provider: "openrouter".into(),
            model: "poolside/laguna-s-2.1:free".into(),
            ..Default::default()
        };
        state.totals.cache_tokens = 2496;
        state.finish_turn(Ok(result(1, "done")));
        let text = status_text(&state, 200);
        assert!(
            text.contains("openrouter/poolside/laguna-s-2.1:free"),
            "{text}"
        );
        assert!(text.contains("2496 cached"), "{text}");
    }

    #[test]
    fn a_narrow_terminal_keeps_the_model_recognisable() {
        let state = TuiState {
            provider: "openrouter".into(),
            model: "poolside/laguna-s-2.1:free".into(),
            ..Default::default()
        };
        // The vendor namespace goes before the model name does.
        let text = status_text(&state, 70);
        assert!(text.contains("laguna-s-2.1:free"), "{text}");
    }

    #[test]
    fn truncation_is_surfaced() {
        let mut state = TuiState::default();
        state.apply(UiEvent::LoopEnd {
            steps: 20,
            finish_reason: LlmFinishReason::Length,
            truncated: true,
        });
        assert!(text_of(&state).contains("cap"));
    }

    // ---- scrolling ---------------------------------------------------------

    /// A state exercising every entry kind, several tool runs, wrapping,
    /// notes at each level, and a partially streamed answer.
    fn busy_timeline() -> TuiState {
        let mut state = TuiState::default();
        state
            .timeline
            .push_user("fix the build, it is failing on CI somewhere");
        state
            .timeline
            .push_agent("Looking now.\nThis answer has\nseveral lines and one that is long enough to wrap more than once at a narrow width.");
        for (name, detail, status) in [
            ("read", "main.go", ToolStatus::Ok),
            ("list_dir", ".", ToolStatus::Ok),
            ("shell", "go build ./...", ToolStatus::Failed),
            ("shell", BLOCKED, ToolStatus::Failed),
            (
                "apply_patch",
                "planning mode is read-only",
                ToolStatus::Denied,
            ),
            (
                "mcp__a_very_long_server__tool",
                "connection refused",
                ToolStatus::Failed,
            ),
        ] {
            state.timeline.start_tool(name, detail);
            state.timeline.finish_tool(name, detail, status, 12);
        }
        state.push_note("rate limited by the provider (429)", NoteLevel::Retry);
        state.push_note(
            "a warning that is long enough to wrap at narrow widths",
            NoteLevel::Warn,
        );
        state.timeline.push_agent("Fixed it.");
        state.timeline.start_tool("shell", "go test ./...");
        state.streaming = "and here is some text still streaming in".to_string();
        state
    }

    /// The refusal that started this: long enough that clipping it to a tool
    /// row loses both the rule and the command, and carrying a hint on its own
    /// line.
    const BLOCKED: &str = "command blocked by the shell allowlist (`shell_allowed` in \
         zcode.json/zcode.toml): cd /workspace && go build ./... 2>&1 | head\n  hint: \
         no pattern in `shell_allowed` matches `cd`; add one, e.g. \"cd( .*)?\"";

    /// Open every run, for tests about how a row is drawn rather than about
    /// whether it is folded. Settled runs fold by default, which is the point
    /// of the feature and not what those tests are checking.
    fn open_runs(state: &mut TuiState) {
        state.set_all_runs(true);
    }

    fn rendered(state: &TuiState, width: usize) -> String {
        let (_, rows, _) = render_timeline(state, width, None);
        rows.iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_failure_too_long_for_its_row_is_wrapped_not_truncated() {
        let mut state = TuiState::default();
        state.timeline.start_tool("shell", "");
        state
            .timeline
            .finish_tool("shell", BLOCKED, ToolStatus::Failed, 12);

        for width in [40usize, 60, 80, 100, 160] {
            open_runs(&mut state);
            let text = rendered(&state, width);
            assert!(
                !text.contains('…'),
                "width {width}: the message was truncated:\n{text}"
            );
            // Every word of the refusal survives, wrapping included.
            for needle in [
                "shell_allowed",
                "zcode.json/zcode.toml",
                "go build",
                "hint:",
            ] {
                let joined = text.replace('\n', " ");
                assert!(
                    joined.contains(needle),
                    "width {width}: {needle:?} missing from:\n{text}"
                );
            }
            // And no row runs past the pane.
            for row in text.lines() {
                assert!(
                    row.chars().count() <= width,
                    "width {width}: row is {} wide: {row:?}",
                    row.chars().count()
                );
            }
        }
    }

    #[test]
    fn a_tool_row_shows_the_duration_the_engine_measured() {
        // Events are drained in batches, so timing the gap between ingesting
        // a start and ingesting its result measures the channel, not the tool
        // — and on a fast burst that reads as `0ms`, i.e. no duration at all.
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolCallStart {
            id: "1".into(),
            name: "shell".into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "1".into(),
            name: "shell".into(),
            content: "b\n".into(),
            error: None,
            elapsed_ms: 1_337,
        });
        assert!(
            rendered(&state, 100).contains("1.3s"),
            "{}",
            rendered(&state, 100)
        );
    }

    fn usage(input: u64, output: u64, cost: Option<f64>) -> domain::LlmFinish {
        domain::LlmFinish {
            reason: domain::LlmFinishReason::ToolUse,
            input_tokens: input,
            output_tokens: output,
            cache_tokens: 0,
            cost_usd: cost,
        }
    }

    #[test]
    fn tokens_move_while_a_turn_is_still_running() {
        // The reported bug: the bar read `0 in / 0 out` for the whole of a
        // multi-step turn, which can be minutes of billed work.
        let mut state = TuiState::default();
        state.apply(UiEvent::Usage(usage(2280, 71, Some(0.000189))));
        assert_eq!(state.totals.input_tokens, 2280);
        assert_eq!(state.totals.output_tokens, 71);
        assert!(state.totals.cost.priced, "a reported cost is a known cost");
    }

    #[test]
    fn running_totals_are_not_counted_once_per_step() {
        // The engine reports the turn's running total, not a per-step delta.
        // Adding each report whole would count step 1 seven times by step 7.
        let mut state = TuiState::default();
        state.apply(UiEvent::Usage(usage(2280, 71, Some(0.000189))));
        state.apply(UiEvent::Usage(usage(11673, 238, Some(0.000697))));
        assert_eq!(state.totals.input_tokens, 11673);
        assert_eq!(state.totals.output_tokens, 238);
        assert!((state.totals.cost.total_usd() - 0.000697).abs() < 1e-9);
    }

    #[test]
    fn settling_a_turn_does_not_double_count_what_it_reported() {
        let mut state = TuiState::default();
        state.apply(UiEvent::Usage(usage(11673, 238, Some(0.000697))));
        let mut settled = result(1, "done");
        settled.input_tokens = 11673;
        settled.output_tokens = 238;
        settled.cost = Cost {
            input_usd: 99.0,
            priced: true,
            ..Default::default()
        };
        state.finish_turn(Ok(settled));
        assert_eq!(state.totals.input_tokens, 11673, "counted once");
        assert_eq!(state.totals.output_tokens, 238);
        // The provider's own figure wins; the estimate is not added on top.
        assert!(
            (state.totals.cost.total_usd() - 0.000697).abs() < 1e-9,
            "got {}",
            state.totals.cost.total_usd()
        );
    }

    #[test]
    fn a_second_turn_starts_from_a_clean_slate() {
        let mut state = TuiState::default();
        state.apply(UiEvent::Usage(usage(100, 10, Some(0.001))));
        let mut settled = result(1, "done");
        settled.input_tokens = 100;
        settled.output_tokens = 10;
        state.finish_turn(Ok(settled));
        state.apply(UiEvent::Usage(usage(50, 5, Some(0.002))));
        assert_eq!(
            state.totals.input_tokens, 150,
            "turn two adds, not replaces"
        );
        assert!((state.totals.cost.total_usd() - 0.003).abs() < 1e-9);
    }

    #[test]
    fn an_unpriced_model_still_reports_its_tokens() {
        // A provider that reports no cost leaves the estimate to the table;
        // the token counts are still real and must show.
        let mut state = TuiState::default();
        state.apply(UiEvent::Usage(usage(500, 20, None)));
        assert_eq!(state.totals.input_tokens, 500);
        assert!(!state.totals.cost.priced);
    }

    #[test]
    fn a_successful_row_shows_what_ran_not_what_came_back() {
        // The reported bug: a shell row read `total 32` — the first line of
        // the output — where it should say which command produced it.
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolCallStart {
            id: "1".into(),
            name: "shell".into(),
        });
        state.apply(UiEvent::ToolCallArgs {
            id: "1".into(),
            arguments: r#"{"command":"ls -lah"}"#.into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "1".into(),
            name: "shell".into(),
            content: "total 32
drwxr-xr-x  ..."
                .into(),
            error: None,
            elapsed_ms: 72,
        });
        open_runs(&mut state);
        let text = rendered(&state, 100);
        assert!(text.contains("ls -lah"), "{text}");
        assert!(!text.contains("total 32"), "{text}");
    }

    #[test]
    fn a_failed_row_shows_the_error_not_the_command() {
        // Once it fails, the error is the thing to act on — and the row is
        // labelled `shell` either way, so nothing is lost.
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolCallStart {
            id: "1".into(),
            name: "shell".into(),
        });
        state.apply(UiEvent::ToolCallArgs {
            id: "1".into(),
            arguments: r#"{"command":"asked"}"#.into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "1".into(),
            name: "shell".into(),
            content: String::new(),
            error: Some("sh: asked: command not found".into()),
            elapsed_ms: 72,
        });
        open_runs(&mut state);
        assert!(rendered(&state, 100).contains("command not found"));
    }

    #[test]
    fn a_tool_that_reports_no_arguments_still_shows_its_result() {
        let mut state = TuiState::default();
        state.timeline.start_tool("read", "");
        state
            .timeline
            .finish_tool("read", "package main", ToolStatus::Ok, 12);
        open_runs(&mut state);
        assert!(rendered(&state, 100).contains("package main"));
    }

    #[test]
    fn the_caret_follows_the_pointer_into_the_conversation() {
        // Clicking the transcript should put the cursor where you clicked, not
        // leave it blinking in the prompt you are not typing into.
        let mut state = pane_state();
        assert_eq!(state.caret_in_conversation(), None, "the prompt has it");

        state.start_selection(10, 4);
        state.extend_selection(30, 6);
        assert_eq!(
            state.caret_in_conversation(),
            Some((30, 6)),
            "the moving end"
        );

        state.clear_selection();
        assert_eq!(
            state.caret_in_conversation(),
            None,
            "and back to the prompt"
        );
    }

    #[test]
    fn typing_takes_the_caret_back_to_the_prompt() {
        let mut state = pane_state();
        state.start_selection(10, 4);
        state.extend_selection(30, 6);

        let (cmd_tx, _rx) = std::sync::mpsc::channel();
        let cancel = CancelFlag::default();
        handle_key(
            &mut state,
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Char('x'),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
            &cancel,
            &cmd_tx,
        );
        assert_eq!(state.caret_in_conversation(), None);
        assert_eq!(state.input.text(), "x");
    }

    // ---- folding a run of tool calls ---------------------------------------

    fn run_of(state: &mut TuiState, calls: &[(&str, &str)], settle: bool) {
        for (id, name) in calls {
            state.apply(UiEvent::ToolCallStart {
                id: (*id).into(),
                name: (*name).into(),
            });
            if settle {
                state.apply(UiEvent::ToolResult {
                    tool_call_id: (*id).into(),
                    name: (*name).into(),
                    content: "ok".into(),
                    error: None,
                    elapsed_ms: 12,
                });
            }
        }
    }

    #[test]
    fn work_in_flight_is_shown_without_being_asked() {
        // Folding a call away while it is still happening would hide the only
        // thing on screen that is changing.
        let mut state = TuiState::default();
        run_of(&mut state, &[("c1", "shell")], false);
        let text = rendered(&state, 100);
        assert!(text.contains("▾ tools used"), "{text}");
        assert!(text.contains("shell"), "{text}");
    }

    #[test]
    fn a_settled_run_folds_itself_away() {
        let mut state = TuiState::default();
        state.timeline.push_agent("I'll look at three things.");
        run_of(
            &mut state,
            &[("c1", "read"), ("c2", "list_dir"), ("c3", "shell")],
            true,
        );
        let text = rendered(&state, 100);
        assert!(text.contains("▸ tools used · 3 calls"), "{text}");
        // The conversation survives; only the calls are folded.
        assert!(text.contains("I'll look at three things."), "{text}");
        assert!(!text.contains("list_dir"), "{text}");
    }

    #[test]
    fn a_folded_header_says_enough_to_decide_whether_to_open_it() {
        let mut state = TuiState::default();
        run_of(&mut state, &[("c1", "read"), ("c2", "shell")], true);
        state.apply(UiEvent::ToolCallStart {
            id: "c3".into(),
            name: "shell".into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "c3".into(),
            name: "shell".into(),
            content: String::new(),
            error: Some("blocked".into()),
            elapsed_ms: 5,
        });
        // A failure keeps the run open, so the message is right there.
        let text = rendered(&state, 100);
        assert!(text.contains("▾ tools used"), "{text}");
        assert!(
            text.contains("blocked"),
            "the failure is not hidden: {text}"
        );

        // Folded by hand, the header still accounts for it.
        let run = state
            .timeline
            .entries()
            .iter()
            .find_map(entry_run)
            .expect("a tool entry");
        state.toggle_run(run);
        let folded = rendered(&state, 100);
        assert!(folded.contains("3 calls"), "{folded}");
        assert!(folded.contains("1 failed"), "{folded}");
    }

    #[test]
    fn clicking_the_header_folds_and_unfolds_that_run() {
        let mut state = pane_state();
        run_of(&mut state, &[("c1", "read"), ("c2", "shell")], true);
        // Draw once so the header's screen row is recorded.
        let (_, _, headers) = render_timeline(&state, 100, None);
        let (index, run) = headers[0];
        state.header_rows = vec![(state.pane.y + 1 + index as u16, run)];
        let header_row = state.pane.y + 1 + index as u16;

        assert!(!rendered(&state, 100).contains("read"), "folded to start");
        // A click that does not move is a click, not a selection.
        state.start_selection(4, header_row);
        state.finish_selection(&mut Vec::new());
        assert!(
            rendered(&state, 100).contains("read"),
            "the click opened it"
        );

        state.header_rows = vec![(header_row, run)];
        state.start_selection(4, header_row);
        state.finish_selection(&mut Vec::new());
        assert!(
            !rendered(&state, 100).contains("read"),
            "and folds it again"
        );
    }

    #[test]
    fn a_drag_over_a_header_selects_rather_than_folding() {
        // Folding on any click would make the header impossible to copy.
        let mut state = pane_state();
        run_of(&mut state, &[("c1", "read")], true);
        state.header_rows = vec![(4, 1)];
        let before = rendered(&state, 100);

        state.start_selection(4, 4);
        state.extend_selection(20, 4);
        state.finish_selection(&mut Vec::new());
        assert_eq!(rendered(&state, 100), before, "the fold did not change");
        assert!(state.run_override.is_empty(), "no run was toggled");
    }

    #[test]
    fn folding_survives_entries_dropping_off_the_front() {
        // Run ids are assigned once, so they cannot shift under the override
        // the way a positional key would.
        let mut state = TuiState::default();
        run_of(&mut state, &[("c1", "read")], true);
        let run = state
            .timeline
            .entries()
            .iter()
            .find_map(entry_run)
            .expect("a tool entry");
        state.toggle_run(run);
        assert!(rendered(&state, 100).contains("read"), "opened");

        for i in 0..timeline::MAX_ENTRIES {
            state.timeline.push_agent(&format!("filler {i}"));
        }
        // The run is gone from the timeline, and the override is harmless.
        assert!(state.run_override.contains_key(&run));
        assert!(!rendered(&state, 100).contains("▸ tools used"));
    }

    #[test]
    fn ctrl_t_folds_everything_then_opens_everything() {
        let mut state = TuiState::default();
        state.timeline.push_agent("one");
        run_of(&mut state, &[("c1", "read")], true);
        state.timeline.push_agent("two");
        run_of(&mut state, &[("c2", "shell")], true);

        let (cmd_tx, _rx) = std::sync::mpsc::channel();
        let cancel = CancelFlag::default();
        let ctrl_t = || {
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Char('t'),
                ratatui::crossterm::event::KeyModifiers::CONTROL,
            )
        };

        // Everything is folded already, so the first press opens.
        handle_key(&mut state, ctrl_t(), &cancel, &cmd_tx);
        let open = rendered(&state, 100);
        assert!(open.contains("read") && open.contains("shell"), "{open}");

        handle_key(&mut state, ctrl_t(), &cancel, &cmd_tx);
        let shut = rendered(&state, 100);
        assert!(!shut.contains("read") && !shut.contains("shell"), "{shut}");
    }

    #[test]
    fn a_short_failure_stays_on_its_row() {
        // Wrapping below costs a line; only pay it when there is no choice.
        let mut state = TuiState::default();
        state.timeline.start_tool("read", "");
        state
            .timeline
            .finish_tool("read", "no such file", ToolStatus::Failed, 12);
        open_runs(&mut state);
        let text = rendered(&state, 100);
        assert_eq!(text.lines().count(), 2, "{text}"); // label + row
        assert!(text.contains("no such file"), "{text}");
    }

    #[test]
    fn a_successful_row_is_still_a_one_line_index() {
        // A tool that returns 4 KB of output must not paste it into the pane.
        let mut state = TuiState::default();
        state.timeline.start_tool("read", "");
        state
            .timeline
            .finish_tool("read", &"x".repeat(1_000), ToolStatus::Ok, 12);
        open_runs(&mut state);
        let text = rendered(&state, 100);
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains('…'), "{text}");
    }

    #[test]
    fn the_counted_height_always_matches_the_rows_built() {
        // The scroll window is placed from the count and drawn from the rows.
        // If they drift, the view jumps or clips — so pin them together across
        // every width and every entry kind.
        let state = busy_timeline();
        for width in [20usize, 40, 60, 80, 100, 160] {
            let (counted, all, _) = render_timeline(&state, width, None);
            assert_eq!(
                counted,
                all.len(),
                "width {width}: counted {counted}, built {}",
                all.len()
            );
        }
    }

    #[test]
    fn a_window_returns_exactly_the_rows_the_full_render_would() {
        // The optimisation must be invisible: windowing is about *when* rows
        // are built, never about which.
        let state = busy_timeline();
        let width = 80;
        let (total, all, _) = render_timeline(&state, width, None);
        let text = |line: &Line| {
            line.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        for skip in 0..total {
            for take in [1usize, 5, 12, total] {
                let (_, window, _) = render_timeline(&state, width, Some((skip, take)));
                let expected: Vec<String> = all.iter().skip(skip).take(take).map(text).collect();
                let got: Vec<String> = window.iter().map(text).collect();
                assert_eq!(got, expected, "skip {skip} take {take}");
            }
        }
    }

    #[test]
    fn a_window_past_the_end_is_empty_not_a_panic() {
        let state = busy_timeline();
        let (total, _, _) = render_timeline(&state, 80, None);
        let (_, rows, _) = render_timeline(&state, 80, Some((total + 100, 10)));
        assert!(rows.is_empty());
    }

    #[test]
    fn scroll_keeps_the_tail_visible() {
        // 100 rows in a 10-row window: show the last 10.
        assert_eq!(tail_offset(100, 10, 0), 90);
        // Nothing to scroll when everything fits.
        assert_eq!(tail_offset(3, 10, 0), 0);
    }

    #[test]
    fn scrollback_moves_the_window_and_cannot_run_off_the_top() {
        assert_eq!(tail_offset(100, 10, 10), 80);
        assert_eq!(tail_offset(100, 10, 500), 0);
    }

    // ---- selection ---------------------------------------------------------

    fn pane_state() -> TuiState {
        let mut state = TuiState::default();
        // A 100x20 pane at the origin: usable cells are 1..=98 by 1..=18.
        state.pane = Rect::new(0, 0, 100, 20);
        state
    }

    #[test]
    fn a_drag_selects_from_where_it_started_to_where_it_is() {
        let mut state = pane_state();
        state.start_selection(10, 4);
        state.extend_selection(30, 6);
        let sel = state.selection.expect("a drag started");
        assert_eq!(sel.ordered(), ((10, 4), (30, 6)));
        assert!(sel.dragging);
    }

    #[test]
    fn dragging_backwards_selects_the_same_span() {
        // Up-and-left is the same words as down-and-right; the caller should
        // not have to know which way the mouse went.
        let mut a = pane_state();
        a.start_selection(30, 6);
        a.extend_selection(10, 4);
        let mut b = pane_state();
        b.start_selection(10, 4);
        b.extend_selection(30, 6);
        assert_eq!(
            a.selection.unwrap().ordered(),
            b.selection.unwrap().ordered()
        );
    }

    #[test]
    fn dragging_off_the_edge_selects_to_the_edge() {
        // Not "stops responding": the pointer leaving the pane is how people
        // select the last line.
        let mut state = pane_state();
        state.start_selection(10, 4);
        state.extend_selection(500, 500);
        let (_, end) = state.selection.unwrap().ordered();
        assert_eq!(end, (98, 18), "clamped inside the border");
    }

    #[test]
    fn a_drag_that_starts_on_the_prompt_is_not_a_selection() {
        // The prompt is below the pane; dragging there is text editing, not
        // transcript selection.
        let mut state = pane_state();
        state.start_selection(10, 25);
        assert!(state.selection.is_none());
    }

    #[test]
    fn a_click_without_a_drag_dismisses_rather_than_copies() {
        let mut state = pane_state();
        state.selected_text = "stale".into();
        state.start_selection(10, 4);
        let mut out: Vec<u8> = Vec::new();
        state.finish_selection(&mut out);
        assert!(state.selection.is_none(), "a bare click clears");
        assert!(
            !state.timeline.entries().iter().any(|e| matches!(
                &e.kind,
                EntryKind::Note { text, .. } if text.contains("copied")
            )),
            "nothing was selected, so nothing was copied"
        );
    }

    #[test]
    fn anything_that_moves_the_rows_drops_the_highlight() {
        // A selection is screen coordinates. Once the rows underneath it move
        // it is pointing at different words, so it must not survive.
        for shift in [
            (|s: &mut TuiState| s.scroll_up(3)) as fn(&mut TuiState),
            |s: &mut TuiState| s.scroll_down(1),
            |s: &mut TuiState| s.finish_turn(Err("boom".into())),
        ] {
            let mut state = pane_state();
            state.max_scroll = 50;
            state.start_selection(10, 4);
            state.extend_selection(30, 6);
            assert!(state.selection.is_some());
            shift(&mut state);
            assert!(state.selection.is_none(), "the highlight outlived its rows");
        }
    }

    #[test]
    fn esc_clears_a_selection_before_it_cancels_a_turn() {
        // Cancelling a running turn because someone wanted to dismiss a
        // highlight would be an expensive misunderstanding.
        let mut state = pane_state();
        state.phase = Phase::Working {
            since: Instant::now(),
            step: 1,
            max: 12,
        };
        state.start_selection(10, 4);
        state.extend_selection(30, 6);

        let (cmd_tx, _rx) = std::sync::mpsc::channel();
        let cancel = CancelFlag::default();
        let key = ratatui::crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            ratatui::crossterm::event::KeyModifiers::NONE,
        );
        handle_key(&mut state, key, &cancel, &cmd_tx);

        assert!(state.selection.is_none());
        assert!(state.busy(), "the turn was left alone");
    }

    #[test]
    fn scrolling_up_stops_at_the_top_instead_of_counting_past_it() {
        // The bug this pins: an unclamped counter keeps climbing after the
        // view has stopped, so the same number of scrolls back down does
        // nothing — which reads exactly like a pane that will not scroll.
        let mut state = TuiState::default();
        state.max_scroll = 12;

        for _ in 0..50 {
            state.scroll_up(WHEEL_ROWS);
        }
        assert_eq!(state.scrollback, 12, "scrolled past the oldest line");

        // One notch back down must move the view one notch, not undo one of
        // fifty invisible ones.
        state.scroll_down(WHEEL_ROWS);
        assert_eq!(state.scrollback, 9);
    }

    #[test]
    fn scrolling_down_settles_on_the_tail() {
        let mut state = TuiState::default();
        state.max_scroll = 40;
        state.scroll_up(PAGE_ROWS);
        assert_eq!(state.scrollback, PAGE_ROWS);
        for _ in 0..10 {
            state.scroll_down(PAGE_ROWS);
        }
        assert_eq!(state.scrollback, 0, "following the tail again");
    }

    #[test]
    fn a_shorter_conversation_pulls_the_view_back_to_what_exists() {
        // `/clear` and a resize both shrink the scrollable range; a stale
        // scrollback would leave the pane parked on rows that are gone.
        let mut state = TuiState::default();
        state.max_scroll = 100;
        state.scroll_up(80);
        state.max_scroll = 5;
        state.scrollback = state.scrollback.min(state.max_scroll);
        assert_eq!(state.scrollback, 5);
    }

    #[test]
    fn scroll_accounts_for_wrapping_not_just_line_count() {
        // One logical line that wraps to five rows must scroll as five.
        let long = "word ".repeat(50);
        let rows = wrap::wrap(&long, 20).len();
        assert!(rows > 5, "expected wrapping, got {rows}");
        assert_eq!(tail_offset(rows, 3, 0), (rows - 3) as u16);
    }

    // ---- the prompt box ----------------------------------------------------

    #[test]
    fn the_prompt_grows_with_its_content_and_stops() {
        let mut state = TuiState::default();
        assert_eq!(input_height(&state, 80), 3);
        state.input.set("a\nb\nc");
        assert_eq!(input_height(&state, 80), 5);
        state.input.set(&"x\n".repeat(50));
        assert_eq!(input_height(&state, 80), MAX_INPUT_ROWS + 2);
    }

    #[test]
    fn the_caret_follows_the_text_across_wrapped_rows() {
        let mut state = TuiState::default();
        state.input.set("0123456789");
        // Width 5: the caret at the end sits on row 1, column 5.
        assert_eq!(cursor_position(&state, 5), (2, 0));
        state.input.set("ab");
        assert_eq!(cursor_position(&state, 5), (0, 2));
    }

    #[test]
    fn the_caret_follows_explicit_newlines() {
        let mut state = TuiState::default();
        state.input.set("one\ntwo");
        assert_eq!(cursor_position(&state, 40), (1, 3));
    }

    #[test]
    fn a_pasted_block_is_kept_whole_in_the_prompt() {
        let mut state = TuiState::default();
        let payload = "fn main() {\n    println!(\"hi\");\n}\n";
        state.input.insert_str(payload);
        assert_eq!(state.input.text(), payload);
    }
}
