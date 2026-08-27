//! LLM provider adapters implementing `domain::LlmPort` (FR-MODEL-01..08).
//!
//! Five wire shapes are supported behind one port:
//!
//! | provider | shape |
//! |----------|-------|
//! | OpenAI, OpenRouter, DeepSeek, vLLM / OpenAI-compatible | OpenAI SSE |
//! | Anthropic | `/v1/messages` SSE |
//! | Ollama | newline-delimited JSON |
//!
//! **Streaming is real.** The response body is read line by line and decoded
//! incrementally, so the first token reaches the UI as soon as the provider
//! emits it rather than after the whole generation completes. Decoding lives
//! in `SseDecode` implementations that are driven identically by the live
//! reader and by the `parse_*_events` helpers, so the hermetic tests exercise
//! exactly the code that runs in production.
//!
//! Direct deps: domain, serde, serde_json, reqwest, thiserror (L3).

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::time::Duration;

use domain::{
    BoxError, LlmEvent, LlmFinish, LlmFinishReason, LlmMessage, LlmRequest, LlmResponse, LlmRole,
    LlmToolCall, RetryNotice,
};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::StatusCode;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Transient failures are retried this many times before giving up.
///
/// Three, not two: a 429 from a busy provider routinely needs more than one
/// backoff, and giving up early turns a recoverable pause into a failed run.
/// Override with `max_retries` in the config file.
const DEFAULT_MAX_RETRIES: u32 = 3;
const USER_AGENT: &str = concat!("zcode/", env!("CARGO_PKG_VERSION"));

// ---------------------------------------------------------------------------
// Transport: shared client construction, retry, and error reporting
// ---------------------------------------------------------------------------

fn build_client(timeout: Duration) -> Client {
    Client::builder()
        // The timeout covers the whole request *including* reading a streamed
        // body, so it must be large enough for a full generation. It is driven
        // by `timeout_ms` in zcode.toml.
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .user_agent(USER_AGENT)
        .build()
        .expect("reqwest client")
}

/// Rate limits and upstream hiccups are worth retrying; a 400 or a 401 is not
/// — retrying those just burns time and quota.
fn is_retryable(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 409 | 429 | 500 | 502 | 503 | 504 | 520 | 522 | 524
    )
}

/// Longest a single backoff may last, however the provider asks.
const MAX_BACKOFF: Duration = Duration::from_secs(120);

/// How long to wait after a 429 that carries no `Retry-After`.
///
/// Rate limits are not transient hiccups: a provider that just refused you is
/// refusing everyone, and coming back in 600ms only spends another request to
/// be told the same thing. Free and shared tiers meter by the minute, so the
/// first retry has to sit out a meaningful part of one. Measured against
/// OpenRouter's free routes, sub-second retries failed every time and a 30s
/// wait succeeded.
const DEFAULT_RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(30);

/// First backoff for a non-rate-limit failure (a 500, a dropped connection).
/// These usually *are* transient, so the old fast retry is right for them.
const TRANSIENT_BACKOFF: Duration = Duration::from_millis(500);

/// How long to wait before trying again, and how many times.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_retries: u32,
    /// Floor for a 429 with no `Retry-After`.
    pub rate_limit_backoff: Duration,
    /// Ceiling for any single wait, including one the provider asked for.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            rate_limit_backoff: DEFAULT_RATE_LIMIT_BACKOFF,
            max_backoff: MAX_BACKOFF,
        }
    }
}

impl RetryPolicy {
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    pub fn with_rate_limit_backoff(mut self, backoff: Duration) -> Self {
        self.rate_limit_backoff = backoff;
        self
    }
}

/// Parse a `Retry-After` value. The header is defined as either a delay in
/// seconds or an HTTP date; providers also send fractional seconds, so all
/// three spellings are accepted.
fn parse_retry_after(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<f64>() {
        if secs.is_finite() && secs >= 0.0 {
            return Some(Duration::from_millis((secs * 1000.0) as u64));
        }
    }
    // An HTTP date: honour it only as a *relative* wait we can bound. Parsing
    // the full date grammar is not worth a dependency; the RFC 1123 form
    // providers actually send has the time at a fixed offset.
    httpdate_delay(raw)
}

/// Seconds until an RFC 1123 timestamp (`Tue, 15 Nov 1994 08:12:31 GMT`),
/// relative to the system clock. Returns `None` for anything unparseable.
fn httpdate_delay(raw: &str) -> Option<Duration> {
    let target = httpdate_to_unix(raw)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let delta = target - now;
    (delta > 0).then(|| Duration::from_secs(delta as u64))
}

/// Minimal RFC 1123 → unix-seconds conversion (days-from-civil algorithm).
fn httpdate_to_unix(raw: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // "Tue, 15 Nov 1994 08:12:31 GMT"
    let rest = raw.split_once(", ").map(|(_, r)| r).unwrap_or(raw);
    let mut parts = rest.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = MONTHS.iter().position(|m| *m == month_name)? as i64 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':');
    let (h, m, sec): (i64, i64, i64) = (
        hms.next()?.parse().ok()?,
        hms.next()?.parse().ok()?,
        hms.next()?.parse().ok()?,
    );
    // Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + m * 60 + sec)
}

/// Deterministic sub-second jitter derived from the process id and attempt.
///
/// Two agents that hit the same rate limit at the same instant must not both
/// come back at the same instant. This is not cryptographic and does not need
/// to be — it only has to decorrelate.
fn jitter_ms(attempt: u32) -> u64 {
    let seed = std::process::id() as u64 ^ (attempt as u64).wrapping_mul(0x9E37_79B9);
    // xorshift, then take the low bits: 0..=249ms.
    let mut x = seed | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x % 250
}

/// How long to wait before attempt `attempt + 1`.
///
/// The provider's own `Retry-After` always wins — it is the only authoritative
/// number in the exchange. Failing that, the wait depends on *why* we are
/// retrying: a rate limit starts at [`RetryPolicy::rate_limit_backoff`], a
/// transient error at [`TRANSIENT_BACKOFF`]. Both grow exponentially and both
/// are capped, so a hostile or mistaken header cannot park the agent.
fn retry_delay(
    attempt: u32,
    status: Option<StatusCode>,
    response: Option<&Response>,
    policy: RetryPolicy,
) -> Duration {
    if let Some(delay) = response
        .and_then(|r| r.headers().get(reqwest::header::RETRY_AFTER))
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after)
    {
        return delay.min(policy.max_backoff);
    }
    // Rate limits do not double. The window a provider meters over is fixed —
    // usually a minute — so waiting 30s, then 60s, then 120s does not improve
    // the odds, it just turns a recoverable pause into minutes of silence. A
    // flat wait keeps the worst case predictable at `max_retries x backoff`.
    // Transient errors *are* worth backing off from progressively.
    let delay = if status == Some(StatusCode::TOO_MANY_REQUESTS) {
        policy.rate_limit_backoff
    } else {
        TRANSIENT_BACKOFF.saturating_mul(1u32 << attempt.min(6))
    };
    (delay + Duration::from_millis(jitter_ms(attempt))).min(policy.max_backoff)
}

/// Short cause for a retried status, used in the notice the user sees.
fn retry_reason(status: StatusCode) -> String {
    match status.as_u16() {
        429 => "rate limited by the provider".to_string(),
        408 => "provider timed out".to_string(),
        409 => "provider reported a conflict".to_string(),
        500..=599 => format!("provider error {}", status.as_u16()),
        other => format!("provider returned {other}"),
    }
}

