//! Application layer — the agent orchestration loop (FR-LOOP-01..04).
//!
//! Depends on `domain` only (FR-DI-02): every capability arrives as a port
//! trait object, so this crate has no idea whether it is talking to OpenAI or
//! a fake, to a real filesystem or a temp dir. The loop is **synchronous**
//! (DQ4) — the TUI runs it on a worker thread, headless `zcode run` runs it
//! inline — which is what keeps `domain`/`app` free of an async runtime.

use std::time::Instant;

use domain::{
    modes, AgentContext, AgentMode, CancelFlag, Emitter, ExtraField, ImageRef, LlmEvent, LlmFinish,
    LlmFinishReason, LlmMessage, LlmPort, LlmRequest, LlmRole, LlmToolCall, LlmToolResult,
    LogLevel, LoggerPort, Session, SessionStorePort, TelemetryEvent, TelemetryPort,
    TelemetryTotals, ToolRegistryPort, ToolSpec, UiEvent,
};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("port resolution failed: {0}")]
    Port(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("llm error: {0}")]
    Llm(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("session error: {0}")]
    Session(String),
    /// Ctrl-C / `CancelFlag`: the partial session is checkpointed first.
    #[error("interrupted")]
    Interrupted,
    #[error("timed out after {0}ms")]
    Timeout(u64),
    #[error("{0}")]
    Domain(#[from] domain::DomainError),
}

/// One agent run: a prompt plus the caps it must respect.
#[derive(Clone, Debug)]
pub struct ExecutionRequest {
    pub prompt: String,
    pub mode: AgentMode,
    /// Resume an existing session, or `None` to start a new one.
    pub session_id: Option<String>,
    pub images: Box<[ImageRef]>,
    pub max_turns: u64,
    pub max_tokens: u64,
    pub max_tool_output_chars: usize,
    pub temperature: f32,
    /// Wall-clock cap for the whole loop (FR-IFACE-05).
    pub timeout_ms: Option<u64>,
}

impl ExecutionRequest {
    /// A request carrying the PRD default caps (FR-LOOP-02/03/04).
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            mode: AgentMode::default(),
            session_id: None,
            images: Box::new([]),
            max_turns: 20,
            max_tokens: 16_384,
            max_tool_output_chars: 16_000,
            temperature: 0.0,
            timeout_ms: None,
        }
    }
}

/// What a run produced.
#[derive(Clone, Debug)]
pub struct ExecutionResult {
    pub session_id: String,
    pub final_text: String,
    pub steps: u64,
    pub finish_reason: LlmFinishReason,
    /// True when a cap (turns or tokens) stopped the loop before the model
    /// produced a final answer.
    pub truncated: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
    /// Estimated spend for this run. `priced` is false when the model is not
    /// in the price table, so the UI can say "n/a" rather than "$0.00".
    pub cost: domain::Cost,
}

/// The engine contract both interfaces drive (FR-IFACE-03).
pub trait AgentLoop {
    fn execute(
        &mut self,
        ctx: &AgentContext,
        req: ExecutionRequest,
    ) -> Result<ExecutionResult, AppError>;
}

/// Drops every UI event: the default for headless runs, where the telemetry
/// port is the output channel.
pub struct NullEmitter;

impl Emitter for NullEmitter {
    fn emit(&mut self, _ev: UiEvent) {}
}

/// Discards log records. Used when no logger is wired.
pub struct NullLogger;

impl LoggerPort for NullLogger {
    fn log(&self, _level: LogLevel, _msg: &str) {}
    fn with_field(&self, _key: &str, _value: &str) -> Box<dyn LoggerPort + Send + Sync> {
        Box::new(NullLogger)
    }
}

/// The orchestrator. Owns its ports outright (`Box<dyn … + Send>`, DQ5): the
/// loop needs `&mut` access to the LLM and the tool registry, and the whole
/// `App` moves onto a worker thread in the TUI.
pub struct App {
    llm: Box<dyn LlmPort + Send>,
    tools: Box<dyn ToolRegistryPort + Send>,
    sessions: Box<dyn SessionStorePort + Send>,
    telemetry: Box<dyn TelemetryPort + Send>,
    /// Rates behind the cost figure the UI shows. Defaults to the built-in
    /// table; the CLI replaces it with one carrying the config's overrides.
    pricing: domain::PriceTable,
    logger: Box<dyn LoggerPort + Send>,
    emitter: Box<dyn Emitter + Send>,
    cancel: CancelFlag,
}

impl App {
    pub fn new(
        llm: Box<dyn LlmPort + Send>,
        tools: Box<dyn ToolRegistryPort + Send>,
        sessions: Box<dyn SessionStorePort + Send>,
        telemetry: Box<dyn TelemetryPort + Send>,
        logger: Box<dyn LoggerPort + Send>,
    ) -> Self {
        Self {
            llm,
            tools,
            sessions,
            telemetry,
            pricing: domain::PriceTable::builtin(),
            logger,
            emitter: Box::new(NullEmitter),
            cancel: CancelFlag::default(),
        }
    }

    /// Point the loop at a different provider client.
    ///
    /// Only the client is replaced: the tool registry, and with it every MCP
    /// and LSP child process, is left running. Restarting those to change
    /// endpoint would cost seconds and lose their warm state, and none of them
    /// has anything to do with which model is answering.
    ///
    /// The session is untouched too, so the transcript carries across the
    /// switch — which is the point of switching mid-conversation.
    pub fn set_llm(&mut self, llm: Box<dyn LlmPort + Send>) {
        self.llm = llm;
    }

    /// Replace the price table, e.g. with the config's `[[pricing]]`
    /// overrides layered ahead of the built-ins.
    pub fn set_pricing(&mut self, pricing: domain::PriceTable) {
        self.pricing = pricing;
    }

