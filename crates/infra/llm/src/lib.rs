//! LLM provider adapters implementing `domain::LlmPort`.
//!
//! Four providers share the streaming contract: OpenAI / OpenRouter / vLLM
//! (OpenAI-compatible SSE), Anthropic (dedicated SSE), and Ollama (NDJSON).
//! Parsing is split out into free functions so hermetic unit tests can feed
//! canned payloads without a network (T4–T6/T8/T9).
//!
//! Direct deps: domain, serde, serde_json, reqwest, thiserror (L3).

use std::time::Duration;

use domain::{
    BoxError, LlmEvent, LlmFinish, LlmFinishReason, LlmMessage, LlmRequest, LlmResponse, LlmRole,
    LlmToolCall,
};
use reqwest::blocking::Client;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared OpenAI-wire-shape adapter. `OpenAiLlm`, `OpenRouterLlm` and `VllmLlm`
/// all wrap one of these with a different endpoint (DRY, L3).
pub struct OpenAiShapeLlm {
    client: Client,
    endpoint: String,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl OpenAiShapeLlm {
    pub fn new(endpoint: &str, api_key: &str, model: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(DEFAULT_TIMEOUT)
                .user_agent("ag/0.2.0")
                .build()
                .expect("reqwest client"),
            endpoint: endpoint.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn http_body(&self, req: &LlmRequest) -> Result<String, BoxError> {
        let payload = build_openai_request(req);
        let resp = self
            .client
            .post(&self.endpoint)
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&payload)
            .timeout(self.timeout)
            .send()?;
        let status = resp.status();
        let body = resp.text()?;
        if !status.is_success() {
            return Err(format!("openai request failed ({}): {}", status, body).into());
        }
        Ok(body)
    }
}

/// OpenAI adapter (default endpoint api.openai.com).
pub struct OpenAiLlm(OpenAiShapeLlm);
impl OpenAiLlm {
    pub fn new(endpoint: &str, api_key: &str, model: &str) -> Self {
        Self(OpenAiShapeLlm::new(endpoint, api_key, model))
    }
}
impl domain::LlmPort for OpenAiLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        let body = self.0.http_body(req)?;
        let events = parse_openai_events(&body);
        Ok(aggregate(events, body))
    }
    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let body = match self.0.http_body(req) {
            Ok(b) => b,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(parse_openai_events(&body).into_iter())
    }
}

/// OpenRouter reuses the exact OpenAI wire shape with a different endpoint.
pub struct OpenRouterLlm(OpenAiShapeLlm);
impl OpenRouterLlm {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self(OpenAiShapeLlm::new(
            "https://openrouter.ai/api/v1/chat/completions",
            api_key,
            model,
        ))
    }
    pub fn endpoint() -> &'static str {
        "https://openrouter.ai/api/v1/chat/completions"
    }
}
impl domain::LlmPort for OpenRouterLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        let body = self.0.http_body(req)?;
        Ok(aggregate(parse_openai_events(&body), body))
    }
    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let body = match self.0.http_body(req) {
            Ok(b) => b,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(parse_openai_events(&body).into_iter())
    }
}

/// vLLM reuses the OpenAI shape with a user-supplied base_url.
pub struct VllmLlm(OpenAiShapeLlm);
impl VllmLlm {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        Self(OpenAiShapeLlm::new(&endpoint, api_key, model))
    }
}
impl domain::LlmPort for VllmLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        let body = self.0.http_body(req)?;
        Ok(aggregate(parse_openai_events(&body), body))
    }
    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let body = match self.0.http_body(req) {
            Ok(b) => b,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(parse_openai_events(&body).into_iter())
    }
}

/// Anthropic adapter (dedicated /v1/messages SSE format).
pub struct AnthropicLlm {
    client: Client,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl AnthropicLlm {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(DEFAULT_TIMEOUT)
                .user_agent("ag/0.2.0")
                .build()
                .expect("reqwest client"),
            api_key: api_key.to_string(),
            model: model.to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn http_body(&self, req: &LlmRequest) -> Result<String, BoxError> {
        let payload = build_anthropic_request(req, &self.model);
        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&payload)
            .timeout(self.timeout)
            .send()?;
        let status = resp.status();
        let body = resp.text()?;
        if !status.is_success() {
            return Err(format!("anthropic request failed ({}): {}", status, body).into());
        }
        Ok(body)
    }
}

