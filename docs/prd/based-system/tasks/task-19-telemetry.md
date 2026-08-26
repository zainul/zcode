# Task 19 — Telemetry: JSONL Emitter + Report File Schema

**Related PRD sections:** §3.6 Structured Output & Telemetry (FR-OUTPUT-01..09), §5.6 Observability (NFR-OBS-01/02), §8 DQ2 (token counting), §7 M1.7/M2.6
**Depends on:** task-02 (Domain — `TelemetryEvent`/`TelemetryTotals`/`TelemetryEvent.extra` defined in §4.4 of the technical plan)
**Status:** Done
**Priority:** High (machine-readable output + cost attribution is a hard requirement US-E-08 / G6)

## Objective

Implement `crates/infra/telemetry` with `JsonTelemetry` that (a) streams one JSON object per event to stdout in headless `--json` mode (FR-OUTPUT-01, NFR-OBS-01) and (b) accumulates totals into `.zcode/reports/<timestamp>-<session>.json` (FR-OUTPUT-02, NFR-OBS-02). The schema is the documented success metric for M1.7. Domain stays dep-free by carrying `ExtraField` (domain enum) instead of `serde_json::Value`.

## Step-by-step

### 1. New crate `crates/infra/telemetry`

`Cargo.toml`:
```toml
[dependencies]
domain = { path = "../../domain", version = "0.1.0" }
serde_json = { workspace = true }
serde = { workspace = true }
[dev-dependencies]
tempfile = "3.10"
```

### 2. Domain `TelemetryEvent` shape (§4.4 — the contract this task implements)

```text
enum ExtraField { Null, Bool(bool), Number(f64), Text(String), Object(Vec<(String,ExtraField)>), Array(Box<[ExtraField]>) }
struct TelemetryEvent {
    kind: String,            // "llm_delta" | "tool_call" | "tool_result" | "finish" | "error" | "loop_start" | "loop_end"
    model: String,           // provider + model, e.g. "openai/gpt-4o-mini"
    input_tokens: u64,
    output_tokens: u64,
    cache_tokens: u64,
    steps: u64,              // cumulative or per-event depending on kind
    execution_time_ms: u64,
    session_id: String,
    extra: Box<[(String, ExtraField)]>,
}
struct TelemetryTotals { model, input_tokens, output_tokens, cache_tokens, steps, execution_time_ms, session_id }
```
`ExtraField` is the **only** serialization bridge domain needs; this crate turns it into JSON. Domain gains no serde dep (FR-DI-01).

### 3. `src/lib.rs` — `JsonTelemetry`

```rust
pub struct JsonTelemetry {
    out: Box<dyn Write + Send>,          // stdout in headless; sink in TUI
    report_dir: PathBuf,                 // .zcode/reports
    totals: TelemetryTotals,
    start: Instant,
}
impl JsonTelemetry {
    pub fn new(out: Box<dyn Write + Send>, report_dir: PathBuf) -> Self;
    fn extra_to_json(extra: &[(String, ExtraField)]) -> serde_json::Value;  // Object/Array/etc
}
impl TelemetryPort for JsonTelemetry {
    fn emit(&mut self, ev: TelemetryEvent) {
        // accumulate into totals (sum tokens/steps; execution_time = now-start)
        // write one JSON object + '\n' to out  (FR-OUTPUT-01 / NFR-OBS-01)
    }
    fn flush_report(&mut self, session_id: &str, totals: TelemetryTotals) -> Result<PathBuf, Box<dyn Error>> {
        // write .zcode/reports/<timestamp>-<session>.json atomically (temp+rename)
    }
}
```

### 4. JSONL event shape (each line)

