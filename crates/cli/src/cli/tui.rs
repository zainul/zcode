//! Interactive terminal UI (FR-IFACE-02/04/05, DQ8).
//!
//! The engine loop is synchronous and blocking (DQ4), so it runs on a
//! dedicated `std::thread` and streams `UiEvent`s back over an `mpsc` channel.
//! The main thread does nothing but render, which keeps the current-thread
//! tokio runtime free and the UI responsive while a turn is in flight.
//!
//! Both panes are bounded (`MAX_LINES`): a runaway tool output cannot grow the
//! process memory without limit (NFR-PERF-03).

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use app::{AgentLoop, ExecutionRequest, ExecutionResult};
use domain::{CancelFlag, UiEvent};
use infra_config::Config;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Terminal, TerminalOptions};

use super::emit::{sanitize, ChannelEmitter};
use super::wire;

/// Upper bound on retained lines per pane.
const MAX_LINES: usize = 500;
const TICK: Duration = Duration::from_millis(50);

/// Restores the terminal even if rendering panics or returns early.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

/// What the renderer knows. Deliberately plain strings: markdown rendering is
/// out of scope for v0.2 (PRD §6 #6).
#[derive(Default)]
pub struct TuiState {
    pub transcript: Vec<String>,
    pub tools: Vec<String>,
    pub input: String,
    /// Text streamed for the in-flight answer, not yet committed.
    pub streaming: String,
    pub busy: bool,
    pub status: String,
}

impl TuiState {
    fn push_transcript(&mut self, line: impl Into<String>) {
        push_bounded(&mut self.transcript, line.into());
    }

    fn push_tool(&mut self, line: impl Into<String>) {
        push_bounded(&mut self.tools, line.into());
    }

    /// Fold one engine event into the view.
    pub fn apply(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Delta(text) => self.streaming.push_str(&sanitize(&text)),
            UiEvent::ToolCallStart { name, .. } => {
                self.push_tool(format!("· {name}"));
            }
            UiEvent::ToolResult {
                name,
                content,
                error,
                ..
            } => match error {
                Some(message) => self.push_tool(format!("  {name}: error: {}", sanitize(&message))),
                None => {
                    let cleaned = sanitize(&content);
                    let first = cleaned.lines().next().unwrap_or_default();
                    let rest = cleaned.lines().count().saturating_sub(1);
                    let suffix = if rest > 0 {
                        format!(" (+{rest} more lines)")
                    } else {
                        String::new()
                    };
                    self.push_tool(format!("  {name}: {first}{suffix}"));
                }
            },
            UiEvent::LoopStart { step, max_turns } => {
                self.status = format!("thinking… step {step}/{max_turns} (Esc to cancel)");
            }
            UiEvent::Error(message) => self.push_tool(format!("! {}", sanitize(&message))),
            UiEvent::LoopEnd { truncated, .. } => {
                if truncated {
                    self.push_tool("! stopped at the turn/token cap".to_string());
                }
            }
            UiEvent::ToolCallArgs { .. } | UiEvent::Finish(_) => {}
        }
    }

    /// Commit the streamed answer to the transcript at the end of a turn.
    pub fn finish_turn(&mut self, outcome: Result<ExecutionResult, String>) {
        let streamed = std::mem::take(&mut self.streaming);
        if !streamed.trim().is_empty() {
            self.push_transcript(format!("zcode: {}", streamed.trim_end()));
        }
        match outcome {
            Ok(result) => {
                if streamed.trim().is_empty() && !result.final_text.trim().is_empty() {
                    self.push_transcript(format!("zcode: {}", result.final_text.trim_end()));
                }
                self.status = format!(
                    "ready · {} step(s) · {} in / {} out tokens · session {}",
                    result.steps, result.input_tokens, result.output_tokens, result.session_id
                );
            }
            Err(message) => {
                self.push_transcript(format!("zcode: [{message}]"));
                self.status = "ready".into();
            }
        }
        self.busy = false;
    }
}

fn push_bounded(buffer: &mut Vec<String>, line: String) {
    if buffer.len() >= MAX_LINES {
        buffer.remove(0);
    }
    buffer.push(line);
}

/// Messages the renderer sends to the engine thread.
enum Command {
    Run(String),
}