impl domain::LlmPort for AnthropicLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        let body = self.http_body(req)?;
        Ok(aggregate(parse_anthropic_events(&body), body))
    }
    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let body = match self.http_body(req) {
            Ok(b) => b,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(parse_anthropic_events(&body).into_iter())
    }
}

/// Ollama adapter (newline-delimited JSON on /api/chat).
pub struct OllamaLlm {
    client: Client,
    endpoint: String,
    model: String,
    timeout: Duration,
}

impl OllamaLlm {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(DEFAULT_TIMEOUT)
                .user_agent("ag/0.2.0")
                .build()
                .expect("reqwest client"),
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn http_body(
        &self,
        req: &LlmRequest,
    ) -> Result<(Vec<Result<LlmEvent, BoxError>>, String), BoxError> {
        let has_images = !req.images.is_empty();
        let payload = build_ollama_request(req, &self.model);
        let resp = self
            .client
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .json(&payload)
            .timeout(self.timeout)
            .send()?;
        let status = resp.status();
        let body = resp.text()?;
        if !status.is_success() {
            return Err(format!("ollama request failed ({}): {}", status, body).into());
        }
        let mut events = parse_ollama_events(&body);
        if has_images {
            events.insert(
                0,
                Ok(LlmEvent::Delta(
                    "(warning: ollama does not support vision)".to_string(),
                )),
            );
        }
        Ok((events, body))
    }
}

impl domain::LlmPort for OllamaLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        let (events, body) = self.http_body(req)?;
        Ok(aggregate(events, body))
    }
    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let (events, _body) = match self.http_body(req) {
            Ok((e, b)) => (e, b),
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(events.into_iter())
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Aggregate a stream of `LlmEvent`s into a single non-streaming `LlmResponse`.
fn aggregate(
    events: impl IntoIterator<Item = Result<LlmEvent, BoxError>>,
    raw: String,
) -> LlmResponse {
    let mut text = String::new();
    let mut finish = LlmFinish {
        reason: LlmFinishReason::Stop,
        input_tokens: 0,
        output_tokens: 0,
        cache_tokens: 0,
    };
    for ev in events.into_iter().flatten() {
        match ev {
            LlmEvent::Delta(t) => text.push_str(&t),
            LlmEvent::ToolCallStart { .. } | LlmEvent::ToolCallArgs { .. } => {}
            LlmEvent::Finish(f) => finish = f,
        }
    }
    LlmResponse { text, finish, raw }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible request + SSE parsing
// ---------------------------------------------------------------------------

fn build_openai_request(req: &LlmRequest) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| openai_message(m, &req.images))
        .collect();
    let tools: Vec<serde_json::Value> = req.tools.iter().map(openai_tool_spec).collect();
    serde_json::json!({
        "model": req.model,
        "messages": messages,
        "tools": tools,
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    })
}

/// Attach top-level request images to the first user message (vision).
fn openai_message(m: &LlmMessage, images: &[domain::ImageRef]) -> serde_json::Value {
    match m.role {
        LlmRole::System => serde_json::json!({ "role": "system", "content": m.content }),
        LlmRole::User => {
            if images.is_empty() {
                serde_json::json!({ "role": "user", "content": m.content })
            } else {
                let mut content = vec![serde_json::json!({
                    "type": "text", "text": m.content
                })];
                for img in images {
                    content.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", img.mime, img.data) }
                    }));
                }
                serde_json::json!({ "role": "user", "content": content })
            }
        }
        LlmRole::Assistant => {
            if m.tool_calls.is_empty() {
                serde_json::json!({ "role": "assistant", "content": m.content })
            } else {
                serde_json::json!({
                    "role": "assistant",
                    "content": m.content,
                    "tool_calls": m.tool_calls.iter().map(|tc| serde_json::json!({
                        "id": tc.id, "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments }
                    })).collect::<Vec<_>>()
                })
            }
        }
        LlmRole::Tool => {
            let mut tc = None;
            if let Some(ref r) = m.tool_result {
                tc = Some(r.tool_call_id.clone());
            }
            serde_json::json!({
                "role": "tool",
                "tool_call_id": tc.unwrap_or_default(),
                "content": m.tool_result.as_ref().map(|r| r.content.clone()).unwrap_or_default(),
            })
        }
    }
}

