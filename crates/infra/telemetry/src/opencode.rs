//! opencode-compatible event stream (`--json-format opencode`).
//!
//! zcode's own JSONL is a flat log: one line per thing that happened. opencode
//! publishes an *event bus* whose envelopes are versioned, aggregated by
//! session, and named `session.next.*`. Consumers written against opencode —
//! dashboards, TUIs, CI parsers — expect that shape, so zcode can speak it.
//!
//! The schema here is transcribed from opencode's own
//! `packages/schema/src/session-event.ts` and `event.ts`, not guessed. The
//! envelope is:
//!
//! ```json
//! { "id": "evt_…", "type": "session.next.tool.called", "data": { … } }
//! ```
//!
//! with `timestamp` (unix millis) and `sessionID` on every payload.
//!
//! **This is a translation, not an emulation.** zcode has no message store, so
//! nothing durable, replayable, or aggregate-sequenced is emitted: no
//! `durable` block, no `message.*` events, no `session.created`. What is
//! emitted matches opencode's field names and types for the subset zcode
//! actually produces, which is what a stream consumer reads.

use std::io::Write;

use domain::{BoxError, ExtraField, TelemetryEvent, TelemetryPort, TelemetryTotals};

/// Translates zcode telemetry into opencode event envelopes.
///
/// Holds the small amount of state opencode's shape needs and zcode's does
/// not: an assistant-message id per run, a text-part id, the accumulated text
/// for `text.ended`, and the tool name behind each call id (opencode reports
/// the name on `tool.called` and the caller correlates by `callID` after).
pub struct OpencodeTelemetry {
    out: Box<dyn Write + Send>,
    seq: u64,
    session_id: String,
    message_id: String,
    /// Some(id) while a text part is open.
    text_id: Option<String>,
    /// Accumulated text of the open part, for the closing `text.ended`.
    text: String,
    /// callID → tool name, so `tool.success` can be attributed.
    calls: Vec<(String, String)>,
    /// True once a step is open, so the closing events are balanced.
    step_open: bool,
}