/// Launch the interactive UI. Returns once the user quits; the worker thread
/// is asked to stop and joined so no MCP/LSP child outlives the process
/// (NFR-REL-04).
pub fn run_tui(
    cfg: Config,
    cancel: CancelFlag,
    session_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>();
    let (done_tx, done_rx) = mpsc::channel::<Result<ExecutionResult, String>>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

    let worker_cfg = cfg.clone();
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || {
        engine_thread(
            worker_cfg,
            worker_cancel,
            session_id,
            ui_tx,
            done_tx,
            cmd_rx,
        );
    });

    let result = render_loop(&cfg, &cancel, &ui_rx, &done_rx, &cmd_tx);

    // Dropping the command sender ends the worker's recv loop.
    drop(cmd_tx);
    cancel.trigger();
    let _ = worker.join();
    result
}

/// Owns the `App` for the whole session so MCP/LSP servers are started once
/// and reused across turns.
fn engine_thread(
    cfg: Config,
    cancel: CancelFlag,
    resume: Option<String>,
    ui_tx: Sender<UiEvent>,
    done_tx: Sender<Result<ExecutionResult, String>>,
    cmd_rx: Receiver<Command>,
) {
    // Telemetry goes to a sink, never stdout: the alternate screen owns it.
    let mut app = match wire(&cfg, Box::new(io::sink())) {
        Ok(app) => app,
        Err(e) => {
            let _ = done_tx.send(Err(e.to_string()));
            return;
        }
    };
    app.set_emitter(Box::new(ChannelEmitter(ui_tx)));
    app.set_cancel(cancel.clone());

    let ctx = cfg.to_agent_context();
    // One session spans the whole REPL, so context carries across turns.
    let mut session_id: Option<String> = resume;

    while let Ok(Command::Run(prompt)) = cmd_rx.recv() {
        let mut req = ExecutionRequest::new(prompt);
        req.mode = cfg.mode;
        req.session_id = session_id.clone();
        req.max_turns = cfg.max_turns;
        req.max_tokens = cfg.max_tokens;
        req.max_tool_output_chars = cfg.max_tool_output_chars;

        let outcome = app.execute(&ctx, req);
        // A cancelled turn must not poison the next one.
        cancel.reset();
        match outcome {
            Ok(result) => {
                session_id = Some(result.session_id.clone());
                let _ = done_tx.send(Ok(result));
            }
            Err(e) => {
                let _ = done_tx.send(Err(e.to_string()));
            }
        }
    }
}

fn render_loop(
    cfg: &Config,
    cancel: &CancelFlag,
    ui_rx: &Receiver<UiEvent>,
    done_rx: &Receiver<Result<ExecutionResult, String>>,
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
        status: format!(
            "ready · {} / {} · {} mode · Enter to send, Esc to cancel, Ctrl-C to quit",
            cfg.provider.as_str(),
            cfg.model,
            cfg.mode.as_str()
        ),
        ..Default::default()
    };
    state.push_transcript("zcode: ready when you are.".to_string());

    loop {
        // Drain everything the engine produced since the last frame.
        while let Ok(ev) = ui_rx.try_recv() {
            state.apply(ev);
        }
        if let Ok(outcome) = done_rx.try_recv() {
            state.finish_turn(outcome);
        }

        terminal.draw(|frame| draw(frame, &state))?;

        if !event::poll(TICK)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows reports both press and release; act on press only.
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => break,
            KeyCode::Esc => {
                if state.busy {
                    // Cancel the turn in flight, keep the session open.
                    cancel.trigger();
                    state.status = "cancelling…".into();
                } else {
                    break;
                }
            }
            // `q` quits only when it would not swallow typed input.
            KeyCode::Char('q') if state.input.is_empty() && !state.busy => break,
            KeyCode::Enter => {
                let prompt = state.input.trim().to_string();
                if prompt.is_empty() || state.busy {
                    continue;
                }
                state.input.clear();
                state.push_transcript(format!("you: {prompt}"));
                state.busy = true;
                state.status = "thinking…".into();
                if cmd_tx.send(Command::Run(prompt)).is_err() {
                    break;
                }
            }
            KeyCode::Backspace => {
                state.input.pop();
            }
            KeyCode::Char(c) => state.input.push(c),
            _ => {}
        }
    }

    Ok(())
}