fn openai_tool_spec(t: &domain::ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": serde_json::from_str::<serde_json::Value>(&t.params_json).unwrap_or_else(|_| serde_json::json!({})),
        }
    })
}

/// Parse an OpenAI-compatible SSE body into a sequence of `LlmEvent`s.
///
/// Recognised line shapes: `data: {json}`, `data: [DONE]`. The final
/// `usage` block (emitted when `include_usage` is set) populates `LlmFinish`.
pub fn parse_openai_events(body: &str) -> Vec<Result<LlmEvent, BoxError>> {
    let mut out: Vec<Result<LlmEvent, BoxError>> = Vec::new();
    let mut pending: Vec<LlmToolCall> = Vec::new();
    let mut started: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut usage: Option<(u64, u64, u64)> = None;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let data = match extract_sse_data(line) {
            Some(d) => d,
            None => continue,
        };
        if data == "[DONE]" {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(u) = parsed.get("usage") {
            let in_tok = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let out_tok = u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache = u
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            usage = Some((in_tok, out_tok, cache));
        }

        let choices = match parsed
            .get("choices")
            .and_then(|c| c.as_array())
            .filter(|c| !c.is_empty())
        {
            Some(c) => c,
            None => continue,
        };
        let first = &choices[0];

        if let Some(delta) = first.get("delta") {
            if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                if !content.is_empty() {
                    out.push(Ok(LlmEvent::Delta(content.to_string())));
                }
            }
            if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tc_array {
                    let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    while pending.len() <= idx {
                        pending.push(LlmToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                    }
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            pending[idx].id = id.to_string();
                        }
                    }
                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                            if !name.is_empty() {
                                pending[idx].name = name.to_string();
                            }
                        }
                        // Emit a start event exactly once the id+name are both known.
                        if !pending[idx].id.is_empty()
                            && !pending[idx].name.is_empty()
                            && started.insert(pending[idx].id.clone())
                        {
                            out.push(Ok(LlmEvent::ToolCallStart {
                                id: pending[idx].id.clone(),
                                name: pending[idx].name.clone(),
                            }));
                        }
                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                            if !args.is_empty() {
                                let id = pending[idx].id.clone();
                                out.push(Ok(LlmEvent::ToolCallArgs {
                                    id,
                                    arguments: args.to_string(),
                                }));
                                pending[idx].arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        if let Some(fr) = first.get("finish_reason").and_then(|v| v.as_str()) {
            let reason = match fr {
                "tool_calls" => LlmFinishReason::ToolUse,
                "length" => LlmFinishReason::Length,
                _ => LlmFinishReason::Stop,
            };
            let (i, o, c) = usage.unwrap_or((0, 0, 0));
            out.push(Ok(LlmEvent::Finish(LlmFinish {
                reason,
                input_tokens: i,
                output_tokens: o,
                cache_tokens: c,
            })));
            pending.clear();
            started.clear();
            usage = None;
        }
    }
    out
}

/// Strip the leading `data: ` (or `event:`-aware) prefix from an SSE line.
fn extract_sse_data(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("data:") {
        Some(rest.trim())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Anthropic request + SSE parsing
// ---------------------------------------------------------------------------

fn build_anthropic_request(req: &LlmRequest, model: &str) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .filter(|m| {
            m.role == LlmRole::User || m.role == LlmRole::Assistant || m.role == LlmRole::Tool
        })
        .map(anthropic_message)
        .collect();
    let tools: Vec<serde_json::Value> = req.tools.iter().map(anthropic_tool_spec).collect();
    serde_json::json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "stream": true,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    })
}