impl OpencodeTelemetry {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self {
            out,
            seq: 0,
            session_id: String::new(),
            message_id: String::new(),
            text_id: None,
            text: String::new(),
            calls: Vec::new(),
            step_open: false,
        }
    }

    /// opencode ids are prefixed and monotonic; `evt_`/`msg_` are the
    /// prefixes its schema validates against.
    fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}{:012}", self.seq)
    }

    fn now_millis() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Write one envelope. `data` is merged with the base fields every
    /// `session.next.*` payload carries.
    fn emit_event(&mut self, kind: &str, mut data: serde_json::Map<String, serde_json::Value>) {
        data.insert("timestamp".into(), Self::now_millis().into());
        data.insert("sessionID".into(), self.session_id.clone().into());
        let id = self.next_id("evt_");
        let envelope = serde_json::json!({ "id": id, "type": kind, "data": data });
        let line = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".into());
        let _ = writeln!(self.out, "{line}");
    }

    fn adopt_session(&mut self, ev: &TelemetryEvent) {
        if self.session_id.is_empty() && !ev.session_id.is_empty() {
            // opencode session ids start with `ses`; zcode's are UUIDv7.
            self.session_id = format!("ses_{}", ev.session_id);
            self.message_id = self.next_id("msg_");
        }
    }

    /// Close an open step, if one is open.
    ///
    /// Per-step token counts are not available: zcode's provider clients
    /// report usage once for the whole run, so intermediate steps carry zeros
    /// and the final one carries the totals. The alternative — one opencode
    /// step per zcode *run* — would lose the step structure entirely.
    fn close_step(&mut self, finish: &str, cost: Option<f64>, tokens: (u64, u64, u64)) {
        if !self.step_open {
            return;
        }
        self.step_open = false;
        let (input, output, cache) = tokens;
        let mut data = self.base_message();
        data.insert("finish".into(), finish.into());
        data.insert("cost".into(), cost.unwrap_or(0.0).into());
        data.insert(
            "tokens".into(),
            serde_json::json!({
                "input": input,
                "output": output,
                "reasoning": 0,
                // zcode reports one cache figure; opencode splits read from
                // write. Attributing it all to `read` is the honest placement —
                // a write would imply a cost we did not measure.
                "cache": { "read": cache, "write": 0 },
            }),
        );
        self.emit_event("session.next.step.ended", data);
    }

    /// Close an open text part with the replayable full value, as opencode
    /// does: deltas are live-only, `text.ended` is the boundary consumers
    /// persist.
    fn close_text(&mut self) {
        let Some(text_id) = self.text_id.take() else {
            return;
        };
        let text = std::mem::take(&mut self.text);
        let mut data = self.base_message();
        data.insert("textID".into(), text_id.into());
        data.insert("text".into(), text.into());
        self.emit_event("session.next.text.ended", data);
    }

    fn base_message(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert("assistantMessageID".into(), self.message_id.clone().into());
        map
    }

    /// opencode carries the model as `{ id, providerID }`, not a flat string.
    fn model_ref(model: &str) -> serde_json::Value {
        match model.split_once('/') {
            Some((provider, id)) => serde_json::json!({ "id": id, "providerID": provider }),
            None => serde_json::json!({ "id": model, "providerID": "zcode" }),
        }
    }

    fn text_field(extra: &[(String, ExtraField)], key: &str) -> Option<String> {
        extra
            .iter()
            .find_map(|(k, v)| match (k.as_str() == key, v) {
                (true, ExtraField::Text(s)) => Some(s.clone()),
                _ => None,
            })
    }

    fn remember_call(&mut self, call_id: String, tool: String) {
        // Bounded: a turn has a handful of calls, and stale ids are pruned as
        // they resolve. Kept as a Vec because a HashMap for four entries is
        // more allocation, not less.
        if self.calls.len() >= 64 {
            self.calls.remove(0);
        }
        self.calls.push((call_id, tool));
    }

    fn take_call(&mut self, call_id: &str) -> Option<String> {
        let index = self.calls.iter().position(|(id, _)| id == call_id)?;
        Some(self.calls.remove(index).1)
    }

    /// The `session.error` opencode reports for a truncated run — or `None`
    /// when the real cause is not a length limit at all.
    ///
    /// `truncated` used to be reported with one hardcoded message regardless
    /// of `stop_cause`: a genuine provider token limit, zcode's own
    /// `--max-turns` cap, a `--timeout-ms` deadline, and a user's Ctrl-C all
    /// read as "stopped at the turn or token cap". That is wrong for the
    /// last two — neither is a length limit — and told a consumer nothing
    /// about which of the first two actually happened. `stop_cause` (set by
    /// `app::AgentLoop::execute`, see `finish_run`) carries the real reason;
    /// this maps it onto opencode's one length-error type, or withholds the
    /// event where opencode's schema has nothing that would be true.
    fn truncation_error(stop_cause: Option<&str>) -> Option<(&'static str, &'static str)> {
        match stop_cause {
            // The provider itself cut the message off at its output/token
            // budget — this is exactly what `MessageOutputLengthError` means.
            Some("token_cap") => Some((
                "MessageOutputLengthError",
                "stopped: the model reached its output token limit",
            )),
            // zcode's own step budget (`--max-turns`). opencode has no
            // separate error for this, so it is reported under the same
            // name — both are "ran out of length budget" — but the message
            // says which budget it actually was, rather than leaving the
            // reader to guess between the two.
            Some("turn_cap") => Some((
                "MessageOutputLengthError",
                "stopped: reached the maximum number of turns",
            )),
            // A wall-clock timeout or a user cancellation is not a length
            // limit; the CLI/TUI already report these through their own
            // channel (see `cli::mod::run`'s handling of `AppError::Timeout`
            // / `AppError::Interrupted`), so nothing is emitted here rather
            // than blaming a cap that was never hit.
            Some("timeout") | Some("cancelled") => None,
            // `truncated` is set with no cause we recognise (an older engine
            // build, or a cause added there but not yet mirrored here): keep
            // reporting *something* rather than silently dropping a real
            // truncation.
            _ => Some((
                "MessageOutputLengthError",
                "stopped at the turn or token cap",
            )),
        }
    }
}