fn draw(frame: &mut ratatui::Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(60),
            Constraint::Percentage(30),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let mut conversation: Vec<Line> = state
        .transcript
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();
    if !state.streaming.is_empty() {
        conversation.push(Line::from(format!("zcode: {}", state.streaming)));
    }
    let messages = Paragraph::new(conversation)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(&state.transcript, chunks[0].height), 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" conversation "),
        );
    frame.render_widget(messages, chunks[0]);

    let tools = Paragraph::new(
        state
            .tools
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((scroll_offset(&state.tools, chunks[1].height), 0))
    .block(Block::default().borders(Borders::ALL).title(" tools "));
    frame.render_widget(tools, chunks[1]);

    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(state.input.as_str()),
    ]);
    let input = Paragraph::new(prompt).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", state.status)),
    );
    frame.render_widget(input, chunks[2]);
}

/// Keep the newest lines visible without a scrollback widget.
fn scroll_offset(lines: &[String], height: u16) -> u16 {
    let visible = height.saturating_sub(2) as usize; // borders
    lines.len().saturating_sub(visible) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{LlmFinishReason, UiEvent};

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
        }
    }

    #[test]
    fn deltas_accumulate_then_commit_on_finish() {
        let mut state = TuiState::default();
        state.apply(UiEvent::Delta("hel".into()));
        state.apply(UiEvent::Delta("lo".into()));
        assert_eq!(state.streaming, "hello");

        state.busy = true;
        state.finish_turn(Ok(result(1, "hello")));
        assert!(state.streaming.is_empty());
        assert_eq!(state.transcript, vec!["zcode: hello".to_string()]);
        assert!(!state.busy);
        assert!(state.status.contains("1 step(s)"));
    }

    #[test]
    fn non_streaming_answer_still_lands_in_the_transcript() {
        let mut state = TuiState::default();
        state.finish_turn(Ok(result(1, "final only")));
        assert_eq!(state.transcript, vec!["zcode: final only".to_string()]);
    }

    #[test]
    fn engine_errors_are_shown_not_swallowed() {
        let mut state = TuiState {
            busy: true,
            ..Default::default()
        };
        state.finish_turn(Err("interrupted".into()));
        assert!(state.transcript[0].contains("interrupted"));
        assert!(!state.busy);
    }

    #[test]
    fn tool_events_land_in_the_tool_pane() {
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolCallStart {
            id: "c1".into(),
            name: "read".into(),
        });
        state.apply(UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "read".into(),
            content: "a\nb\nc".into(),
            error: None,
        });
        assert_eq!(state.tools[0], "· read");
        assert!(state.tools[1].contains("(+2 more lines)"));
        // The conversation pane stays clean.
        assert!(state.transcript.is_empty());
    }

    #[test]
    fn tool_output_escapes_are_stripped_before_rendering() {
        let mut state = TuiState::default();
        state.apply(UiEvent::ToolResult {
            tool_call_id: "c1".into(),
            name: "shell".into(),
            content: "\u{1b}[2Jwiped".into(),
            error: None,
        });
        assert!(!state.tools[0].contains('\u{1b}'));
        // Streamed model text is sanitised too.
        state.apply(UiEvent::Delta("\u{1b}[31mred".into()));
        assert!(!state.streaming.contains('\u{1b}'));
    }

    #[test]
    fn panes_are_bounded() {
        let mut state = TuiState::default();
        for i in 0..(MAX_LINES + 50) {
            state.push_tool(format!("line {i}"));
        }
        assert_eq!(state.tools.len(), MAX_LINES);
        // The oldest lines are the ones dropped.
        assert_eq!(state.tools[0], format!("line {}", 50));
    }

    #[test]
    fn loop_start_reports_progress() {
        let mut state = TuiState::default();
        state.apply(UiEvent::LoopStart {
            step: 2,
            max_turns: 20,
        });
        assert!(state.status.contains("step 2/20"));
    }

    #[test]
    fn truncation_is_surfaced() {
        let mut state = TuiState::default();
        state.apply(UiEvent::LoopEnd {
            steps: 20,
            finish_reason: LlmFinishReason::Length,
            truncated: true,
        });
        assert!(state.tools[0].contains("cap"));
    }

    #[test]
    fn scroll_keeps_the_tail_visible() {
        let lines: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        // 12 rows of chrome-inclusive height => 10 visible => offset 90.
        assert_eq!(scroll_offset(&lines, 12), 90);
        // Nothing to scroll when everything fits.
        assert_eq!(scroll_offset(&lines[..3], 12), 0);
    }
}