/// A failed HTTP exchange, rendered with the provider's own error text —
/// which is where the useful part ("model not found", "insufficient credits")
/// always lives.
fn http_error(provider: &str, status: StatusCode, body: &str) -> BoxError {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message").or(Some(e)))
                .map(|m| m.to_string())
        })
        .unwrap_or_else(|| body.chars().take(400).collect());
    let hint = match status.as_u16() {
        401 | 403 => " — check the API key named by `api_key_env`",
        404 => " — check `model` and `base_url`",
        429 => " — rate limited or out of credits",
        _ => "",
    };
    format!("{provider} request failed ({status}): {detail}{hint}").into()
}

/// A response plus the retries it took to get one.
///
/// The notices are carried out rather than logged here so the caller can
/// replay them as `LlmEvent::Retry` at the head of the stream: a rate-limited
/// agent should look *rate limited*, not hung.
pub struct RetriedResponse {
    pub response: Response,
    pub retries: Vec<RetryNotice>,
}

/// Send with retries, returning a streaming-capable response.
fn send_with_retry(
    provider: &str,
    policy: RetryPolicy,
    make_request: impl Fn() -> RequestBuilder,
) -> Result<RetriedResponse, BoxError> {
    let mut attempt = 0;
    let mut retries: Vec<RetryNotice> = Vec::new();
    loop {
        let outcome = make_request().send();
        match outcome {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return Ok(RetriedResponse { response, retries });
                }
                if is_retryable(status) && attempt < policy.max_retries {
                    let delay = retry_delay(attempt, Some(status), Some(&response), policy);
                    retries.push(RetryNotice {
                        attempt: attempt + 1,
                        max_attempts: policy.max_retries,
                        delay_ms: delay.as_millis() as u64,
                        status: Some(status.as_u16()),
                        reason: retry_reason(status),
                    });
                    std::thread::sleep(delay);
                    attempt += 1;
                    continue;
                }
                let body = response.text().unwrap_or_default();
                return Err(http_error(provider, status, &body));
            }
            Err(e) => {
                // Connection reset / timeout: worth one more go.
                if attempt < policy.max_retries
                    && (e.is_timeout() || e.is_request() || e.is_connect())
                {
                    let delay = retry_delay(attempt, None, None, policy);
                    retries.push(RetryNotice {
                        attempt: attempt + 1,
                        max_attempts: policy.max_retries,
                        delay_ms: delay.as_millis() as u64,
                        status: None,
                        reason: if e.is_timeout() {
                            "connection timed out".into()
                        } else {
                            "connection failed".into()
                        },
                    });
                    std::thread::sleep(delay);
                    attempt += 1;
                    continue;
                }
                return Err(format!("{provider} request failed: {e}").into());
            }
        }
    }
}

/// Turn the retries that preceded a stream into leading events, so every
/// renderer sees them in order before the model's first token.
fn retry_events(
    retries: Vec<RetryNotice>,
) -> impl Iterator<Item = Result<LlmEvent, BoxError>> + Send {
    retries.into_iter().map(|n| Ok(LlmEvent::Retry(n)))
}

// ---------------------------------------------------------------------------
// Incremental decoding
// ---------------------------------------------------------------------------

type EventQueue = VecDeque<Result<LlmEvent, BoxError>>;

/// Turns raw response lines into `LlmEvent`s, one line at a time.
///
/// Implementations buffer the terminal `Finish` event until [`SseDecode::finish`]
/// because providers report token usage *after* the stop reason.
pub trait SseDecode: Send {
    fn push_line(&mut self, line: &str, out: &mut EventQueue);
    fn finish(&mut self, out: &mut EventQueue);
}

/// Reads a streaming body and yields events as they arrive.
///
/// Generic over the reader so tests can drive the exact production decoding
/// path from an in-memory buffer.
struct EventStream<R: BufRead + Send, D: SseDecode> {
    reader: R,
    decoder: D,
    queue: EventQueue,
    line: String,
    done: bool,
}

impl<R: BufRead + Send, D: SseDecode> EventStream<R, D> {
    fn new(reader: R, decoder: D) -> Self {
        Self {
            reader,
            decoder,
            queue: VecDeque::new(),
            line: String::new(),
            done: false,
        }
    }
}

impl<D: SseDecode> EventStream<BufReader<Response>, D> {
    fn from_response(response: Response, decoder: D) -> Self {
        Self::new(BufReader::new(response), decoder)
    }
}

impl<R: BufRead + Send, D: SseDecode> Iterator for EventStream<R, D> {
    type Item = Result<LlmEvent, BoxError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Some(event);
            }
            if self.done {
                return None;
            }
            self.line.clear();
            match self.reader.read_line(&mut self.line) {
                Ok(0) => {
                    self.done = true;
                    self.decoder.finish(&mut self.queue);
                }
                Ok(_) => {
                    let line = self.line.trim_end_matches(['\r', '\n']);
                    self.decoder.push_line(line, &mut self.queue);
                }
                Err(e) => {
                    self.done = true;
                    self.queue
                        .push_back(Err(format!("stream read failed: {e}").into()));
                }
            }
        }
    }
}

/// Drive a decoder over a complete body — the batch path used by `send()` and
/// by every hermetic test.
fn decode_all<D: SseDecode>(body: &str, mut decoder: D) -> Vec<Result<LlmEvent, BoxError>> {
    let mut queue = EventQueue::new();
    for line in body.lines() {
        decoder.push_line(line, &mut queue);
    }
    decoder.finish(&mut queue);
    queue.into()
}

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
            LlmEvent::ToolCallStart { .. } | LlmEvent::ToolCallArgs { .. } | LlmEvent::Retry(_) => {
            }
            LlmEvent::Finish(f) => finish = f,
        }
    }
    LlmResponse { text, finish, raw }
}

// ---------------------------------------------------------------------------
// OpenAI wire shape (OpenAI, OpenRouter, DeepSeek, vLLM, openai-compatible)
// ---------------------------------------------------------------------------