    pub fn with_pricing(mut self, pricing: domain::PriceTable) -> Self {
        self.set_pricing(pricing);
        self
    }

    /// Attach a rendering sink (the TUI's channel bridge, or a pretty stdout
    /// printer). Keeps `app` dep-free while letting the interface render.
    pub fn set_emitter(&mut self, emitter: Box<dyn Emitter + Send>) {
        self.emitter = emitter;
    }

    pub fn with_emitter(mut self, emitter: Box<dyn Emitter + Send>) -> Self {
        self.set_emitter(emitter);
        self
    }

    /// Share the flag the CLI's SIGINT handler flips (FR-IFACE-05).
    pub fn set_cancel(&mut self, cancel: CancelFlag) {
        self.cancel = cancel;
    }

    pub fn with_cancel(mut self, cancel: CancelFlag) -> Self {
        self.set_cancel(cancel);
        self
    }

    /// The tools the model may see this turn. Anything the mode would refuse
    /// is withheld entirely, so the model is never even tempted to call it
    /// (FR-MODE-01). The filter and the dispatch gate share
    /// `modes::denies`, so the list shown can never disagree with the list
    /// allowed.
    pub fn tool_specs_for(&self, mode: AgentMode) -> Box<[ToolSpec]> {
        let all = self.tools.list();
        if mode == AgentMode::Auto {
            return all;
        }
        all.into_vec()
            .into_iter()
            .filter(|spec| !modes::denies(mode, &spec.name))
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    /// Enumerate every tool, unfiltered (`zcode tools list`).
    pub fn tool_specs(&self) -> Box<[ToolSpec]> {
        self.tools.list()
    }

    /// Direct access to the session store for the `zcode session …` subcommands.
    pub fn sessions_mut(&mut self) -> &mut (dyn SessionStorePort + Send) {
        self.sessions.as_mut()
    }

    fn open_session(
        &mut self,
        req: &ExecutionRequest,
        ctx: &AgentContext,
    ) -> Result<Session, AppError> {
        let id = match &req.session_id {
            Some(id) => id.clone(),
            None => self
                .sessions
                .create()
                .map_err(|e| AppError::Session(e.to_string()))?,
        };
        let mut session = self
            .sessions
            .load(&id)
            .map_err(|e| AppError::Session(format!("cannot load session {id}: {e}")))?;
        // Mode and model are per-run properties recorded on the session
        // (FR-MODE-04, FR-SESSION-07).
        session.mode = req.mode;
        session.model = ctx.model.clone();
        Ok(session)
    }

    /// Persist the transcript without deep-copying it: the history vector is
    /// moved into the session for the write and moved straight back out
    /// (FR-SESSION-06; keeps the 20-step memory ceiling, NFR-PERF-03).
    fn checkpoint(
        &mut self,
        session: &mut Session,
        history: &mut Vec<LlmMessage>,
        steps: u64,
    ) -> Result<(), AppError> {
        session.step_count = steps;
        session.messages = std::mem::take(history).into_boxed_slice();
        let outcome = self.sessions.checkpoint(&session.id, session);
        *history = std::mem::replace(&mut session.messages, Box::new([])).into_vec();
        outcome.map_err(|e| AppError::Session(e.to_string()))
    }

    fn emit_telemetry(
        &mut self,
        kind: &str,
        session: &Session,
        steps: u64,
        extra: Vec<(String, ExtraField)>,
    ) {
        self.telemetry.emit(TelemetryEvent {
            kind: kind.to_string(),
            model: session.model.clone(),
            input_tokens: 0,
            output_tokens: 0,
            cache_tokens: 0,
            steps,
            execution_time_ms: 0,
            session_id: session.id.clone(),
            extra: extra.into_boxed_slice(),
        });
    }

    /// Flush the run report and hand back the result. Called on the happy
    /// path *and* on abort, so a killed run still leaves telemetry behind.
    #[allow(clippy::too_many_arguments)]
    fn finish_run(
        &mut self,
        session: &Session,
        steps: u64,
        reason: LlmFinishReason,
        truncated: bool,
        stop_cause: Option<&'static str>,
        totals: (u64, u64, u64),
        reported_cost_usd: Option<f64>,
        started: Instant,
    ) {
        let (input_tokens, output_tokens, cache_tokens) = totals;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        // What the provider charged beats what the table guesses: it covers
        // models the table has never seen, which is the case that otherwise
        // reports `n/a` while real money is being spent.
        let cost = match reported_cost_usd {
            Some(reported) => domain::Cost::from_reported_usd(reported),
            None => {
                self.pricing
                    .estimate(&session.model, input_tokens, output_tokens, cache_tokens)
            }
        };
        self.telemetry.emit(TelemetryEvent {
            kind: "finish".into(),
            model: session.model.clone(),
            input_tokens,
            output_tokens,
            cache_tokens,
            steps,
            execution_time_ms: elapsed_ms,
            session_id: session.id.clone(),
            extra: Box::new([
                ("reason".into(), ExtraField::Text(reason_str(reason).into())),
                ("truncated".into(), ExtraField::Bool(truncated)),
                // Distinguishes *why* the run stopped early: `truncated` alone
                // cannot tell a consumer whether a turn cap, the model's own
                // token budget, a request timeout, or a user cancellation was
                // responsible, and those need different, honest messages
                // (opencode's translation used to report all four as "stopped
                // at the turn or token cap", which is simply wrong for a
                // timeout or a cancellation).
                (
                    "stop_cause".into(),
                    match stop_cause {
                        Some(cause) => ExtraField::Text(cause.into()),
                        None => ExtraField::Null,
                    },
                ),
                (
                    "mode".into(),
                    ExtraField::Text(session.mode.as_str().into()),
                ),
                (
                    "cost_usd".into(),
                    match cost.priced {
                        true => ExtraField::Number(cost.total_usd()),
                        false => ExtraField::Null,
                    },
                ),
            ]),
        });
        let totals = TelemetryTotals {
            model: session.model.clone(),
            input_tokens,
            output_tokens,
            cache_tokens,
            steps,
            execution_time_ms: elapsed_ms,
            session_id: session.id.clone(),
            finish_reason: reason_str(reason).into(),
            truncated,
            cost_usd: cost.priced.then(|| cost.total_usd()),
        };
        if let Err(e) = self.telemetry.flush_report(&session.id, totals) {
            self.logger
                .log(LogLevel::Warn, &format!("could not write report: {e}"));
        }
    }
}

impl AgentLoop for App {
    fn execute(
        &mut self,
        ctx: &AgentContext,
        req: ExecutionRequest,
    ) -> Result<ExecutionResult, AppError> {
        let started = Instant::now();
        let mut session = self.open_session(&req, ctx)?;
        let mut history: Vec<LlmMessage> =
            std::mem::replace(&mut session.messages, Box::new([])).into_vec();

        // The system prompt encodes the mode policy (FR-MODE-03). A resumed
        // session gets its prompt rewritten so a mode switch takes effect.
        let system = LlmMessage::system(modes::system_prompt(req.mode));
        match history.first_mut() {
            Some(first) if first.role == LlmRole::System => *first = system,
            _ => history.insert(0, system),
        }
        history.push(LlmMessage::user(&req.prompt));

        let specs = self.tool_specs_for(req.mode);
        let mut steps: u64 = 0;
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut cache_tokens: u64 = 0;
        // Summed across the turn's calls, when the provider reports it. `None`
        // means no call reported one, so the local price table is the only
        // estimate available.
        let mut reported_cost_usd: Option<f64> = None;
        let mut final_text = String::new();
        let finish_reason;
        let mut truncated = false;
        // Why the run stopped early, for a consumer that needs the real cause
        // rather than a guess (see `finish_run`). `None` until one of the
        // early-exit or cap branches below sets it.
        let mut stop_cause: Option<&'static str> = None;
        // Images ride along with the first turn only; re-sending them every
        // turn would re-bill the vision tokens (FR-MODEL-08).
        let mut pending_images = req.images.clone();

        loop {
            if self.cancel.triggered() {
                self.checkpoint(&mut session, &mut history, steps)?;
                self.finish_run(
                    &session,
                    steps,
                    LlmFinishReason::Stop,
                    true,
                    Some("cancelled"),
                    (input_tokens, output_tokens, cache_tokens),
                    reported_cost_usd,
                    started,
                );
                return Err(AppError::Interrupted);
            }
            if let Some(limit) = req.timeout_ms {
                if started.elapsed().as_millis() as u64 >= limit {
                    self.checkpoint(&mut session, &mut history, steps)?;
                    self.finish_run(
                        &session,
                        steps,
                        LlmFinishReason::Length,
                        true,
                        Some("timeout"),
                        (input_tokens, output_tokens, cache_tokens),
                        reported_cost_usd,
                        started,
                    );
                    return Err(AppError::Timeout(limit));
                }
            }
            if steps >= req.max_turns {
                // FR-LOOP-02: the turn cap stops the loop and says so.
                truncated = true;
                stop_cause = Some("turn_cap");
                finish_reason = LlmFinishReason::Length;
                break;
            }

            self.emitter.emit(UiEvent::LoopStart {
                step: steps + 1,
                max_turns: req.max_turns,
            });
            self.emit_telemetry(
                "loop_start",
                &session,
                steps + 1,
                vec![("mode".into(), ExtraField::Text(req.mode.as_str().into()))],
            );

            let llm_request = LlmRequest {
                messages: history.clone().into_boxed_slice(),
                tools: specs.clone(),
                model: session.model.clone(),
                max_tokens: req.max_tokens,
                temperature: req.temperature,
                images: std::mem::replace(&mut pending_images, Box::new([])),
            };

            let mut assistant = LlmMessage::assistant("");
            let mut tool_calls: Vec<LlmToolCall> = Vec::new();
            let mut finish: Option<LlmFinish> = None;

            for event in self.llm.stream(&llm_request) {
                let event = event.map_err(|e| AppError::Llm(e.to_string()))?;
                match event {
                    LlmEvent::Delta(text) => {
                        assistant.append_content(&text);
                        self.emitter.emit(UiEvent::Delta(text.clone()));
                        self.emit_telemetry(
                            "llm_delta",
                            &session,
                            steps + 1,
                            vec![("text".into(), ExtraField::Text(text))],
                        );
                    }
                    LlmEvent::ToolCallStart { id, name } => {
                        self.emitter.emit(UiEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                        });
                        tool_calls.push(LlmToolCall {
                            id,
                            name,
                            arguments: String::new(),
                        });
                    }
                    LlmEvent::ToolCallArgs { id, arguments } => {
                        self.emitter.emit(UiEvent::ToolCallArgs {
                            id: id.clone(),
                            arguments: arguments.clone(),
                        });
                        match tool_calls.iter_mut().find(|c| c.id == id) {
                            Some(call) => call.arguments.push_str(&arguments),
                            // Arguments before a start event: keep them rather
                            // than dropping the call on the floor.
                            None => tool_calls.push(LlmToolCall {
                                id,
                                name: String::new(),
                                arguments,
                            }),
                        }
                    }
                    LlmEvent::Retry(notice) => {
                        // A rate-limited turn must look rate-limited rather
                        // than hung; the client has already waited by now.
                        //
                        // Reported through the emitter and telemetry only, not
                        // the logger: in the TUI the log stream is rendered
                        // into the same pane, so logging here would print every
                        // retry twice.
                        self.emit_telemetry(
                            "llm_retry",
                            &session,
                            steps + 1,
                            vec![
                                ("attempt".into(), ExtraField::Number(notice.attempt as f64)),
                                (
                                    "max_attempts".into(),
                                    ExtraField::Number(notice.max_attempts as f64),
                                ),
                                (
                                    "delay_ms".into(),
                                    ExtraField::Number(notice.delay_ms as f64),
                                ),
                                (
                                    "status".into(),
                                    match notice.status {
                                        Some(code) => ExtraField::Number(code as f64),
                                        None => ExtraField::Null,
                                    },
                                ),
                                ("reason".into(), ExtraField::Text(notice.reason.clone())),
                            ],
                        );
                        self.emitter.emit(UiEvent::Retry(notice));
                    }
                    LlmEvent::Finish(f) => {
                        finish = Some(f);
                        break;
                    }
                }
            }

            steps += 1;
            let finish = finish.unwrap_or(LlmFinish {
                reason: if tool_calls.is_empty() {
                    LlmFinishReason::Stop
                } else {
                    LlmFinishReason::ToolUse
                },
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
                cost_usd: None,
            });

            // Provider-reported usage is authoritative; the heuristic is only
            // a fallback for providers that omit it (DQ2).
            input_tokens += if finish.input_tokens > 0 {
                finish.input_tokens
            } else {
                history
                    .iter()
                    .map(|m| domain::tokens::estimate_tokens(&m.content))
                    .sum()
            };
            output_tokens += if finish.output_tokens > 0 {
                finish.output_tokens
            } else {
                domain::tokens::estimate_tokens(&assistant.content)
            };
            cache_tokens += finish.cache_tokens;
            if let Some(step_cost) = finish.cost_usd {
                reported_cost_usd = Some(reported_cost_usd.unwrap_or(0.0) + step_cost);
            }

            // Report usage after *every* call, not only the one that ends the
            // turn. A tool-using turn can run for minutes across many steps,
            // and each one has already been billed — showing `0 in / 0 out`
            // until it finishes tells the user nothing about a cost they are
            // already incurring.
            self.emitter.emit(UiEvent::Usage(LlmFinish {
                reason: finish.reason,
                input_tokens,
                output_tokens,
                cache_tokens,
                cost_usd: reported_cost_usd,
            }));

            // Some providers report `Stop` while still emitting tool calls;
            // trust the calls over the label.
            let wants_tools = !tool_calls.is_empty();
            assistant.tool_calls = tool_calls.clone().into_boxed_slice();
            history.push(assistant.clone());

            if !wants_tools {
                // Only a turn that ends the run decides the reported reason;
                // a tool round is never the final word.
                finish_reason = finish.reason;
                final_text = assistant.content;
                if finish.reason == LlmFinishReason::Length {
                    truncated = true;
                    stop_cause = Some("token_cap");
                }
                self.emitter.emit(UiEvent::Finish(finish));
                self.checkpoint(&mut session, &mut history, steps)?;
                break;
            }

            for call in &tool_calls {
                // FR-MODE-01: refuse tools the mode does not grant. The check
                // is by canonical name, so no alias slips past, and it uses
                // the same predicate as the spec filter above.
                if modes::denies(req.mode, &call.name) {
                    let message = format!(
                        "tool `{}` denied: {}",
                        call.name,
                        modes::denial_reason(req.mode)
                    );
                    self.emitter.emit(UiEvent::Error(message.clone()));
                    self.emit_telemetry(
                        "tool_denied",
                        &session,
                        steps,
                        vec![
                            ("tool".into(), ExtraField::Text(call.name.clone())),
                            ("tool_call_id".into(), ExtraField::Text(call.id.clone())),
                            (
                                "reason".into(),
                                ExtraField::Text(format!("{}_mode", req.mode.as_str())),
                            ),
                        ],
                    );
                    self.checkpoint(&mut session, &mut history, steps)?;
                    self.finish_run(
                        &session,
                        steps,
                        LlmFinishReason::Stop,
                        false,
                        None,
                        (input_tokens, output_tokens, cache_tokens),
                        reported_cost_usd,
                        started,
                    );
                    return Err(AppError::Tool(message));
                }

                self.emit_telemetry(
                    "tool_call",
                    &session,
                    steps,
                    vec![
                        ("tool".into(), ExtraField::Text(call.name.clone())),
                        ("tool_call_id".into(), ExtraField::Text(call.id.clone())),
                        ("arguments".into(), ExtraField::Text(call.arguments.clone())),
                    ],
                );

                let call_started = std::time::Instant::now();
                let outcome = self.tools.call(&call.name, &call.arguments);
                let elapsed_ms = call_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                let (content, error) = match outcome {
                    Ok(result) => (result.content, result.error),
                    // A registry-level failure is reported to the model as a
                    // tool error so the loop can continue (NFR-REL-01).
                    Err(e) => (String::new(), Some(e.to_string())),
                };
                let payload = match &error {
                    Some(message) => format!("error: {message}"),
                    None => content.clone(),
                };
                // FR-LOOP-04: cap the result *before* it enters the history so
                // the transcript can never balloon past the configured budget.
                let (payload, was_truncated) =
                    truncate_tool_output(payload, req.max_tool_output_chars);

                self.emitter.emit(UiEvent::ToolResult {
                    tool_call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: payload.clone(),
                    error: error.clone(),
                    elapsed_ms,
                });
                self.emit_telemetry(
                    "tool_result",
                    &session,
                    steps,
                    vec![
                        ("tool".into(), ExtraField::Text(call.name.clone())),
                        ("tool_call_id".into(), ExtraField::Text(call.id.clone())),
                        (
                            "error".into(),
                            match &error {
                                Some(message) => ExtraField::Text(message.clone()),
                                None => ExtraField::Null,
                            },
                        ),
                        ("truncated".into(), ExtraField::Bool(was_truncated)),
                        ("duration_ms".into(), ExtraField::Number(elapsed_ms as f64)),
                        ("output".into(), ExtraField::Text(payload.clone())),
                    ],
                );

                history.push(LlmMessage::tool_result_message(LlmToolResult {
                    tool_call_id: call.id.clone(),
                    content: payload,
                }));
            }

            // FR-SESSION-06: a crash after this point resumes from here.
            self.checkpoint(&mut session, &mut history, steps)?;
        }