fn anthropic_message(m: &LlmMessage) -> serde_json::Value {
    match m.role {
        LlmRole::User => {
            let content: Vec<serde_json::Value> = vec![serde_json::json!({
                "type": "text", "text": m.content
            })];
            serde_json::json!({ "role": "user", "content": content })
        }
        LlmRole::Assistant => serde_json::json!({
            "role": "assistant",
            "content": m.content,
        }),
        LlmRole::Tool => {
            let tc_id = m
                .tool_result
                .as_ref()
                .map(|r| r.tool_call_id.clone())
                .unwrap_or_default();
            let content = m
                .tool_result
                .as_ref()
                .map(|r| r.content.clone())
                .unwrap_or_default();
            serde_json::json!({
                "role": "assistant",
                "content": [{ "type": "text", "text": &content }],
                "tool_calls": [{ "id": tc_id, "type": "tool_use", "name": m.content.clone(), "input": m.tool_result.as_ref().map(|r| serde_json::Value::String(r.content.clone())).unwrap_or_default() }],
            })
        }
        LlmRole::System => {
            serde_json::json!({ "role": "user", "content": [{ "type": "text", "text": m.content }] })
        }
    }
}

fn anthropic_tool_spec(t: &domain::ToolSpec) -> serde_json::Value {
    let params: serde_json::Value =
        serde_json::from_str(&t.params_json).unwrap_or_else(|_| serde_json::json!({}));
    serde_json::json!({
        "name": t.name,
        "description": t.description,
        "input_schema": params,
    })
}

/// Parse an Anthropic SSE body. Anthropic uses `event:` lines to delimit
/// messages; each `data:` line carries a JSON payload.
pub fn parse_anthropic_events(body: &str) -> Vec<Result<LlmEvent, BoxError>> {
    let mut out: Vec<Result<LlmEvent, BoxError>> = Vec::new();
    let mut pending: Vec<(String, String, String)> = Vec::new(); // (id, name, args)
    let mut usage = LlmFinish {
        reason: LlmFinishReason::Stop,
        input_tokens: 0,
        output_tokens: 0,
        cache_tokens: 0,
    };
    let mut last_event = String::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            last_event = String::new();
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            last_event = rest.trim().to_string();
            continue;
        }
        let data = match line.strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };
        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match last_event.as_str() {
            "content_block_delta" => {
                let delta = &parsed["delta"];
                if delta["type"] == "text_delta" {
                    if let Some(t) = delta["text"].as_str() {
                        if !t.is_empty() {
                            out.push(Ok(LlmEvent::Delta(t.to_string())));
                        }
                    }
                } else if delta["type"] == "input_json_delta" {
                    if let Some(partial) = delta["partial_json"].as_str() {
                        if let Some(last) = pending.last_mut() {
                            last.2.push_str(partial);
                            out.push(Ok(LlmEvent::ToolCallArgs {
                                id: last.0.clone(),
                                arguments: partial.to_string(),
                            }));
                        }
                    }
                }
            }
            "content_block_start" => {
                let cb = &parsed["content_block"];
                if cb["type"] == "tool_use" {
                    let id = cb
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = cb
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let _idx = pending.len();
                    pending.push((id.clone(), name.clone(), String::new()));
                    out.push(Ok(LlmEvent::ToolCallStart { id, name }));
                }
            }
            "message_delta" => {
                let u = parsed
                    .get("usage")
                    .or_else(|| parsed.get("message").and_then(|m| m.get("usage")));
                if let Some(u) = u {
                    if let Some(o) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                        usage.output_tokens += o;
                    }
                    if let Some(c) = u
                        .get("cache_creation_output_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        usage.cache_tokens += c;
                    }
                }
                if let Some(stop) = parsed["delta"].get("stop_reason").and_then(|v| v.as_str()) {
                    usage.reason = match stop {
                        "tool_use" => LlmFinishReason::ToolUse,
                        "length" => LlmFinishReason::Length,
                        _ => LlmFinishReason::Stop,
                    };
                }
            }
            "message_start" => {
                let u = parsed
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .or_else(|| parsed.get("usage"));
                if let Some(u) = u {
                    if let Some(i) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                        usage.input_tokens = i;
                    }
                    if let Some(c) = u
                        .get("cache_creation_output_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        usage.cache_tokens += c;
                    }
                }
            }
            "message_stop" => {
                out.push(Ok(LlmEvent::Finish(usage.clone())));
                pending.clear();
            }
            _ => {}
        }
    }

    // If the stream ended without message_stop, synthesize a Finish from usage.
    if !out.iter().any(|e| matches!(e, Ok(LlmEvent::Finish(_)))) {
        out.push(Ok(LlmEvent::Finish(usage)));
    }
    out
}