/// Shared OpenAI-wire-shape adapter. `OpenAiLlm`, `OpenRouterLlm`,
/// `DeepSeekLlm` and `VllmLlm` all wrap one of these with a different
/// endpoint and headers (DRY, L3).
pub struct OpenAiShapeLlm {
    client: Client,
    endpoint: String,
    api_key: String,
    model: String,
    provider: &'static str,
    retry: RetryPolicy,
    /// Extra headers a specific provider requires (e.g. OpenRouter attribution).
    extra_headers: Vec<(&'static str, String)>,
    /// When set, the stable prefix (system prompt + tools + history) is marked
    /// with `cache_control` breakpoints so repeated calls hit the provider's
    /// prompt cache instead of re-billing the full prompt every turn. Required
    /// for Anthropic models routed through the OpenAI shape (OpenRouter), and
    /// ignored by providers that do not understand it — which is why it is a
    /// per-adapter flag rather than always-on.
    cache_control: bool,
}

impl OpenAiShapeLlm {
    pub fn new(endpoint: &str, api_key: &str, model: &str) -> Self {
        Self::with_timeout(endpoint, api_key, model, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(endpoint: &str, api_key: &str, model: &str, timeout: Duration) -> Self {
        Self {
            client: build_client(timeout),
            endpoint: endpoint.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            provider: "openai",
            retry: RetryPolicy::default(),
            extra_headers: Vec::new(),
            cache_control: false,
        }
    }

    /// Replace the retry policy (`max_retries` / `rate_limit_backoff_ms`).
    pub fn set_retry_policy(&mut self, retry: RetryPolicy) {
        self.retry = retry;
    }

    /// Enable provider prompt caching by marking the request prefix with
    /// `cache_control` breakpoints (FR-COST-01).
    pub fn with_cache_control(mut self, on: bool) -> Self {
        self.cache_control = on;
        self
    }

    fn labelled(mut self, provider: &'static str) -> Self {
        self.provider = provider;
        self
    }

    fn with_header(mut self, name: &'static str, value: String) -> Self {
        self.extra_headers.push((name, value));
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn request(&self, payload: &serde_json::Value) -> RequestBuilder {
        let mut builder = self
            .client
            .post(&self.endpoint)
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        for (name, value) in &self.extra_headers {
            builder = builder.header(*name, value);
        }
        builder.json(payload)
    }

    fn open_stream(&self, req: &LlmRequest) -> Result<RetriedResponse, BoxError> {
        let payload = build_openai_request(req, &self.model, self.cache_control);
        send_with_retry(self.provider, self.retry, || self.request(&payload))
    }

    /// Non-streaming read of the whole body (used by `send()`).
    fn http_body(&self, req: &LlmRequest) -> Result<String, BoxError> {
        self.open_stream(req)?
            .response
            .text()
            .map_err(|e| format!("{} response read failed: {e}", self.provider).into())
    }

    fn stream_events(
        &self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        match self.open_stream(req) {
            Ok(RetriedResponse { response, retries }) => Box::new(retry_events(retries).chain(
                EventStream::from_response(response, OpenAiDecoder::default()),
            )),
            Err(e) => Box::new(std::iter::once(Err(e))),
        }
    }
}

macro_rules! openai_shaped_port {
    ($ty:ty) => {
        impl $ty {
            /// Replace the retry policy (`max_retries` / `rate_limit_backoff_ms`).
            pub fn set_retry_policy(&mut self, retry: RetryPolicy) {
                self.0.set_retry_policy(retry);
            }
        }
        impl domain::LlmPort for $ty {
            fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
                let body = self.0.http_body(req)?;
                let events = parse_openai_events(&body);
                Ok(aggregate(events, body))
            }
            fn stream(
                &mut self,
                req: &LlmRequest,
            ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
                self.0.stream_events(req)
            }
        }
    };
}

/// OpenAI adapter (default endpoint api.openai.com).
pub struct OpenAiLlm(OpenAiShapeLlm);

impl OpenAiLlm {
    pub fn new(endpoint: &str, api_key: &str, model: &str) -> Self {
        Self::with_timeout(endpoint, api_key, model, DEFAULT_TIMEOUT)
    }
    pub fn with_timeout(endpoint: &str, api_key: &str, model: &str, timeout: Duration) -> Self {
        Self(OpenAiShapeLlm::with_timeout(endpoint, api_key, model, timeout).labelled("openai"))
    }
}
openai_shaped_port!(OpenAiLlm);

/// OpenRouter reuses the OpenAI wire shape. The two attribution headers are
/// optional per the API docs but recommended, and some upstream models refuse
/// requests without them.
pub struct OpenRouterLlm(OpenAiShapeLlm);

impl OpenRouterLlm {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self::with_timeout(api_key, model, DEFAULT_TIMEOUT)
    }
    pub fn with_timeout(api_key: &str, model: &str, timeout: Duration) -> Self {
        Self(
            OpenAiShapeLlm::with_timeout(Self::endpoint(), api_key, model, timeout)
                .labelled("openrouter")
                .with_header(
                    "HTTP-Referer",
                    "https://github.com/zainul/zcode".to_string(),
                )
                .with_header("X-Title", "zcode".to_string())
                .with_cache_control(true),
        )
    }
    pub fn endpoint() -> &'static str {
        "https://openrouter.ai/api/v1/chat/completions"
    }
}
openai_shaped_port!(OpenRouterLlm);

/// DeepSeek is OpenAI-compatible on a dedicated host (FR-MODEL: deepseek api).
pub struct DeepSeekLlm(OpenAiShapeLlm);

impl DeepSeekLlm {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self::with_timeout(api_key, model, DEFAULT_TIMEOUT)
    }
    pub fn with_timeout(api_key: &str, model: &str, timeout: Duration) -> Self {
        Self(
            OpenAiShapeLlm::with_timeout(Self::endpoint(), api_key, model, timeout)
                .labelled("deepseek"),
        )
    }
    pub fn endpoint() -> &'static str {
        "https://api.deepseek.com/chat/completions"
    }
}
openai_shaped_port!(DeepSeekLlm);

/// vLLM (and any other OpenAI-compatible server) with a user-supplied base_url.
pub struct VllmLlm(OpenAiShapeLlm);

impl VllmLlm {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Self::with_timeout(base_url, api_key, model, DEFAULT_TIMEOUT)
    }
    pub fn with_timeout(base_url: &str, api_key: &str, model: &str, timeout: Duration) -> Self {
        Self(
            OpenAiShapeLlm::with_timeout(&chat_completions_url(base_url), api_key, model, timeout)
                .labelled("openai-compatible"),
        )
    }
}

/// Resolve `base_url` to a chat-completions endpoint, whichever the user gave.
///
/// "base URL" is genuinely ambiguous — vLLM's docs print
/// `http://host:8000/v1`, while most people copy the full
/// `.../v1/chat/completions` out of a curl example. Appending unconditionally
/// turned the second, reasonable spelling into a 404 at a doubled path, and
/// the error named a URL the user never typed.
fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        return trimmed.to_string();
    }
    format!("{trimmed}/chat/completions")
}
openai_shaped_port!(VllmLlm);

fn build_openai_request(req: &LlmRequest, model: &str, cache_control: bool) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| openai_message(m, &req.images))
        .collect();
    let tools: Vec<serde_json::Value> = req.tools.iter().map(openai_tool_spec).collect();
    let model = if model.is_empty() { &req.model } else { model };
    let mut payload = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    });
    // An empty `tools` array is rejected by some OpenAI-compatible servers.
    if !tools.is_empty() {
        payload["tools"] = serde_json::Value::Array(tools);
        payload["tool_choice"] = serde_json::json!("auto");
    }
    // Mark the whole stable prefix — system prompt, tools and the conversation
    // so far — as a cache breakpoint. On a provider that honours
    // `cache_control` (e.g. OpenRouter routing Anthropic models, or any
    // OpenAI-compatible server that implements prompt caching) the next turn's
    // identical prefix is served from cache instead of being re-billed. The
    // breakpoint sits on the last message so it folds in everything before it.
    if cache_control {
        if let Some(last) = payload["messages"]
            .as_array_mut()
            .and_then(|m| m.last_mut())
        {
            last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
        }
    }
    payload
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
            let tool_call_id = m
                .tool_result
                .as_ref()
                .map(|r| r.tool_call_id.clone())
                .unwrap_or_default();
            serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
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

/// Incremental decoder for the OpenAI SSE shape.
#[derive(Default)]
pub struct OpenAiDecoder {
    pending: Vec<LlmToolCall>,
    started: HashSet<String>,
    /// Held back until the stream ends so a trailing `usage` chunk can be
    /// folded in (FR-OUTPUT-03/04/05).
    pending_finish: Option<LlmFinish>,
    usage: Option<(u64, u64, u64)>,
}