        if truncated && final_text.is_empty() {
            self.checkpoint(&mut session, &mut history, steps)?;
        }

        self.emitter.emit(UiEvent::LoopEnd {
            steps,
            finish_reason,
            truncated,
        });
        self.finish_run(
            &session,
            steps,
            finish_reason,
            truncated,
            stop_cause,
            (input_tokens, output_tokens, cache_tokens),
            reported_cost_usd,
            started,
        );

        let cost = match reported_cost_usd {
            Some(reported) => domain::Cost::from_reported_usd(reported),
            None => {
                self.pricing
                    .estimate(&session.model, input_tokens, output_tokens, cache_tokens)
            }
        };
        Ok(ExecutionResult {
            session_id: session.id,
            final_text,
            steps,
            finish_reason,
            truncated,
            input_tokens,
            output_tokens,
            cache_tokens,
            cost,
        })
    }
}

fn reason_str(reason: LlmFinishReason) -> &'static str {
    match reason {
        LlmFinishReason::Stop => "stop",
        LlmFinishReason::ToolUse => "tool_use",
        LlmFinishReason::Length => "length",
    }
}

/// Trim a tool result to `max_chars`, respecting UTF-8 boundaries, and say
/// whether anything was dropped (FR-LOOP-04).
pub fn truncate_tool_output(content: String, max_chars: usize) -> (String, bool) {
    if max_chars == 0 || content.chars().count() <= max_chars {
        return (content, false);
    }
    let mut out: String = content.chars().take(max_chars).collect();
    out.push_str("\n...[truncated]");
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use domain::{BoxError, LlmResponse, ToolResult, ToolSpec};

    /// Replays a canned script of events, one `Vec` per turn.
    struct FakeLlm {
        turns: Vec<Vec<LlmEvent>>,
        calls: usize,
        seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl FakeLlm {
        fn new(turns: Vec<Vec<LlmEvent>>) -> Self {
            Self {
                turns,
                calls: 0,
                seen_tools: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl LlmPort for FakeLlm {
        fn send(&mut self, _req: &LlmRequest) -> Result<LlmResponse, BoxError> {
            unimplemented!("the loop only streams")
        }

        fn stream(
            &mut self,
            req: &LlmRequest,
        ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
            self.seen_tools
                .lock()
                .unwrap()
                .push(req.tools.iter().map(|t| t.name.clone()).collect());
            // Past the script, keep answering with a plain stop so cap tests
            // terminate on the cap rather than on running out of script.
            let events = self
                .turns
                .get(self.calls)
                .cloned()
                .unwrap_or_else(|| vec![LlmEvent::Finish(finish(LlmFinishReason::Stop))]);
            self.calls += 1;
            Box::new(events.into_iter().map(Ok))
        }
    }

    fn finish(reason: LlmFinishReason) -> LlmFinish {
        LlmFinish {
            reason,
            input_tokens: 10,
            output_tokens: 5,
            cache_tokens: 1,
            cost_usd: None,
        }
    }

    fn tool_use_turn(id: &str, name: &str, args: &str) -> Vec<LlmEvent> {
        vec![
            LlmEvent::ToolCallStart {
                id: id.into(),
                name: name.into(),
            },
            LlmEvent::ToolCallArgs {
                id: id.into(),
                arguments: args.into(),
            },
            LlmEvent::Finish(finish(LlmFinishReason::ToolUse)),
        ]
    }

    #[derive(Clone, Default)]
    struct RecordedCalls(Arc<Mutex<Vec<(String, String)>>>);

    struct FakeTools {
        calls: RecordedCalls,
        response: String,
    }

    impl ToolRegistryPort for FakeTools {
        fn list(&self) -> Box<[ToolSpec]> {
            ["read", "write", "str_replace_editor", "shell", "lsp__hover"]
                .iter()
                .map(|name| ToolSpec {
                    name: (*name).into(),
                    description: String::new(),
                    params_json: "{}".into(),
                })
                .collect()
        }
        fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
            self.calls
                .0
                .lock()
                .unwrap()
                .push((name.to_string(), args_json.to_string()));
            Ok(ToolResult::ok(&self.response))
        }
        fn is_native(&self, name: &str) -> bool {
            !name.starts_with("mcp__") && !name.starts_with("lsp__")
        }
    }

    #[derive(Clone, Default)]
    struct FakeSessions {
        store: Arc<Mutex<HashMap<String, Session>>>,
        checkpoints: Arc<Mutex<usize>>,
        next_id: Arc<Mutex<u64>>,
    }

    impl SessionStorePort for FakeSessions {
        fn create(&mut self) -> Result<String, BoxError> {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            let id = format!("session-{next}");
            self.store.lock().unwrap().insert(
                id.clone(),
                Session {
                    id: id.clone(),
                    created_at: "now".into(),
                    model: "fake-model".into(),
                    mode: AgentMode::Auto,
                    last_message_at: "now".into(),
                    step_count: 0,
                    messages: Box::new([]),
                },
            );
            Ok(id)
        }
        fn load(&self, id: &str) -> Result<Session, BoxError> {
            self.store
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| format!("no session {id}").into())
        }
        fn checkpoint(&mut self, id: &str, session: &Session) -> Result<(), BoxError> {
            *self.checkpoints.lock().unwrap() += 1;
            self.store
                .lock()
                .unwrap()
                .insert(id.to_string(), session.clone());
            Ok(())
        }
        fn fork(&mut self, _id: &str, _new_id: &str) -> Result<(), BoxError> {
            Ok(())
        }
        fn import_from(&mut self, _path: &std::path::Path) -> Result<String, BoxError> {
            Ok("imported".into())
        }
        fn export_to(&self, _id: &str, _path: &std::path::Path) -> Result<(), BoxError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FakeTelemetry {
        events: Arc<Mutex<Vec<TelemetryEvent>>>,
        reports: Arc<Mutex<Vec<TelemetryTotals>>>,
    }

    impl TelemetryPort for FakeTelemetry {
        fn emit(&mut self, ev: TelemetryEvent) {
            self.events.lock().unwrap().push(ev);
        }
        fn flush_report(
            &mut self,
            _session_id: &str,
            total: TelemetryTotals,
        ) -> Result<std::path::PathBuf, BoxError> {
            self.reports.lock().unwrap().push(total);
            Ok(std::path::PathBuf::from("report.json"))
        }
    }

    struct Harness {
        app: App,
        tool_calls: RecordedCalls,
        sessions: FakeSessions,
        telemetry: FakeTelemetry,
        llm_tools: Arc<Mutex<Vec<Vec<String>>>>,
    }

    fn harness(turns: Vec<Vec<LlmEvent>>, tool_response: &str) -> Harness {
        let llm = FakeLlm::new(turns);
        let llm_tools = llm.seen_tools.clone();
        let tool_calls = RecordedCalls::default();
        let sessions = FakeSessions::default();
        let telemetry = FakeTelemetry::default();
        let app = App::new(
            Box::new(llm),
            Box::new(FakeTools {
                calls: tool_calls.clone(),
                response: tool_response.to_string(),
            }),
            Box::new(sessions.clone()),
            Box::new(telemetry.clone()),
            Box::new(NullLogger),
        );
        Harness {
            app,
            tool_calls,
            sessions,
            telemetry,
            llm_tools,
        }
    }

    fn ctx() -> AgentContext {
        AgentContext {
            working_dir: std::path::PathBuf::from("."),
            model: "fake-model".into(),
            env: Vec::new(),
        }
    }

    fn kinds(telemetry: &FakeTelemetry) -> Vec<String> {
        telemetry
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect()
    }

    /// The text value of an extra field on the last event of `kind`, if any.
    fn last_extra_text(telemetry: &FakeTelemetry, kind: &str, key: &str) -> Option<String> {
        telemetry
            .events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e.kind == kind)
            .and_then(|e| {
                e.extra
                    .iter()
                    .find_map(|(k, v)| match (k.as_str() == key, v) {
                        (true, ExtraField::Text(s)) => Some(s.clone()),
                        _ => None,
                    })
            })
    }

    #[test]
    fn dispatches_a_tool_call_then_finishes() {
        let mut h = harness(
            vec![
                tool_use_turn(
                    "c1",
                    "str_replace_editor",
                    r#"{"command":"view","path":"a.rs"}"#,
                ),
                vec![
                    LlmEvent::Delta("done".into()),
                    LlmEvent::Finish(finish(LlmFinishReason::Stop)),
                ],
            ],
            "file contents",
        );
        let result = h
            .app
            .execute(&ctx(), ExecutionRequest::new("edit it"))
            .unwrap();

        assert_eq!(result.steps, 2);
        assert_eq!(result.final_text, "done");
        assert!(!result.truncated);

        let calls = h.tool_calls.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "str_replace_editor");
        assert!(calls[0].1.contains("a.rs"));

        // The transcript is system + user + assistant(tool_calls) + tool + assistant.
        let session = h.sessions.load(&result.session_id).unwrap();
        let roles: Vec<LlmRole> = session.messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                LlmRole::System,
                LlmRole::User,
                LlmRole::Assistant,
                LlmRole::Tool,
                LlmRole::Assistant
            ]
        );
    }

    #[test]
    fn checkpoints_every_round_and_writes_a_report() {
        let mut h = harness(
            vec![
                tool_use_turn("c1", "read", r#"{"path":"a.rs"}"#),
                vec![LlmEvent::Finish(finish(LlmFinishReason::Stop))],
            ],
            "ok",
        );
        h.app.execute(&ctx(), ExecutionRequest::new("go")).unwrap();
        // One checkpoint after the tool round, one on the final answer.
        assert_eq!(*h.sessions.checkpoints.lock().unwrap(), 2);
        assert_eq!(h.telemetry.reports.lock().unwrap().len(), 1);
    }

    #[test]
    fn planning_mode_hides_and_refuses_execute_tools() {
        let mut h = harness(vec![tool_use_turn("c1", "write", r#"{"path":"a"}"#)], "ok");
        let mut req = ExecutionRequest::new("plan it");
        req.mode = AgentMode::Planning;

        let err = h.app.execute(&ctx(), req).unwrap_err();
        assert!(matches!(err, AppError::Tool(_)), "got {err:?}");

        // The write never reached the registry…
        assert!(h.tool_calls.0.lock().unwrap().is_empty());
        // …and it was not even offered to the model (FR-MODE-01).
        let offered = &h.llm_tools.lock().unwrap()[0];
        assert!(offered.contains(&"read".to_string()));
        assert!(!offered.contains(&"write".to_string()));
        assert!(!offered.contains(&"shell".to_string()));
        // The refusal is recorded and a report still lands.
        assert!(kinds(&h.telemetry).contains(&"tool_denied".to_string()));
        assert_eq!(h.telemetry.reports.lock().unwrap().len(), 1);
    }

    #[test]
    fn build_mode_offers_and_runs_execute_tools() {
        let mut h = harness(
            vec![
                tool_use_turn("c1", "write", r#"{"path":"a","content":"x"}"#),
                vec![LlmEvent::Finish(finish(LlmFinishReason::Stop))],
            ],
            "written",
        );
        let mut req = ExecutionRequest::new("do it");
        req.mode = AgentMode::Auto;
        h.app.execute(&ctx(), req).unwrap();

        assert_eq!(h.tool_calls.0.lock().unwrap()[0].0, "write");
        assert!(h.llm_tools.lock().unwrap()[0].contains(&"write".to_string()));
    }

    #[test]
    fn turn_cap_stops_the_loop_and_reports_truncation() {
        // The model asks for a tool forever; the cap must win (FR-LOOP-02).
        let turns = (0..10)
            .map(|i| tool_use_turn(&format!("c{i}"), "read", "{}"))
            .collect();
        let mut h = harness(turns, "content");
        let mut req = ExecutionRequest::new("loop");
        req.max_turns = 3;

        let result = h.app.execute(&ctx(), req).unwrap();
        assert_eq!(result.steps, 3);
        assert!(result.truncated);
        assert_eq!(result.finish_reason, LlmFinishReason::Length);
        assert_eq!(h.tool_calls.0.lock().unwrap().len(), 3);
        // The turn cap and a genuine provider token cap both set
        // `finish_reason: Length`; `stop_cause` is what tells a consumer
        // (e.g. the opencode translation) which one actually happened.
        assert_eq!(
            last_extra_text(&h.telemetry, "finish", "stop_cause").as_deref(),
            Some("turn_cap")
        );
    }

    #[test]
    fn a_provider_token_cap_is_a_distinct_stop_cause_from_the_turn_cap() {
        let mut h = harness(
            vec![vec![LlmEvent::Finish(finish(LlmFinishReason::Length))]],
            "unused",
        );
        let req = ExecutionRequest::new("write a lot");

        let result = h.app.execute(&ctx(), req).unwrap();
        assert!(result.truncated);
        assert_eq!(result.finish_reason, LlmFinishReason::Length);
        assert_eq!(
            last_extra_text(&h.telemetry, "finish", "stop_cause").as_deref(),
            Some("token_cap")
        );
    }

    #[test]
    fn tool_output_is_capped_before_entering_history() {
        let big = "x".repeat(20_000);
        let mut h = harness(
            vec![
                tool_use_turn("c1", "read", "{}"),
                vec![LlmEvent::Finish(finish(LlmFinishReason::Stop))],
            ],
            &big,
        );
        let mut req = ExecutionRequest::new("read it");
        req.max_tool_output_chars = 16_000;
        let result = h.app.execute(&ctx(), req).unwrap();

        let session = h.sessions.load(&result.session_id).unwrap();
        let tool_msg = session
            .messages
            .iter()
            .find(|m| m.role == LlmRole::Tool)
            .expect("tool message");
        let content = &tool_msg.tool_result.as_ref().unwrap().content;
        assert!(content.ends_with("...[truncated]"));
        assert!(content.chars().count() < 16_100, "cap not applied");
    }

    #[test]
    fn truncate_helper_respects_char_boundaries() {
        let (out, cut) = truncate_tool_output("héllo wörld".into(), 5);
        assert!(cut);
        assert!(out.starts_with("héllo"));
        let (out, cut) = truncate_tool_output("short".into(), 100);
        assert!(!cut);
        assert_eq!(out, "short");
    }

    #[test]
    fn cancellation_checkpoints_reports_and_returns_interrupted() {
        let mut h = harness(vec![tool_use_turn("c1", "read", "{}")], "ok");
        let (flag, handle) = CancelFlag::new();
        h.app.set_cancel(flag);
        handle.trigger(); // as the SIGINT handler would

        let err = h
            .app
            .execute(&ctx(), ExecutionRequest::new("interrupt me"))
            .unwrap_err();
        assert!(matches!(err, AppError::Interrupted));
        // FR-IFACE-05: partial session persisted, telemetry flushed.
        assert!(*h.sessions.checkpoints.lock().unwrap() >= 1);
        assert_eq!(h.telemetry.reports.lock().unwrap().len(), 1);
    }

    #[test]
    fn zero_timeout_aborts_before_calling_the_model() {
        let mut h = harness(vec![tool_use_turn("c1", "read", "{}")], "ok");
        let mut req = ExecutionRequest::new("too slow");
        req.timeout_ms = Some(0);
        let err = h.app.execute(&ctx(), req).unwrap_err();
        assert!(matches!(err, AppError::Timeout(0)), "got {err:?}");
    }

    #[test]
    fn resumes_an_existing_session_transcript() {
        let mut h = harness(
            vec![vec![
                LlmEvent::Delta("second answer".into()),
                LlmEvent::Finish(finish(LlmFinishReason::Stop)),
            ]],
            "ok",
        );
        let first = h
            .app
            .execute(&ctx(), ExecutionRequest::new("first question"))
            .unwrap();

        let mut req = ExecutionRequest::new("second question");
        req.session_id = Some(first.session_id.clone());
        let second = h.app.execute(&ctx(), req).unwrap();
        assert_eq!(second.session_id, first.session_id);

        let session = h.sessions.load(&second.session_id).unwrap();
        let user_turns: Vec<&str> = session
            .messages
            .iter()
            .filter(|m| m.role == LlmRole::User)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(user_turns, vec!["first question", "second question"]);
        // Exactly one system prompt survives the resume.
        assert_eq!(
            session
                .messages
                .iter()
                .filter(|m| m.role == LlmRole::System)
                .count(),
            1
        );
    }

    #[test]
    fn tool_failures_are_fed_back_instead_of_aborting() {
        struct FailingTools;
        impl ToolRegistryPort for FailingTools {
            fn list(&self) -> Box<[ToolSpec]> {
                Box::new([])
            }
            fn call(&mut self, _name: &str, _args: &str) -> Result<ToolResult, BoxError> {
                Err("registry exploded".into())
            }
            fn is_native(&self, _name: &str) -> bool {
                true
            }
        }
        let sessions = FakeSessions::default();
        let telemetry = FakeTelemetry::default();
        let mut app = App::new(
            Box::new(FakeLlm::new(vec![
                tool_use_turn("c1", "read", "{}"),
                vec![
                    LlmEvent::Delta("recovered".into()),
                    LlmEvent::Finish(finish(LlmFinishReason::Stop)),
                ],
            ])),
            Box::new(FailingTools),
            Box::new(sessions.clone()),
            Box::new(telemetry.clone()),
            Box::new(NullLogger),
        );

        let result = app.execute(&ctx(), ExecutionRequest::new("go")).unwrap();
        assert_eq!(result.final_text, "recovered");
        let session = sessions.load(&result.session_id).unwrap();
        let tool_msg = session
            .messages
            .iter()
            .find(|m| m.role == LlmRole::Tool)
            .unwrap();
        assert!(tool_msg
            .tool_result
            .as_ref()
            .unwrap()
            .content
            .contains("registry exploded"));
    }

    #[test]
    fn llm_transport_failure_surfaces_as_llm_error() {
        struct BrokenLlm;
        impl LlmPort for BrokenLlm {
            fn send(&mut self, _req: &LlmRequest) -> Result<LlmResponse, BoxError> {
                Err("no network".into())
            }
            fn stream(
                &mut self,
                _req: &LlmRequest,
            ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
                Box::new(std::iter::once(Err("no network".into())))
            }
        }
        let mut app = App::new(
            Box::new(BrokenLlm),
            Box::new(FakeTools {
                calls: RecordedCalls::default(),
                response: String::new(),
            }),
            Box::new(FakeSessions::default()),
            Box::new(FakeTelemetry::default()),
            Box::new(NullLogger),
        );
        let err = app
            .execute(&ctx(), ExecutionRequest::new("go"))
            .unwrap_err();
        assert!(matches!(err, AppError::Llm(_)), "got {err:?}");
    }

    #[test]
    fn emits_ui_and_telemetry_events_for_a_run() {
        #[derive(Clone, Default)]
        struct Recorder(Arc<Mutex<Vec<String>>>);
        impl Emitter for Recorder {
            fn emit(&mut self, ev: UiEvent) {
                let label = match ev {
                    UiEvent::Delta(_) => "delta",
                    UiEvent::ToolCallStart { .. } => "tool_call_start",
                    UiEvent::ToolCallArgs { .. } => "tool_call_args",
                    UiEvent::ToolResult { .. } => "tool_result",
                    UiEvent::Finish(_) => "finish",
                    UiEvent::Usage(_) => "usage",
                    UiEvent::LoopStart { .. } => "loop_start",
                    UiEvent::LoopEnd { .. } => "loop_end",
                    UiEvent::Error(_) => "error",
                    UiEvent::Retry(_) => "retry",
                    UiEvent::Notice(_) => "notice",
                };
                self.0.lock().unwrap().push(label.to_string());
            }
        }

        let mut h = harness(
            vec![
                tool_use_turn("c1", "read", "{}"),
                vec![
                    LlmEvent::Delta("hi".into()),
                    LlmEvent::Finish(finish(LlmFinishReason::Stop)),
                ],
            ],
            "ok",
        );
        let recorder = Recorder::default();
        h.app.set_emitter(Box::new(recorder.clone()));
        h.app.execute(&ctx(), ExecutionRequest::new("go")).unwrap();

        let seen = recorder.0.lock().unwrap();
        for expected in [
            "loop_start",
            "tool_call_start",
            "tool_result",
            "delta",
            "loop_end",
        ] {
            assert!(seen.contains(&expected.to_string()), "missing {expected}");
        }
        let telemetry_kinds = kinds(&h.telemetry);
        for expected in [
            "loop_start",
            "tool_call",
            "tool_result",
            "llm_delta",
            "finish",
        ] {
            assert!(
                telemetry_kinds.contains(&expected.to_string()),
                "missing telemetry {expected}"
            );
        }
    }

    #[test]
    fn token_usage_accumulates_across_turns() {
        let mut h = harness(
            vec![
                tool_use_turn("c1", "read", "{}"),
                vec![LlmEvent::Finish(finish(LlmFinishReason::Stop))],
            ],
            "ok",
        );
        let result = h.app.execute(&ctx(), ExecutionRequest::new("go")).unwrap();
        // Two turns, provider-reported 10/5/1 each (DQ2).
        assert_eq!(result.input_tokens, 20);
        assert_eq!(result.output_tokens, 10);
        assert_eq!(result.cache_tokens, 2);
    }

    #[test]
    fn falls_back_to_the_token_heuristic_when_usage_is_absent() {
        let mut h = harness(
            vec![vec![
                LlmEvent::Delta("some words here".into()),
                LlmEvent::Finish(LlmFinish {
                    reason: LlmFinishReason::Stop,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_tokens: 0,
                    cost_usd: None,
                }),
            ]],
            "ok",
        );
        let result = h.app.execute(&ctx(), ExecutionRequest::new("go")).unwrap();
        assert!(result.output_tokens > 0, "heuristic should fill the gap");
        assert!(result.input_tokens > 0);
    }
}