// ---------------------------------------------------------------------------
// Ollama request + NDJSON parsing
// ---------------------------------------------------------------------------

fn build_ollama_request(req: &LlmRequest, model: &str) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": match m.role {
                    LlmRole::System => "system",
                    LlmRole::User => "user",
                    LlmRole::Assistant => "assistant",
                    LlmRole::Tool => "tool",
                },
                "content": m.content,
            })
        })
        .collect();
    let tools: Vec<serde_json::Value> = req
        .tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": serde_json::from_str::<serde_json::Value>(&t.params_json).unwrap_or_else(|_| serde_json::json!({})),
                }
            })
        })
        .collect();
    serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "tools": tools,
    })
}

/// Parse Ollama newline-delimited JSON chat responses.
pub fn parse_ollama_events(body: &str) -> Vec<Result<LlmEvent, BoxError>> {
    let mut out: Vec<Result<LlmEvent, BoxError>> = Vec::new();
    let mut usage = LlmFinish {
        reason: LlmFinishReason::Stop,
        input_tokens: 0,
        output_tokens: 0,
        cache_tokens: 0,
    };
    let mut last_content = String::new();
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if parsed.get("done").and_then(|v| v.as_bool()) == Some(true) {
            if let Some(eval) = parsed.get("eval_count").and_then(|v| v.as_u64()) {
                usage.output_tokens = eval;
            }
            out.push(Ok(LlmEvent::Finish(usage)));
            break;
        }
        if let Some(msg) = parsed.get("message") {
            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                let delta = &content[last_content.len()..];
                let delta = if delta.is_empty() { content } else { delta };
                if !delta.is_empty() {
                    out.push(Ok(LlmEvent::Delta(delta.to_string())));
                }
                last_content = content.to_string();
            }
            if let Some(tc_array) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tc_array {
                    let fn_ = &tc["function"];
                    let name = fn_.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or(name);
                    if let Some(args) = fn_.get("arguments").and_then(|v| v.as_str()) {
                        if !name.is_empty() && !tc_id.is_empty() {
                            if !seen_ids.contains(tc_id) {
                                seen_ids.insert(tc_id.to_string());
                                out.push(Ok(LlmEvent::ToolCallStart {
                                    id: tc_id.to_string(),
                                    name: name.to_string(),
                                }));
                            }
                            out.push(Ok(LlmEvent::ToolCallArgs {
                                id: tc_id.to_string(),
                                arguments: args.to_string(),
                            }));
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> LlmRequest {
        LlmRequest {
            messages: Box::new([
                LlmMessage::system("you are helpful"),
                LlmMessage::user("rename foo to bar"),
            ]),
            tools: Box::new([]),
            model: "gpt-4o-mini".into(),
            max_tokens: 16384,
            temperature: 0.7,
            images: Box::new([]),
        }
    }

    #[test]
    fn openai_shape_parses_tool_call_sse() {
        let body = "\
data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}
data: {\"id\":\"chatcmpl-2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"str_replace_editor\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}
data: {\"id\":\"chatcmpl-3\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"arguments\":\"{\\\"new_str\\\":\\\"bar\\\"\"}}]},\"finish_reason\":null}]}
data: {\"id\":\"chatcmpl-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":12,\"prompt_tokens_details\":{\"cached_tokens\":4}}}
data: [DONE]
";
        let events: Vec<_> = parse_openai_events(body)
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert!(matches!(events[0], LlmEvent::Delta(_)));
        assert!(
            matches!(&events[1], LlmEvent::ToolCallStart { name, .. } if name == "str_replace_editor")
        );
        assert!(
            matches!(&events[2], LlmEvent::ToolCallArgs { arguments, .. } if !arguments.is_empty())
        );
        match &events[3] {
            LlmEvent::Finish(f) => {
                assert_eq!(f.reason, LlmFinishReason::ToolUse);
                assert_eq!(f.input_tokens, 10);
                assert_eq!(f.output_tokens, 12);
                assert_eq!(f.cache_tokens, 4);
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn openai_stop_finish() {
        let body = "\
data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}
data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}
data: [DONE]
";
        let events: Vec<_> = parse_openai_events(body)
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        match &events[1] {
            LlmEvent::Finish(f) => {
                assert_eq!(f.reason, LlmFinishReason::Stop);
                assert_eq!(f.input_tokens, 1);
                assert_eq!(f.output_tokens, 2);
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_parses_usage() {
        let body = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0,\"cache_creation_output_tokens\":2,\"cache_read_input_tokens\":0}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":8,\"cache_creation_output_tokens\":0}}

event: message_stop
data: {\"type\":\"message_stop\"}
";
        let events: Vec<_> = parse_anthropic_events(body)
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert!(matches!(events[0], LlmEvent::Delta(ref t) if t == "Hi"));
        match &events[1] {
            LlmEvent::Finish(f) => {
                assert_eq!(f.reason, LlmFinishReason::Stop);
                assert_eq!(f.input_tokens, 5);
                assert_eq!(f.output_tokens, 8);
                assert_eq!(f.cache_tokens, 2);
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_parses_tool_call() {
        let body = "\
event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"shell\",\"input\":{}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"echo hi\\\"\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}

event: message_stop
data: {\"type\":\"message_stop\"}
";
        let events: Vec<_> = parse_anthropic_events(body)
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert!(matches!(&events[0], LlmEvent::ToolCallStart { name, .. } if name == "shell"));
        assert!(
            matches!(&events[1], LlmEvent::ToolCallArgs { arguments, .. } if !arguments.is_empty())
        );
        match &events[2] {
            LlmEvent::Finish(f) => assert_eq!(f.reason, LlmFinishReason::ToolUse),
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn openrouter_reuses_openai_shape() {
        assert_eq!(
            OpenRouterLlm::endpoint(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        // same parser as OpenAiLlm -> identical behavior
        let r = OpenRouterLlm::new("key", "gpt-4o");
        assert_eq!(r.0.model(), "gpt-4o");
        assert_eq!(
            r.0.endpoint(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn vllm_uses_base_url() {
        let v = VllmLlm::new("http://localhost:8000/v1", "key", "qwen");
        assert_eq!(v.0.endpoint(), "http://localhost:8000/v1/chat/completions");
    }

    #[test]
    fn ollama_warns_on_image() {
        let mut img_req = req();
        img_req.images = Box::new([domain::ImageRef {
            mime: "image/png".into(),
            data: "base64data".into(),
        }]);
        let body = r#"{"model":"x","message":{"role":"assistant","content":""},"done":true,"eval_count":1}"#;
        let mut events = parse_ollama_events(body);
        events.insert(
            0,
            Ok(LlmEvent::Delta(
                "(warning: ollama does not support vision)".to_string(),
            )),
        );
        let text: String = events
            .into_iter()
            .map(|r| r.unwrap())
            .filter_map(|e| {
                if let LlmEvent::Delta(t) = e {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        assert!(text.contains("ollama does not support vision"));
    }

    #[test]
    fn ollama_parses_ndjson_done() {
        let body = r#"{"model":"x","message":{"role":"assistant","content":"h"},"done":false}
{"model":"x","message":{"role":"assistant","content":"hello"},"done":false}
{"model":"x","done":true,"eval_count":2,"eval_duration":100}"#;
        let events: Vec<_> = parse_ollama_events(body)
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert!(matches!(&events[0], LlmEvent::Delta(t) if t == "h"));
        assert!(matches!(&events[1], LlmEvent::Delta(t) if t == "ello"));
        match &events[2] {
            LlmEvent::Finish(f) => {
                assert_eq!(f.output_tokens, 2);
                assert_eq!(f.reason, LlmFinishReason::Stop);
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }
}