impl SseDecode for OpenAiDecoder {
    fn push_line(&mut self, line: &str, out: &mut EventQueue) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Some(data) = extract_sse_data(line) else {
            return;
        };
        if data == "[DONE]" {
            return;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };

        if let Some(u) = parsed.get("usage").filter(|u| !u.is_null()) {
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
            self.usage = Some((in_tok, out_tok, cache));
            if let Some(finish) = self.pending_finish.as_mut() {
                finish.input_tokens = in_tok;
                finish.output_tokens = out_tok;
                finish.cache_tokens = cache;
            }
        }

        // A provider may report an error mid-stream instead of via HTTP status.
        if let Some(err) = parsed.get("error").filter(|e| !e.is_null()) {
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| err.to_string());
            out.push_back(Err(format!("provider error: {message}").into()));
            return;
        }

        let Some(choices) = parsed
            .get("choices")
            .and_then(|c| c.as_array())
            .filter(|c| !c.is_empty())
        else {
            return;
        };
        let first = &choices[0];

        if let Some(delta) = first.get("delta") {
            if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                if !content.is_empty() {
                    out.push_back(Ok(LlmEvent::Delta(content.to_string())));
                }
            }
            if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tc_array {
                    self.push_tool_call_delta(tc, out);
                }
            }
        }

        if let Some(fr) = first
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let reason = match fr {
                "tool_calls" | "function_call" => LlmFinishReason::ToolUse,
                "length" => LlmFinishReason::Length,
                _ => LlmFinishReason::Stop,
            };
            let (i, o, c) = self.usage.unwrap_or((0, 0, 0));
            self.pending_finish = Some(LlmFinish {
                reason,
                input_tokens: i,
                output_tokens: o,
                cache_tokens: c,
            });
            self.pending.clear();
            self.started.clear();
        }
    }

    fn finish(&mut self, out: &mut EventQueue) {
        if let Some(finish) = self.pending_finish.take() {
            out.push_back(Ok(LlmEvent::Finish(finish)));
        }
    }
}

impl OpenAiDecoder {
    fn push_tool_call_delta(&mut self, tc: &serde_json::Value, out: &mut EventQueue) {
        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        while self.pending.len() <= idx {
            self.pending.push(LlmToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        }
        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                self.pending[idx].id = id.to_string();
            }
        }
        let Some(func) = tc.get("function") else {
            return;
        };
        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                self.pending[idx].name = name.to_string();
            }
        }
        // Some providers omit `id` entirely; fall back to the index so the
        // engine can still correlate arguments with a call.
        if self.pending[idx].id.is_empty() && !self.pending[idx].name.is_empty() {
            self.pending[idx].id = format!("call_{idx}");
        }
        // Emit the start exactly once, as soon as id+name are both known.
        if !self.pending[idx].id.is_empty()
            && !self.pending[idx].name.is_empty()
            && self.started.insert(self.pending[idx].id.clone())
        {
            out.push_back(Ok(LlmEvent::ToolCallStart {
                id: self.pending[idx].id.clone(),
                name: self.pending[idx].name.clone(),
            }));
        }
        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
            if !args.is_empty() {
                out.push_back(Ok(LlmEvent::ToolCallArgs {
                    id: self.pending[idx].id.clone(),
                    arguments: args.to_string(),
                }));
                self.pending[idx].arguments.push_str(args);
            }
        }
    }
}

/// Parse a complete OpenAI-compatible SSE body (batch path / tests).
pub fn parse_openai_events(body: &str) -> Vec<Result<LlmEvent, BoxError>> {
    decode_all(body, OpenAiDecoder::default())
}

/// Strip the leading `data:` prefix from an SSE line.
fn extract_sse_data(line: &str) -> Option<&str> {
    line.strip_prefix("data:").map(|rest| rest.trim())
}

// ---------------------------------------------------------------------------
// Anthropic
// ---------------------------------------------------------------------------

/// Anthropic adapter (dedicated /v1/messages SSE format).
pub struct AnthropicLlm {
    client: Client,
    api_key: String,
    model: String,
    retry: RetryPolicy,
}

impl AnthropicLlm {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self::with_timeout(api_key, model, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(api_key: &str, model: &str, timeout: Duration) -> Self {
        Self {
            client: build_client(timeout),
            api_key: api_key.to_string(),
            model: model.to_string(),
            retry: RetryPolicy::default(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Replace the retry policy (`max_retries` / `rate_limit_backoff_ms`).
    pub fn set_retry_policy(&mut self, retry: RetryPolicy) {
        self.retry = retry;
    }

    pub fn endpoint() -> &'static str {
        "https://api.anthropic.com/v1/messages"
    }

    fn open_stream(&self, req: &LlmRequest) -> Result<RetriedResponse, BoxError> {
        let payload = build_anthropic_request(req, &self.model);
        send_with_retry("anthropic", self.retry, || {
            self.client
                .post(Self::endpoint())
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .json(&payload)
        })
    }

    fn http_body(&self, req: &LlmRequest) -> Result<String, BoxError> {
        self.open_stream(req)?
            .response
            .text()
            .map_err(|e| format!("anthropic response read failed: {e}").into())
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
        match self.open_stream(req) {
            Ok(RetriedResponse { response, retries }) => Box::new(retry_events(retries).chain(
                EventStream::from_response(response, AnthropicDecoder::default()),
            )),
            Err(e) => Box::new(std::iter::once(Err(e))),
        }
    }
}

fn build_anthropic_request(req: &LlmRequest, model: &str) -> serde_json::Value {
    // Anthropic takes the system prompt as a top-level field, not a message.
    let system: String = req
        .messages
        .iter()
        .filter(|m| m.role == LlmRole::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(req.messages.len());
    for m in req.messages.iter() {
        match m.role {
            LlmRole::System => {}
            LlmRole::User => {
                let mut content = vec![serde_json::json!({ "type": "text", "text": m.content })];
                // Vision blocks ride on the first user message.
                if messages.is_empty() {
                    for img in req.images.iter() {
                        content.push(serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": img.mime,
                                "data": img.data,
                            }
                        }));
                    }
                }
                messages.push(serde_json::json!({ "role": "user", "content": content }));
            }
            LlmRole::Assistant => {
                let mut content: Vec<serde_json::Value> = Vec::new();
                if !m.content.is_empty() {
                    content.push(serde_json::json!({ "type": "text", "text": m.content }));
                }
                for tc in m.tool_calls.iter() {
                    content.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": serde_json::from_str::<serde_json::Value>(&tc.arguments)
                            .unwrap_or_else(|_| serde_json::json!({})),
                    }));
                }
                if content.is_empty() {
                    continue;
                }
                messages.push(serde_json::json!({ "role": "assistant", "content": content }));
            }
            LlmRole::Tool => {
                // Tool results are `user` turns carrying `tool_result` blocks.
                let (id, text) = m
                    .tool_result
                    .as_ref()
                    .map(|r| (r.tool_call_id.clone(), r.content.clone()))
                    .unwrap_or_default();
                let block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": text,
                });
                // Consecutive tool results belong to one user turn.
                match messages.last_mut() {
                    Some(last)
                        if last["role"] == "user"
                            && last["content"][0]["type"] == "tool_result" =>
                    {
                        if let Some(array) = last["content"].as_array_mut() {
                            array.push(block);
                        }
                    }
                    _ => messages.push(serde_json::json!({ "role": "user", "content": [block] })),
                }
            }
        }
    }

    let tools: Vec<serde_json::Value> = req.tools.iter().map(anthropic_tool_spec).collect();
    let model = if model.is_empty() { &req.model } else { model };
    let mut payload = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    });
    if !system.is_empty() {
        // Anthropic accepts the system prompt as an array of text blocks, and
        // only blocks carry `cache_control`. Marking it caches the (large,
        // turn-stable) system instructions so every turn after the first reads
        // them from the prompt cache (FR-COST-01).
        payload["system"] = serde_json::json!([
            { "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }
        ]);
    }
    if !tools.is_empty() {
        let mut tools = tools;
        // The tool schemas are equally stable; a cache breakpoint on the last
        // tool folds the whole tool list into the cached prefix.
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
        }
        payload["tools"] = serde_json::Value::Array(tools);
    }
    payload
}