impl TelemetryPort for OpencodeTelemetry {
    fn emit(&mut self, ev: TelemetryEvent) {
        self.adopt_session(&ev);

        match ev.kind.as_str() {
            "loop_start" => {
                // A new step means any text from the previous one is final,
                // and that the previous step itself is over. opencode pairs
                // `step.started` with `step.ended`; leaving them unbalanced
                // would strand every intermediate step open in a consumer.
                self.close_text();
                self.close_step("tool_use", None, (0, 0, 0));
                let mut data = self.base_message();
                data.insert(
                    "agent".into(),
                    Self::text_field(&ev.extra, "mode")
                        .unwrap_or_else(|| "build".into())
                        .into(),
                );
                data.insert("model".into(), Self::model_ref(&ev.model));
                self.emit_event("session.next.step.started", data);
                self.step_open = true;
            }

            "llm_delta" => {
                let Some(delta) = Self::text_field(&ev.extra, "text") else {
                    return;
                };
                if self.text_id.is_none() {
                    let text_id = self.next_id("txt_");
                    self.text_id = Some(text_id.clone());
                    let mut data = self.base_message();
                    data.insert("textID".into(), text_id.into());
                    self.emit_event("session.next.text.started", data);
                }
                self.text.push_str(&delta);
                let text_id = self.text_id.clone().unwrap_or_default();
                let mut data = self.base_message();
                data.insert("textID".into(), text_id.into());
                data.insert("delta".into(), delta.into());
                self.emit_event("session.next.text.delta", data);
            }

            "tool_call" => {
                self.close_text();
                let tool = Self::text_field(&ev.extra, "tool").unwrap_or_default();
                let call_id = Self::text_field(&ev.extra, "tool_call_id").unwrap_or_default();
                let arguments = Self::text_field(&ev.extra, "arguments").unwrap_or_default();
                self.remember_call(call_id.clone(), tool.clone());

                // opencode types `input` as an object; a model that emits
                // malformed JSON still has to produce a valid envelope, so
                // unparseable arguments are carried under `raw`.
                let input: serde_json::Value = serde_json::from_str(&arguments)
                    .ok()
                    .filter(serde_json::Value::is_object)
                    .unwrap_or_else(|| serde_json::json!({ "raw": arguments }));

                let mut data = self.base_message();
                data.insert("callID".into(), call_id.into());
                data.insert("tool".into(), tool.into());
                data.insert("input".into(), input);
                data.insert("provider".into(), serde_json::json!({ "executed": false }));
                self.emit_event("session.next.tool.called", data);
            }

            "tool_result" => {
                let call_id = Self::text_field(&ev.extra, "tool_call_id").unwrap_or_default();
                self.take_call(&call_id);
                let error = Self::text_field(&ev.extra, "error");
                let output = Self::text_field(&ev.extra, "output").unwrap_or_default();

                let mut data = self.base_message();
                data.insert("callID".into(), call_id.into());
                match error {
                    Some(message) => {
                        data.insert(
                            "error".into(),
                            serde_json::json!({ "name": "ToolError", "data": { "message": message } }),
                        );
                        data.insert("provider".into(), serde_json::json!({ "executed": true }));
                        self.emit_event("session.next.tool.failed", data);
                    }
                    None => {
                        data.insert("structured".into(), serde_json::json!({}));
                        data.insert(
                            "content".into(),
                            serde_json::json!([{ "type": "text", "text": output }]),
                        );
                        data.insert("provider".into(), serde_json::json!({ "executed": true }));
                        self.emit_event("session.next.tool.success", data);
                    }
                }
            }

            "tool_denied" => {
                let call_id = Self::text_field(&ev.extra, "tool_call_id").unwrap_or_default();
                let tool = Self::text_field(&ev.extra, "tool").unwrap_or_default();
                let reason = Self::text_field(&ev.extra, "reason").unwrap_or_default();
                self.take_call(&call_id);
                let mut data = self.base_message();
                data.insert("callID".into(), call_id.into());
                data.insert(
                    "error".into(),
                    serde_json::json!({
                        "name": "PermissionDenied",
                        "data": { "message": format!("tool `{tool}` denied: {reason}") }
                    }),
                );
                data.insert("provider".into(), serde_json::json!({ "executed": false }));
                self.emit_event("session.next.tool.failed", data);
            }

            "llm_retry" => {
                let number = |key: &str| {
                    ev.extra
                        .iter()
                        .find_map(|(k, v)| match (k.as_str() == key, v) {
                            (true, ExtraField::Number(n)) => Some(*n),
                            _ => None,
                        })
                };
                let mut error = serde_json::Map::new();
                error.insert(
                    "message".into(),
                    Self::text_field(&ev.extra, "reason")
                        .unwrap_or_else(|| "retrying".into())
                        .into(),
                );
                if let Some(status) = number("status") {
                    error.insert("statusCode".into(), status.into());
                }
                error.insert("isRetryable".into(), true.into());

                let mut data = serde_json::Map::new();
                data.insert("attempt".into(), number("attempt").unwrap_or(1.0).into());
                data.insert("error".into(), serde_json::Value::Object(error));
                self.emit_event("session.next.retried", data);
            }

            "finish" => {
                self.close_text();
                let truncated = ev
                    .extra
                    .iter()
                    .any(|(k, v)| k == "truncated" && matches!(v, ExtraField::Bool(true)));
                let cost = ev.extra.iter().find_map(|(k, v)| match (k.as_str(), v) {
                    ("cost_usd", ExtraField::Number(n)) => Some(*n),
                    _ => None,
                });
                let reason = Self::text_field(&ev.extra, "reason").unwrap_or_else(|| "stop".into());
                self.close_step(
                    &reason,
                    cost,
                    (ev.input_tokens, ev.output_tokens, ev.cache_tokens),
                );

                if truncated {
                    let stop_cause = Self::text_field(&ev.extra, "stop_cause");
                    if let Some((name, message)) = Self::truncation_error(stop_cause.as_deref()) {
                        let mut data = serde_json::Map::new();
                        data.insert(
                            "error".into(),
                            serde_json::json!({
                                "name": name,
                                "data": { "message": message }
                            }),
                        );
                        self.emit_event("session.error", data);
                    }
                }
                // opencode signals the end of activity with `session.idle`.
                self.emit_event("session.idle", serde_json::Map::new());
            }

            _ => {}
        }
        let _ = self.out.flush();
    }