```jsonc
{ "kind":"llm_delta", "model":"openai/gpt-4o-mini", "input_tokens":0, "output_tokens":3,
  "cache_tokens":0, "steps":1, "execution_time_ms":42, "session_id":"<uuidv7>",
  "delta":"Hel" }
```
```jsonc
{ "kind":"tool_call", "model":"...", "steps":2, "execution_time_ms":88, "session_id":"...",
  "tool":"str_replace_editor", "args":{...} }
{ "kind":"tool_result", "model":"...", "steps":2, "execution_time_ms":92, "session_id":"...",
  "content":"...(truncated to max_tool_output_chars if needed)..." }
{ "kind":"finish", "model":"...", "input_tokens":128, "output_tokens":64, "cache_tokens":0,
  "steps":3, "execution_time_ms":1200, "session_id":"...", "reason":"stop" }
```
Every line is a single valid JSON object → `jq -e .` parseable (M1.6, NFR-OBS-01). `model` is always `provider/model` (FR-OUTPUT-08).

### 5. Report file schema (`.zcode/reports/<timestamp>-<session>.json`)

```jsonc
{ "version":1, "session_id":"<uuidv7>", "model":"openai/gpt-4o-mini",
  "input_tokens":N, "output_tokens":N, "cache_tokens":N,
  "steps":N, "execution_time_ms":N, "finish_reason":"stop|tool_use|length|error",
  "truncated":false }
```
M1.7 requires exactly these keys. `flush_report` is called by the engine on loop exit (success or `Ctrl-C`).

### 6. Token accounting (DQ2)

- Provider-reported usage from `LlmEvent::Finish` is authoritative (input/output/cache). The engine feeds these to `emit` on each `finish` event; `JsonTelemetry` accumulates the max/running sums.
- When a provider omits usage (Ollama, some stubs), the engine passes `input_tokens = domain::tokens::estimate_tokens(concatenated_messages)`. `JsonTelemetry` records it and sets an `ExtraField::Bool("token_estimate", true)` so consumers know it's an estimate.

### 7. Tests

- `emit_writes_one_jsonl_line`: emit 3 events, drain `out`, assert 3 valid JSON lines via `serde_json::from_str` (NFR-OBS-01).
- `flush_report_has_required_schema`: call `flush_report` → read file → assert keys `{version, session_id, model, input_tokens, output_tokens, cache_tokens, steps, execution_time_ms, finish_reason, truncated}` present (M1.7).
- `report_written_atomically`: a `.tmp` left from a mock crash is ignored on re-read; completed report parses.
- `extra_fields_serialize`: `ExtraField::{Object, Array, Number, Bool, Text, Null}` → correct JSON values (domain→JSON bridge correct).
- `accumulates_totals`: emit delta (output 3) + finish (input 128, output 64, cache 0) → report `input_tokens=128, output_tokens=64`.

## Test-case scenario

- `zcode run --json "echo hi"` → stdout shows `{"kind":"loop_start",...}\n{"kind":"llm_delta",...}\n{"kind":"finish",...}\n`, parseable by `jq -e .`. A `.zcode/reports/<ts>-<session>.json` appears with all required keys and the model token counts.

## How to verify

```
cargo test -p infra-telemetry
cargo clippy -p infra-telemetry -- -D warnings
cargo tree -p infra-telemetry     # deps: domain, serde_json, serde (+ tempfile dev)
jq -e . < <(cargo run -q -- run --json "..." 2>/dev/null)   # manual: one-valid-json-per-line
```

**Pass criteria:** every emitted line is valid JSON (NFR-OBS-01); report file has the documented schema (M1.7); domain stays dep-free (no serde in `domain`); total token accumulation matches provider-reported usage; zero `unsafe`; `cargo tree -p infra-telemetry` = `{domain, serde_json, serde}`.

## Success metric mapping

- M1.7 (report schema), M1.6 (JSONL parseable), NFR-OBS-01/02, FR-OUTPUT-01..08, FR-OUTPUT-09 (skill access is a tool, not telemetry, but `extra` can carry the skill name), DQ2 (provider-reported usage + estimate fallback).

## Notes / risks

- `out` is `Box<dyn Write + Send>` so the TUI can swap stdout for an in-memory sink and replay events in the message pane (FR-IFACE-04) while still writing the report file.
- `serde_json` is used **inside** this crate only; domain carries the `ExtraField` enum precisely so domain `cargo tree` stays clean (FR-DI-01 enforcement via `make check-deps`).