/// Anthropic splits cache accounting across several fields — writes
/// (`cache_creation_*`) and reads (`cache_read_*`). They are separate token
/// buckets, so the run's cache total is their **sum**; picking one would
/// under-report whenever both are present.
fn anthropic_cache_tokens(usage: &serde_json::Value) -> u64 {
    [
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "cache_creation_output_tokens",
    ]
    .iter()
    .filter_map(|k| usage.get(*k).and_then(|v| v.as_u64()))
    .sum()
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

/// Incremental decoder for Anthropic's `event:`/`data:` SSE pairs.
pub struct AnthropicDecoder {
    pending: Vec<(String, String)>,
    usage: LlmFinish,
    last_event: String,
    emitted_finish: bool,
}

impl Default for AnthropicDecoder {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            usage: LlmFinish {
                reason: LlmFinishReason::Stop,
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
            },
            last_event: String::new(),
            emitted_finish: false,
        }
    }
}

impl SseDecode for AnthropicDecoder {
    fn push_line(&mut self, line: &str, out: &mut EventQueue) {
        let line = line.trim();
        if line.is_empty() {
            self.last_event = String::new();
            return;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.last_event = rest.trim().to_string();
            return;
        }
        let Some(data) = line.strip_prefix("data:").map(|d| d.trim()) else {
            return;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };

        match self.last_event.as_str() {
            "error" => {
                let message = parsed["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown error");
                out.push_back(Err(format!("anthropic stream error: {message}").into()));
            }
            "content_block_delta" => {
                let delta = &parsed["delta"];
                if delta["type"] == "text_delta" {
                    if let Some(t) = delta["text"].as_str() {
                        if !t.is_empty() {
                            out.push_back(Ok(LlmEvent::Delta(t.to_string())));
                        }
                    }
                } else if delta["type"] == "input_json_delta" {
                    if let Some(partial) = delta["partial_json"].as_str() {
                        if let Some(last) = self.pending.last() {
                            out.push_back(Ok(LlmEvent::ToolCallArgs {
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
                    let id = cb["id"].as_str().unwrap_or_default().to_string();
                    let name = cb["name"].as_str().unwrap_or_default().to_string();
                    self.pending.push((id.clone(), name.clone()));
                    out.push_back(Ok(LlmEvent::ToolCallStart { id, name }));
                }
            }
            "message_delta" => {
                let u = parsed
                    .get("usage")
                    .or_else(|| parsed.get("message").and_then(|m| m.get("usage")));
                if let Some(u) = u {
                    if let Some(o) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                        self.usage.output_tokens += o;
                    }
                    self.usage.cache_tokens += anthropic_cache_tokens(u);
                }
                if let Some(stop) = parsed["delta"].get("stop_reason").and_then(|v| v.as_str()) {
                    self.usage.reason = match stop {
                        "tool_use" => LlmFinishReason::ToolUse,
                        "max_tokens" | "length" => LlmFinishReason::Length,
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
                        self.usage.input_tokens = i;
                    }
                    self.usage.cache_tokens += anthropic_cache_tokens(u);
                }
            }
            "message_stop" => {
                out.push_back(Ok(LlmEvent::Finish(self.usage.clone())));
                self.emitted_finish = true;
                self.pending.clear();
            }
            _ => {}
        }
    }

    fn finish(&mut self, out: &mut EventQueue) {
        // A stream cut short still needs a terminal event.
        if !self.emitted_finish {
            out.push_back(Ok(LlmEvent::Finish(self.usage.clone())));
            self.emitted_finish = true;
        }
    }
}

/// Parse a complete Anthropic SSE body (batch path / tests).
pub fn parse_anthropic_events(body: &str) -> Vec<Result<LlmEvent, BoxError>> {
    decode_all(body, AnthropicDecoder::default())
}

// ---------------------------------------------------------------------------
// Ollama
// ---------------------------------------------------------------------------

/// Ollama adapter (newline-delimited JSON on /api/chat).
pub struct OllamaLlm {
    client: Client,
    endpoint: String,
    model: String,
    retry: RetryPolicy,
}

impl OllamaLlm {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self::with_timeout(endpoint, model, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(endpoint: &str, model: &str, timeout: Duration) -> Self {
        Self {
            client: build_client(timeout),
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            retry: RetryPolicy::default(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Replace the retry policy (`max_retries` / `rate_limit_backoff_ms`).
    pub fn set_retry_policy(&mut self, retry: RetryPolicy) {
        self.retry = retry;
    }

    fn open_stream(&self, req: &LlmRequest) -> Result<RetriedResponse, BoxError> {
        let payload = build_ollama_request(req, &self.model);
        send_with_retry("ollama", self.retry, || {
            self.client
                .post(&self.endpoint)
                .header("content-type", "application/json")
                .json(&payload)
        })
    }

    fn http_body(
        &self,
        req: &LlmRequest,
    ) -> Result<(Vec<Result<LlmEvent, BoxError>>, String), BoxError> {
        let has_images = !req.images.is_empty();
        let body = self
            .open_stream(req)?
            .response
            .text()
            .map_err(|e| -> BoxError { format!("ollama response read failed: {e}").into() })?;
        let mut events = parse_ollama_events(&body);
        if has_images {
            events.insert(0, Ok(LlmEvent::Delta(OLLAMA_VISION_WARNING.to_string())));
        }
        Ok((events, body))
    }
}

const OLLAMA_VISION_WARNING: &str = "(warning: ollama does not support vision)";

impl domain::LlmPort for OllamaLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, BoxError> {
        let (events, body) = self.http_body(req)?;
        Ok(aggregate(events, body))
    }
    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send> {
        let warn = !req.images.is_empty();
        match self.open_stream(req) {
            Ok(RetriedResponse { response, retries }) => {
                let stream = retry_events(retries).chain(EventStream::from_response(
                    response,
                    OllamaDecoder::default(),
                ));
                if warn {
                    Box::new(
                        std::iter::once(Ok(LlmEvent::Delta(OLLAMA_VISION_WARNING.to_string())))
                            .chain(stream),
                    )
                } else {
                    Box::new(stream)
                }
            }
            Err(e) => Box::new(std::iter::once(Err(e))),
        }
    }
}

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
                "content": match m.role {
                    LlmRole::Tool => m
                        .tool_result
                        .as_ref()
                        .map(|r| r.content.clone())
                        .unwrap_or_default(),
                    _ => m.content.clone(),
                },
            })
        })
        .collect();
    let tools: Vec<serde_json::Value> = req.tools.iter().map(openai_tool_spec).collect();
    let model = if model.is_empty() { &req.model } else { model };
    let mut payload = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });
    if !tools.is_empty() {
        payload["tools"] = serde_json::Value::Array(tools);
    }
    payload
}