    fn flush_report(
        &mut self,
        _session_id: &str,
        _total: TelemetryTotals,
    ) -> Result<std::path::PathBuf, BoxError> {
        // The report file belongs to the primary telemetry port; in
        // opencode mode this emitter only renders the stream.
        Ok(std::path::PathBuf::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn event(kind: &str, extra: Vec<(&str, ExtraField)>) -> TelemetryEvent {
        TelemetryEvent {
            kind: kind.into(),
            model: "anthropic/claude-haiku-4.5".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_tokens: 0,
            steps: 1,
            execution_time_ms: 0,
            session_id: "01a03d78-8a60-7d00-853b-21ad80af5fd2".into(),
            extra: extra
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    fn render(events: Vec<TelemetryEvent>) -> Vec<serde_json::Value> {
        let sink = Sink::default();
        {
            let mut t = OpencodeTelemetry::new(Box::new(sink.clone()));
            for ev in events {
                t.emit(ev);
            }
        }
        let bytes = sink.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line is valid JSON"))
            .collect()
    }

    fn kinds(events: &[serde_json::Value]) -> Vec<String> {
        events
            .iter()
            .map(|e| e["type"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn every_envelope_carries_id_type_and_data() {
        let out = render(vec![event(
            "loop_start",
            vec![("mode", ExtraField::Text("auto".into()))],
        )]);
        let ev = &out[0];
        assert!(ev["id"].as_str().unwrap().starts_with("evt_"));
        assert_eq!(ev["type"], "session.next.step.started");
        assert!(ev["data"]["timestamp"].is_number());
        assert!(ev["data"]["sessionID"].as_str().unwrap().starts_with("ses"));
        assert!(ev["data"]["assistantMessageID"]
            .as_str()
            .unwrap()
            .starts_with("msg_"));
    }

    #[test]
    fn the_model_is_a_ref_not_a_string() {
        // opencode types it as { id, providerID }.
        let out = render(vec![event("loop_start", vec![])]);
        assert_eq!(out[0]["data"]["model"]["providerID"], "anthropic");
        assert_eq!(out[0]["data"]["model"]["id"], "claude-haiku-4.5");
    }

    #[test]
    fn text_is_bracketed_by_started_and_ended() {
        let out = render(vec![
            event("loop_start", vec![]),
            event("llm_delta", vec![("text", ExtraField::Text("hel".into()))]),
            event("llm_delta", vec![("text", ExtraField::Text("lo".into()))]),
            event("finish", vec![("reason", ExtraField::Text("stop".into()))]),
        ]);
        assert_eq!(
            kinds(&out),
            vec![
                "session.next.step.started",
                "session.next.text.started",
                "session.next.text.delta",
                "session.next.text.delta",
                "session.next.text.ended",
                "session.next.step.ended",
                "session.idle",
            ]
        );
        // `text.ended` carries the whole value, as opencode's does.
        let ended = out.iter().find(|e| e["type"] == "session.next.text.ended");
        assert_eq!(ended.unwrap()["data"]["text"], "hello");
    }

    #[test]
    fn a_tool_call_and_its_result_correlate_by_call_id() {
        let out = render(vec![
            event(
                "tool_call",
                vec![
                    ("tool", ExtraField::Text("read".into())),
                    ("tool_call_id", ExtraField::Text("call_1".into())),
                    (
                        "arguments",
                        ExtraField::Text(r#"{"path":"main.go"}"#.into()),
                    ),
                ],
            ),
            event(
                "tool_result",
                vec![
                    ("tool", ExtraField::Text("read".into())),
                    ("tool_call_id", ExtraField::Text("call_1".into())),
                    ("error", ExtraField::Null),
                    ("output", ExtraField::Text("package main".into())),
                ],
            ),
        ]);
        assert_eq!(
            kinds(&out),
            vec!["session.next.tool.called", "session.next.tool.success"]
        );
        assert_eq!(out[0]["data"]["callID"], "call_1");
        assert_eq!(out[1]["data"]["callID"], "call_1");
        // `input` is an object, not the raw string.
        assert_eq!(out[0]["data"]["input"]["path"], "main.go");
        assert_eq!(out[1]["data"]["content"][0]["type"], "text");
        assert_eq!(out[1]["data"]["content"][0]["text"], "package main");
    }

    #[test]
    fn malformed_tool_arguments_still_produce_a_valid_envelope() {
        // A model that emits broken JSON must not break the stream.
        let out = render(vec![event(
            "tool_call",
            vec![
                ("tool", ExtraField::Text("read".into())),
                ("tool_call_id", ExtraField::Text("c1".into())),
                ("arguments", ExtraField::Text("{not json".into())),
            ],
        )]);
        assert_eq!(out[0]["data"]["input"]["raw"], "{not json");
    }

    #[test]
    fn a_failing_tool_becomes_tool_failed() {
        let out = render(vec![event(
            "tool_result",
            vec![
                ("tool", ExtraField::Text("shell".into())),
                ("tool_call_id", ExtraField::Text("c1".into())),
                ("error", ExtraField::Text("blocked by the allowlist".into())),
            ],
        )]);
        assert_eq!(out[0]["type"], "session.next.tool.failed");
        assert_eq!(
            out[0]["data"]["error"]["data"]["message"],
            "blocked by the allowlist"
        );
    }

    #[test]
    fn a_denied_tool_is_a_permission_failure() {
        let out = render(vec![event(
            "tool_denied",
            vec![
                ("tool", ExtraField::Text("apply_patch".into())),
                ("tool_call_id", ExtraField::Text("c1".into())),
                ("reason", ExtraField::Text("planning_mode".into())),
            ],
        )]);
        assert_eq!(out[0]["type"], "session.next.tool.failed");
        assert_eq!(out[0]["data"]["error"]["name"], "PermissionDenied");
    }

    #[test]
    fn a_retry_carries_the_attempt_and_status() {
        let out = render(vec![event(
            "llm_retry",
            vec![
                ("attempt", ExtraField::Number(2.0)),
                ("status", ExtraField::Number(429.0)),
                (
                    "reason",
                    ExtraField::Text("rate limited by the provider".into()),
                ),
            ],
        )]);
        assert_eq!(out[0]["type"], "session.next.retried");
        assert_eq!(out[0]["data"]["attempt"], 2.0);
        assert_eq!(out[0]["data"]["error"]["statusCode"], 429.0);
        assert_eq!(out[0]["data"]["error"]["isRetryable"], true);
    }

    #[test]
    fn the_finish_event_carries_opencodes_token_shape() {
        let mut ev = event("finish", vec![("reason", ExtraField::Text("stop".into()))]);
        ev.input_tokens = 7078;
        ev.output_tokens = 154;
        ev.cache_tokens = 2496;
        // A real run opens a step first; `step.ended` closes that step.
        let out = render(vec![event("loop_start", vec![]), ev]);
        let ended = &out[1];
        assert_eq!(ended["type"], "session.next.step.ended");
        assert_eq!(ended["data"]["tokens"]["input"], 7078);
        assert_eq!(ended["data"]["tokens"]["output"], 154);
        assert_eq!(ended["data"]["tokens"]["cache"]["read"], 2496);
        assert_eq!(ended["data"]["tokens"]["cache"]["write"], 0);
        assert_eq!(ended["data"]["finish"], "stop");
        // …and the run signs off as idle.
        assert_eq!(out[2]["type"], "session.idle");
    }

    #[test]
    fn every_started_step_is_closed() {
        // Regression: `step.started` was emitted per step but `step.ended`
        // only once, so a multi-step run stranded every intermediate step
        // open in a consumer tracking them.
        let mut events = Vec::new();
        for _ in 0..4 {
            events.push(event("loop_start", vec![]));
            events.push(event(
                "tool_call",
                vec![
                    ("tool", ExtraField::Text("read".into())),
                    ("tool_call_id", ExtraField::Text("c1".into())),
                    ("arguments", ExtraField::Text("{}".into())),
                ],
            ));
        }
        events.push(event(
            "finish",
            vec![("reason", ExtraField::Text("stop".into()))],
        ));
        let out = render(events);
        let started = out
            .iter()
            .filter(|e| e["type"] == "session.next.step.started")
            .count();
        let ended = out
            .iter()
            .filter(|e| e["type"] == "session.next.step.ended")
            .count();
        assert_eq!(started, 4);
        assert_eq!(ended, started, "{started} started, {ended} ended");
    }

    #[test]
    fn intermediate_steps_close_as_tool_use_and_the_last_carries_the_totals() {
        let mut finish = event("finish", vec![("reason", ExtraField::Text("stop".into()))]);
        finish.input_tokens = 7078;
        finish.output_tokens = 154;
        let out = render(vec![
            event("loop_start", vec![]),
            event("loop_start", vec![]),
            finish,
        ]);
        let ended: Vec<&serde_json::Value> = out
            .iter()
            .filter(|e| e["type"] == "session.next.step.ended")
            .collect();
        assert_eq!(ended.len(), 2);
        assert_eq!(ended[0]["data"]["finish"], "tool_use");
        assert_eq!(ended[0]["data"]["tokens"]["input"], 0);
        assert_eq!(ended[1]["data"]["finish"], "stop");
        assert_eq!(ended[1]["data"]["tokens"]["input"], 7078);
    }

    #[test]
    fn a_run_with_no_steps_still_goes_idle() {
        // A failure before the first step must not emit an unpaired end.
        let out = render(vec![event("finish", vec![])]);
        assert_eq!(kinds(&out), vec!["session.idle"]);
    }

    #[test]
    fn a_truncated_run_reports_an_error_before_going_idle() {
        let out = render(vec![
            event("loop_start", vec![]),
            event(
                "finish",
                vec![
                    ("reason", ExtraField::Text("length".into())),
                    ("truncated", ExtraField::Bool(true)),
                ],
            ),
        ]);
        assert_eq!(
            kinds(&out),
            vec![
                "session.next.step.started",
                "session.next.step.ended",
                "session.error",
                "session.idle"
            ]
        );
        // No `stop_cause` on the event: the fallback message, not a guess
        // dressed up as one of the specific causes.
        let error = out.iter().find(|e| e["type"] == "session.error").unwrap();
        assert_eq!(
            error["data"]["error"]["data"]["message"],
            "stopped at the turn or token cap"
        );
    }

    #[test]
    fn a_turn_cap_truncation_names_the_real_cause() {
        let out = render(vec![
            event("loop_start", vec![]),
            event(
                "finish",
                vec![
                    ("reason", ExtraField::Text("length".into())),
                    ("truncated", ExtraField::Bool(true)),
                    ("stop_cause", ExtraField::Text("turn_cap".into())),
                ],
            ),
        ]);
        let error = out.iter().find(|e| e["type"] == "session.error").unwrap();
        assert_eq!(
            error["data"]["error"]["data"]["message"],
            "stopped: reached the maximum number of turns"
        );
    }

    #[test]
    fn a_token_cap_truncation_names_the_real_cause() {
        let out = render(vec![
            event("loop_start", vec![]),
            event(
                "finish",
                vec![
                    ("reason", ExtraField::Text("length".into())),
                    ("truncated", ExtraField::Bool(true)),
                    ("stop_cause", ExtraField::Text("token_cap".into())),
                ],
            ),
        ]);
        let error = out.iter().find(|e| e["type"] == "session.error").unwrap();
        assert_eq!(
            error["data"]["error"]["data"]["message"],
            "stopped: the model reached its output token limit"
        );
    }

    #[test]
    fn a_timeout_or_cancellation_is_not_reported_as_a_length_cap() {
        // Neither is a length limit; reporting `session.error` /
        // `MessageOutputLengthError` for them would be flatly wrong.
        for cause in ["timeout", "cancelled"] {
            let out = render(vec![
                event("loop_start", vec![]),
                event(
                    "finish",
                    vec![
                        ("reason", ExtraField::Text("stop".into())),
                        ("truncated", ExtraField::Bool(true)),
                        ("stop_cause", ExtraField::Text(cause.into())),
                    ],
                ),
            ]);
            assert_eq!(
                kinds(&out),
                vec![
                    "session.next.step.started",
                    "session.next.step.ended",
                    "session.idle"
                ],
                "cause `{cause}` must not emit session.error"
            );
        }
    }

    #[test]
    fn event_ids_are_unique_and_ordered() {
        let out = render(vec![
            event("loop_start", vec![]),
            event("llm_delta", vec![("text", ExtraField::Text("a".into()))]),
            event("finish", vec![]),
        ]);
        let ids: Vec<&str> = out.iter().map(|e| e["id"].as_str().unwrap()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids must be unique");
        assert_eq!(sorted, ids, "ids must be monotonically ordered");
    }

    #[test]
    fn the_call_table_cannot_grow_without_bound() {
        // A pathological run that never resolves its calls must not leak.
        let sink = Sink::default();
        let mut t = OpencodeTelemetry::new(Box::new(sink));
        for i in 0..500 {
            t.emit(event(
                "tool_call",
                vec![
                    ("tool", ExtraField::Text("read".into())),
                    ("tool_call_id", ExtraField::Text(format!("c{i}"))),
                    ("arguments", ExtraField::Text("{}".into())),
                ],
            ));
        }
        assert!(t.calls.len() <= 64, "{} entries retained", t.calls.len());
    }
}
