# Task 12 — Infra: LLM Multi-Provider Clients (OpenAI, Anthropic, OpenRouter, Ollama)

**Related PRD sections:** §3.4 Model & Provider Agnosticism (FR-MODEL-01..08), §3.6 Telemetry (FR-OUTPUT-03/04/05), §8 DQ3 (HTTP) / DQ2 (token counting)
**Depends on:** task-02 (Domain — evolved `LlmPort` from §4.1 of the technical plan), task-20 (config `Provider` + `api_key_env`)
**Status:** Done
**Priority:** High (the engine loop cannot run without a working `LlmPort`)

## Objective

Replace the v0.1 `OpenAiLlm` stub (which returns `Err("llm network disabled")`) with four real, config-driven provider adapters implementing the **evolved** `domain::LlmPort` (`send`/`stream` over `LlmRequest`/`LlmEvent`). Each adapter must stream Server-Sent Events (or newline-delimited JSON) and surface **provider-reported** token usage in `LlmEvent::Finish` (DQ2). Provider dispatch lives in the `wire()` composition root (task-17), but each adapter is independently unit-testable against a local HTTP server.

## Step-by-step

### 1. `crates/infra/llm/Cargo.toml`

```toml
[dependencies]
domain = { path = "../../domain", version = "0.1.0" }
serde = { workspace = true }
serde_json = { workspace = true }
reqwest = { workspace = true }     # blocking + rustls + json + gzip
thiserror = { workspace = true }
```

### 2. Evolve `LlmPort` impl (the domain trait changes are in task-02/domain-update; this task consumes it)

Each adapter impls:
```rust
impl domain::LlmPort for OpenAiLlm {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, Box<dyn Error>>;
    fn stream(&mut self, req: &LlmRequest)
        -> Box<dyn Iterator<Item = Result<LlmEvent, Box<dyn Error>>> + Send>;
}
```

### 3. Provider adapters

**`OpenAiLlm`** — endpoint `https://api.openai.com/v1/chat/completions` (or `base_url` override from config for vLLM / OpenAI-compatible). Bearer token from `resolve_api_key`. POST JSON: `model`, `messages`, `tools`, `stream: true`, `max_tokens`, `temperature`. Parse SSE `data: ...` lines: `choices[].delta.content`, `choices[].delta.tool_calls[]`, `choices[].finish_reason`, `usage` (streamed on the final line or as a separate `data: [DONE]` usage object). Map `finish_reason` `tool_calls` → `LlmFinishReason::ToolUse`.

**`AnthropicLlm`** — endpoint `https://api.anthropic.com/v1/messages`, `x-api-key` header. POST JSON with `stream: true`. Parse SSE events: `event: content_block.delta` (text delta) and `content_block.start` for tool use, `event: message_stop` with `usage` (Anthropic emits `cache_creation_output_tokens`). Map `stop_reason: "tool_use"` → `ToolUse`.

**`OpenRouterLlm`** — reuses the **exact same request/response shape as OpenAI** (OpenRouter is OpenAI-compatible) with endpoint `https://openrouter.ai/api/v1/chat/completions` and `Authorization: Bearer <key>`. **Refactor:** extract `OpenAiShapeLlm` (the OpenAI-wire impl) and have both `OpenAiLlm` and `OpenRouterLlm` wrap it with a different `endpoint` + label. vLLM (`Provider::Vllm`) reuses `OpenAiShapeLlm` with a user-supplied `base_url`. This satisfies FR-MODEL-03/04/05 with **one** transport + per-provider endpoint strings (DRY, L3 edge count).