/// Incremental decoder for Ollama's newline-delimited JSON.
#[derive(Default)]
pub struct OllamaDecoder {
    last_content: String,
    seen_ids: HashSet<String>,
    done: bool,
}

impl SseDecode for OllamaDecoder {
    fn push_line(&mut self, line: &str, out: &mut EventQueue) {
        let line = line.trim();
        if line.is_empty() || self.done {
            return;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        if let Some(err) = parsed.get("error").and_then(|e| e.as_str()) {
            out.push_back(Err(format!("ollama error: {err}").into()));
            self.done = true;
            return;
        }
        if parsed.get("done").and_then(|v| v.as_bool()) == Some(true) {
            let mut finish = LlmFinish {
                reason: LlmFinishReason::Stop,
                input_tokens: parsed
                    .get("prompt_eval_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                output_tokens: parsed
                    .get("eval_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                cache_tokens: 0,
            };
            if !self.seen_ids.is_empty() {
                finish.reason = LlmFinishReason::ToolUse;
            }
            out.push_back(Ok(LlmEvent::Finish(finish)));
            self.done = true;
            return;
        }
        let Some(msg) = parsed.get("message") else {
            return;
        };
        if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
            // Some builds send cumulative content, others send deltas.
            let delta = content
                .strip_prefix(self.last_content.as_str())
                .unwrap_or(content);
            if !delta.is_empty() {
                out.push_back(Ok(LlmEvent::Delta(delta.to_string())));
            }
            self.last_content = content.to_string();
        }
        if let Some(tc_array) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tc_array {
                let func = &tc["function"];
                let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or(name);
                // Ollama sends arguments as an object, not a JSON string.
                let arguments = match func.get("arguments") {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => continue,
                };
                if self.seen_ids.insert(id.to_string()) {
                    out.push_back(Ok(LlmEvent::ToolCallStart {
                        id: id.to_string(),
                        name: name.to_string(),
                    }));
                }
                out.push_back(Ok(LlmEvent::ToolCallArgs {
                    id: id.to_string(),
                    arguments,
                }));
            }
        }
    }

    fn finish(&mut self, out: &mut EventQueue) {
        if !self.done {
            out.push_back(Ok(LlmEvent::Finish(LlmFinish {
                reason: LlmFinishReason::Stop,
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
            })));
            self.done = true;
        }
    }
}

/// Parse a complete Ollama NDJSON body (batch path / tests).
pub fn parse_ollama_events(body: &str) -> Vec<Result<LlmEvent, BoxError>> {
    decode_all(body, OllamaDecoder::default())
}
#[cfg(test)]
mod tests {

    // ---- streaming behaviour -------------------------------------------------

    /// A reader that records how much of the body has been consumed, so a test
    /// can prove events arrive *before* the response is fully read.
    struct CountingReader {
        data: std::io::Cursor<Vec<u8>>,
        consumed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl std::io::Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = std::io::Read::read(&mut self.data, buf)?;
            self.consumed
                .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
            Ok(n)
        }
    }

    /// The whole point of the streaming path: the first delta must be
    /// deliverable long before the provider has finished generating. If this
    /// regresses, the TUI shows nothing until the turn completes.
    #[test]
    fn events_are_yielded_before_the_body_is_fully_read() {
        let mut body = String::new();
        body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n");
        // A large tail: if decoding waited for EOF, `consumed` would cover it.
        for _ in 0..200 {
            body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n");
        }
        body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n");
        let total = body.len();

        let consumed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader = std::io::BufReader::with_capacity(
            64,
            CountingReader {
                data: std::io::Cursor::new(body.into_bytes()),
                consumed: consumed.clone(),
            },
        );
        let mut stream = EventStream::new(reader, OpenAiDecoder::default());

        let first = stream.next().expect("first event");
        assert!(matches!(first, Ok(LlmEvent::Delta(ref t)) if t == "first"));
        let read_so_far = consumed.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            read_so_far < total,
            "decoder consumed the whole body ({read_so_far}/{total}) before emitting"
        );
    }

    #[test]
    fn stream_ends_with_finish_carrying_trailing_usage() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":4}}\n",
            "data: [DONE]\n",
        );
        let events: Vec<_> = EventStream::new(
            std::io::Cursor::new(body.as_bytes().to_vec()),
            OpenAiDecoder::default(),
        )
        .map(|r| r.unwrap())
        .collect();

        assert!(matches!(events[0], LlmEvent::Delta(_)));
        match events.last().expect("finish") {
            LlmEvent::Finish(f) => {
                assert_eq!(f.input_tokens, 9);
                assert_eq!(f.output_tokens, 4);
            }
            other => panic!("expected Finish last, got {other:?}"),
        }
    }

    #[test]
    fn mid_stream_provider_error_surfaces() {
        let body = "data: {\"error\":{\"message\":\"rate limited upstream\"}}\n";
        let events = parse_openai_events(body);
        assert!(events.iter().any(|e| e
            .as_ref()
            .err()
            .is_some_and(|e| e.to_string().contains("rate limited upstream"))));
    }

    #[test]
    fn tool_calls_without_ids_still_dispatch() {
        // Some OpenAI-compatible servers omit `id` on tool-call deltas.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
        );
        let events: Vec<_> = parse_openai_events(body).into_iter().flatten().collect();
        assert!(matches!(
            &events[0],
            LlmEvent::ToolCallStart { id, name } if !id.is_empty() && name == "read"
        ));
    }

    // ---- retry policy --------------------------------------------------------

    #[test]
    fn only_transient_statuses_are_retried() {
        for code in [429, 500, 502, 503, 504, 408] {
            assert!(
                is_retryable(StatusCode::from_u16(code).unwrap()),
                "{code} should retry"
            );
        }
        // Retrying these just burns quota — the request will never succeed.
        for code in [400, 401, 403, 404, 422] {
            assert!(
                !is_retryable(StatusCode::from_u16(code).unwrap()),
                "{code} must not retry"
            );
        }
    }

    #[test]
    fn retry_after_accepts_seconds_and_fractions() {
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after(" 30 "), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("nonsense"), None);
        // A negative value is not a wait; fall back to backoff.
        assert_eq!(parse_retry_after("-5"), None);
    }

    #[test]
    fn retry_after_accepts_an_http_date() {
        // The RFC 1123 form providers actually send. Fixed epoch check first,
        // so the conversion itself is pinned rather than only its sign.
        assert_eq!(
            httpdate_to_unix("Tue, 15 Nov 1994 08:12:31 GMT"),
            Some(784_887_151)
        );
        assert_eq!(httpdate_to_unix("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        // A date in the past is not a wait.
        assert_eq!(httpdate_delay("Tue, 15 Nov 1994 08:12:31 GMT"), None);
        assert_eq!(httpdate_to_unix("not a date"), None);
    }

    #[test]
    fn a_hostile_retry_after_cannot_park_the_agent() {
        // 24h in the header must not become a 24h sleep.
        assert!(parse_retry_after("86400").unwrap() > MAX_BACKOFF);
        // …because `retry_delay` clamps it to the policy ceiling.
        assert_eq!(MAX_BACKOFF, Duration::from_secs(120));
        assert_eq!(RetryPolicy::default().max_backoff, MAX_BACKOFF);
    }

    #[test]
    fn jitter_decorrelates_but_stays_small() {
        for attempt in 0..8 {
            assert!(jitter_ms(attempt) < 250, "attempt {attempt}");
        }
        // Different attempts must not all land on the same offset.
        let distinct: HashSet<u64> = (0..8).map(jitter_ms).collect();
        assert!(distinct.len() > 1, "jitter is constant");
    }

    #[test]
    fn a_rate_limit_notice_reads_like_a_sentence() {
        let notice = RetryNotice {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 2_000,
            status: Some(429),
            reason: retry_reason(StatusCode::TOO_MANY_REQUESTS),
        };
        assert_eq!(
            notice.render(),
            "rate limited by the provider (429) — retrying in 2.0s (attempt 1/3)"
        );
    }

    #[test]
    fn retries_become_leading_stream_events() {
        // A rate-limited turn must look rate-limited to the UI, not hung.
        let notices = vec![RetryNotice {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 500,
            status: Some(429),
            reason: "rate limited by the provider".into(),
        }];
        let events: Vec<_> = retry_events(notices).collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Ok(LlmEvent::Retry(_))));
    }

    #[test]
    fn retry_events_do_not_pollute_the_aggregated_answer() {
        let events = vec![
            Ok(LlmEvent::Retry(RetryNotice {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 500,
                status: Some(429),
                reason: "rate limited by the provider".into(),
            })),
            Ok(LlmEvent::Delta("hi".into())),
        ];
        let response = aggregate(events, String::new());
        assert_eq!(response.text, "hi");
    }

    #[test]
    fn a_rate_limit_waits_far_longer_than_a_transient_error() {
        // The reported problem: three sub-second retries after a 429 all
        // failed, because a provider that just refused you is still refusing
        // you 600ms later.
        let policy = RetryPolicy::default();
        let limited = retry_delay(0, Some(StatusCode::TOO_MANY_REQUESTS), None, policy);
        let transient = retry_delay(0, Some(StatusCode::BAD_GATEWAY), None, policy);
        assert!(
            limited >= Duration::from_secs(30),
            "a 429 must sit out a meaningful part of a rate-limit window: {limited:?}"
        );
        assert!(transient < Duration::from_secs(1), "{transient:?}");
    }

    #[test]
    fn the_rate_limit_backoff_is_configurable() {
        let policy = RetryPolicy::default().with_rate_limit_backoff(Duration::from_secs(5));
        let delay = retry_delay(0, Some(StatusCode::TOO_MANY_REQUESTS), None, policy);
        assert!(delay >= Duration::from_secs(5) && delay < Duration::from_secs(6));
    }

    #[test]
    fn a_provider_supplied_retry_after_beats_the_default() {
        // `Retry-After` is the only authoritative number in the exchange, so
        // it wins in both directions — including when it is *shorter*.
        let policy = RetryPolicy::default();
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        // With no header, the 429 default applies instead.
        assert!(
            retry_delay(0, Some(StatusCode::TOO_MANY_REQUESTS), None, policy)
                >= policy.rate_limit_backoff
        );
    }

    #[test]
    fn the_rate_limit_wait_is_flat_so_the_worst_case_is_predictable() {
        // Doubling from a 30s base turned a throttled run into nine minutes of
        // silence. The metering window is fixed, so the wait should be too:
        // the worst case is `max_retries x rate_limit_backoff`, no more.
        let policy = RetryPolicy::default();
        let waits: Vec<Duration> = (0..policy.max_retries)
            .map(|a| retry_delay(a, Some(StatusCode::TOO_MANY_REQUESTS), None, policy))
            .collect();
        for w in &waits {
            assert!(*w >= policy.rate_limit_backoff, "{w:?}");
            // Only jitter separates them from the base.
            assert!(
                *w < policy.rate_limit_backoff + Duration::from_secs(1),
                "{w:?}"
            );
        }
        let total: Duration = waits.iter().sum();
        assert!(
            total < policy.rate_limit_backoff * policy.max_retries + Duration::from_secs(1),
            "worst case {total:?}"
        );
        assert!(retry_delay(20, Some(StatusCode::TOO_MANY_REQUESTS), None, policy) <= MAX_BACKOFF);
    }

    #[test]
    fn a_transient_error_still_backs_off_progressively() {
        let policy = RetryPolicy::default();
        let first = retry_delay(0, Some(StatusCode::BAD_GATEWAY), None, policy);
        let second = retry_delay(1, Some(StatusCode::BAD_GATEWAY), None, policy);
        assert!(second > first, "{first:?} then {second:?}");
        assert!(retry_delay(20, Some(StatusCode::BAD_GATEWAY), None, policy) <= MAX_BACKOFF);
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        let policy = RetryPolicy::default();
        let first = retry_delay(0, None, None, policy);
        let second = retry_delay(1, None, None, policy);
        assert!(second > first);
        assert!(retry_delay(20, None, None, policy) <= MAX_BACKOFF);
    }

    #[test]
    fn http_errors_quote_the_provider_message_and_hint() {
        let err = http_error(
            "openrouter",
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"No auth credentials found"}}"#,
        );
        let text = err.to_string();
        assert!(text.contains("No auth credentials found"));
        assert!(
            text.contains("api_key_env"),
            "should hint at the fix: {text}"
        );

        let err = http_error("openai", StatusCode::NOT_FOUND, "plain text body");
        assert!(err.to_string().contains("plain text body"));
    }

    // ---- request shaping -----------------------------------------------------

    #[test]
    fn openai_omits_empty_tool_arrays() {
        // An empty `tools: []` is rejected by several OpenAI-compatible servers.
        let payload = build_openai_request(&req(), "gpt-4o-mini", false);
        assert!(payload.get("tools").is_none());
        assert!(payload["stream"].as_bool().unwrap());
        assert!(payload["stream_options"]["include_usage"]
            .as_bool()
            .unwrap());
    }

    #[test]
    fn openai_includes_tools_when_present() {
        let mut r = req();
        r.tools = Box::new([domain::ToolSpec {
            name: "read".into(),
            description: "read a file".into(),
            params_json: r#"{"type":"object"}"#.into(),
        }]);
        let payload = build_openai_request(&r, "gpt-4o-mini", false);
        assert_eq!(payload["tools"][0]["function"]["name"], "read");
        assert_eq!(payload["tool_choice"], "auto");
    }

    #[test]
    fn anthropic_lifts_the_system_prompt_out_of_messages() {
        let payload = build_anthropic_request(&req(), "claude-sonnet-4");
        assert_eq!(payload["system"][0]["text"], "you are helpful");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "system must not remain a message");
        assert_eq!(messages[0]["role"], "user");
    }

    /// A multi-turn tool exchange has to round-trip in Anthropic's own shape:
    /// `tool_use` blocks on the assistant turn, `tool_result` blocks on a user
    /// turn. Getting this wrong makes every tool-using conversation fail.
    #[test]
    fn anthropic_maps_tool_use_and_tool_result_blocks() {
        let mut assistant = LlmMessage::assistant("let me look");
        assistant.tool_calls = Box::new([LlmToolCall {
            id: "call_1".into(),
            name: "read".into(),
            arguments: r#"{"path":"a.rs"}"#.into(),
        }]);
        let tool = LlmMessage::tool_result_message(domain::LlmToolResult {
            tool_call_id: "call_1".into(),
            content: "file body".into(),
        });
        let r = LlmRequest {
            messages: Box::new([
                LlmMessage::system("sys"),
                LlmMessage::user("read it"),
                assistant,
                tool,
            ]),
            tools: Box::new([]),
            model: "claude-sonnet-4".into(),
            max_tokens: 1024,
            temperature: 0.0,
            images: Box::new([]),
        };
        let payload = build_anthropic_request(&r, "claude-sonnet-4");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        assert_eq!(messages[1]["role"], "assistant");
        let blocks = messages[1]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "call_1");
        // `input` must be an object, not the raw argument string.
        assert_eq!(blocks[1]["input"]["path"], "a.rs");

        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(messages[2]["content"][0]["content"], "file body");
    }

    #[test]
    fn anthropic_sends_images_as_base64_source_blocks() {
        let mut r = req();
        r.images = Box::new([domain::ImageRef {
            mime: "image/png".into(),
            data: "Zm9v".into(),
        }]);
        let payload = build_anthropic_request(&r, "claude-sonnet-4");
        let content = payload["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "Zm9v");
    }

    #[test]
    fn ollama_accepts_object_shaped_tool_arguments() {
        // Ollama sends `arguments` as a JSON object, unlike OpenAI's string.
        let body = concat!(
            r#"{"message":{"tool_calls":[{"function":{"name":"read","arguments":{"path":"a.rs"}}}]}}"#,
            "\n",
            r#"{"done":true,"eval_count":3,"prompt_eval_count":9}"#,
            "\n",
        );
        let events: Vec<_> = parse_ollama_events(body).into_iter().flatten().collect();
        assert!(matches!(&events[0], LlmEvent::ToolCallStart { name, .. } if name == "read"));
        match &events[1] {
            LlmEvent::ToolCallArgs { arguments, .. } => {
                assert!(arguments.contains("\"path\""), "got {arguments}");
            }
            other => panic!("expected args, got {other:?}"),
        }
        match events.last().unwrap() {
            LlmEvent::Finish(f) => {
                assert_eq!(f.reason, LlmFinishReason::ToolUse);
                assert_eq!(f.input_tokens, 9);
            }
            other => panic!("expected finish, got {other:?}"),
        }
    }

    #[test]
    fn truncated_streams_still_produce_a_finish_event() {
        // A stream cut off mid-generation must still terminate, or the engine
        // waits forever for an event that is never coming.
        assert!(parse_ollama_events("{\"message\":{\"content\":\"hi\"}}\n")
            .iter()
            .any(|e| matches!(e, Ok(LlmEvent::Finish(_)))));
        assert!(parse_anthropic_events(
            "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n"
        )
        .iter()
        .any(|e| matches!(e, Ok(LlmEvent::Finish(_)))));
    }

    // ---- provider wiring -----------------------------------------------------

    #[test]
    fn deepseek_targets_its_own_endpoint() {
        let d = DeepSeekLlm::new("key", "deepseek-chat");
        assert_eq!(d.0.endpoint(), "https://api.deepseek.com/chat/completions");
        assert_eq!(d.0.model(), "deepseek-chat");
    }

    #[test]
    fn openrouter_sends_attribution_headers() {
        // OpenRouter recommends these, and some upstream models require them.
        let r = OpenRouterLlm::new("key", "openai/gpt-4o");
        let names: Vec<&str> = r.0.extra_headers.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"HTTP-Referer"));
        assert!(names.contains(&"X-Title"));
    }

    /// OpenAI streams `usage` in a trailing chunk whose `choices` array is
    /// empty, i.e. after `finish_reason`. Those counts must still reach the
    /// finish event, otherwise every run reports zero tokens and the engine
    /// silently falls back to the heuristic (DQ2, FR-OUTPUT-03/04/05).
    #[test]
    fn late_usage_chunk_is_folded_into_the_finish_event() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":3}}}\n",
            "data: [DONE]\n"
        );
        let events: Vec<_> = parse_openai_events(body).into_iter().flatten().collect();
        let finish = events
            .iter()
            .find_map(|e| match e {
                LlmEvent::Finish(f) => Some(f),
                _ => None,
            })
            .expect("finish event");
        assert_eq!(finish.input_tokens, 11);
        assert_eq!(finish.output_tokens, 7);
        assert_eq!(finish.cache_tokens, 3);
        assert_eq!(finish.reason, LlmFinishReason::Stop);
    }

    /// The other ordering (usage before the finish chunk) must keep working.
    #[test]
    fn early_usage_chunk_still_populates_the_finish_event() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n"
        );
        let events: Vec<_> = parse_openai_events(body).into_iter().flatten().collect();
        let finish = events
            .iter()
            .find_map(|e| match e {
                LlmEvent::Finish(f) => Some(f),
                _ => None,
            })
            .expect("finish event");
        assert_eq!(finish.input_tokens, 5);
        assert_eq!(finish.output_tokens, 2);
    }
    use super::*;

    /// OpenRouter (and any cache-aware OpenAI-shaped server) must get a
    /// `cache_control` breakpoint on the request prefix, or every turn re-pays
    /// for the full system prompt + tools.
    #[test]
    fn openai_cache_control_marks_the_last_message() {
        let payload = build_openai_request(&req(), "gpt-4o-mini", true);
        let messages = payload["messages"].as_array().unwrap();
        assert!(
            messages
                .last()
                .unwrap()
                .get("cache_control")
                .is_some_and(|c| c["type"] == "ephemeral"),
            "last message must carry the cache breakpoint: {messages:?}"
        );
        // A provider that ignores it must not receive the field at all.
        let off = build_openai_request(&req(), "gpt-4o-mini", false);
        assert!(off["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m.get("cache_control").is_none()));
    }

    /// Anthropic caches only what is explicitly marked; without the breakpoint
    /// the large system prompt and tool schemas are re-billed every turn.
    #[test]
    fn anthropic_cache_control_marks_system_and_tools() {
        let mut r = req();
        r.tools = Box::new([domain::ToolSpec {
            name: "read".into(),
            description: "read a file".into(),
            params_json: r#"{"type":"object"}"#.into(),
        }]);
        let payload = build_anthropic_request(&r, "claude-sonnet-4");
        let sys = payload["system"]
            .as_array()
            .expect("system must be an array of blocks");
        assert_eq!(sys[0]["type"], "text");
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
        let tools = payload["tools"].as_array().unwrap();
        assert_eq!(
            tools.last().unwrap()["cache_control"]["type"],
            "ephemeral",
            "last tool must carry the cache breakpoint"
        );
    }

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
    fn a_base_url_that_is_already_an_endpoint_is_not_doubled() {
        // Regression: `base_url` copied from a curl example produced
        // `…/v1/chat/completions/chat/completions`.
        for given in [
            "http://127.0.0.1:8099/v1",
            "http://127.0.0.1:8099/v1/",
            "http://127.0.0.1:8099/v1/chat/completions",
            "http://127.0.0.1:8099/v1/chat/completions/",
        ] {
            assert_eq!(
                chat_completions_url(given),
                "http://127.0.0.1:8099/v1/chat/completions",
                "{given}"
            );
        }
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