**`OllamaLlm`** — endpoint `http://localhost:11434/api/chat`. Ollama's `/api/chat` returns **newline-delimited JSON**, not SSE: each `{"message": {...}, "done": false, ...}` line, final `{"done": true, "eval_count": N, "eval_duration": ...}` carries usage. For Ollama:
- `tools` field maps OpenAI-style `tool` functions; emit deltas from `message.content`/partial `tool_calls`.
- Multimodal is **text-only**; if `--image` is passed to an Ollama model, emit a warning-level `LlmEvent::Delta` (FR-MODEL-08) and continue (do **not** hard-fail).
- Token counts: Ollama's `eval_count` is output tokens; input tokens are not directly reported → fall back to `domain::tokens::estimate_tokens` on the concatenated prompt (DQ2 fallback).

### 4. Shared SSE/NDJSON driver

Private helper `fn sse_lines(resp: reqwest::blocking::Response) -> impl Iterator<Item=Result<String>>` that yields each `data:` line. A second helper `parse_openai_events(lines) -> impl Iterator<Item=LlmEvent>` is shared by OpenAI/OpenRouter/vLLM.

### 5. `LlmResponse` (non-streaming `send`)

Aggregate the stream into a single `LlmResponse { text, finish, raw }` for any caller that wants blocking full-response mode (useful for tests/fakes in task-16).

### 6. Tests

Hermetic (no network):
- `openai_shape_parses_tool_call_sse`: feed a canned SSE sequence to the stream parser; assert it emits `Delta`, `ToolCallStart`, `ToolCallArgs`, then `Finish` with `ToolUse` and the right token counts.
- `anthropic_parses_usage`: canned `event: message_delta` with `usage` → assert `input_tokens`/`output_tokens`/`cache_tokens` populated.
- `openrouter_reuses_openai_shape`: assert `OpenRouterLlm::endpoint == "https://openrouter.ai/api/v1/chat/completions"` and its `send`/`stream` delegate to the shared shape (behavior identical).
- `vllm_uses_base_url`: `base_url = "http://localhost:8000/v1"` → `OpenAiLlm` posts there.
- `ollama_warns_on_image`: construct `LlmRequest` with an `ImageRef`, call `stream`, drain iterator, assert a `Delta` containing `"(warning: ollama does not support vision)"`.

Integration (network, `#[ignore]`):
- `live_openai_streams_tokens`: `#[ignore]` against a real key; smoke-test only.

## Test-case scenario

- `wire()` dispatches `Provider::Anthropic` to `AnthropicLlm` pointing at `https://api.anthropic.com/v1/messages`; streaming a 1-turn no-tool request emits `Delta` + `Finish(Stop)` with `input_tokens > 0`.
- An OpenAI-style `tool_calls` finish emits `Finish(ToolUse)` with populated `ToolCallStart`/`ToolCallArgs` events, which the task-16 loop consumes to dispatch tools.

## How to verify

```
cargo test -p infra-llm            # all hermetic unit tests green
cargo test -p infra-llm -- --ignored  # optional: live smoke (needs keys)
cargo clippy -p infra-llm -- -D warnings
cargo tree -p infra-llm             # deps: domain, serde, serde_json, reqwest, thiserror (>=L3)
```

**Pass criteria:** all non-ignored tests pass; SSE/NDJSON parsing correctly maps `finish_reason`→`LlmFinishReason` and populates token counts from provider `usage`; no `unsafe`; `cargo tree -p infra-llm` shows exactly `{domain, serde, serde_json, reqwest, thiserror}`.

## Success metric mapping

- M1.2 (tests), M1.3 (clippy), M1.4 (acyclic), FR-MODEL-01..08, FR-OUTPUT-03/04/05 (token reporting), DQ2 (provider-reported usage), L3 (OpenAI/OpenRouter/vLLM share one transport), NFR-PERF-04 (single-threaded blocking client, no thread-per-request).

## Notes / risks

- `reqwest` blocking is acceptable because app is synchronous (DQ4) and the TUI offloads the loop to a worker thread (task-17). If concurrent in-flight requests become needed (v0.3.0), switch to `reqwest::Client` async behind an async app.
- Anthropic's `usage` on stream arrives on `message_delta`/`message_stop`; if absent, fall back to the whitespace heuristic.
