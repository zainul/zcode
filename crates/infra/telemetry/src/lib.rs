//! Telemetry emitter: streams one JSON object per event to `out` (JSONL, for
//! `zcode run --json`) and accumulates totals into `.zcode/reports/<ts>-<session>.json`
//! on `flush_report` (FR-OUTPUT-01/02, NFR-OBS-01/02, M1.6/M1.7).
//!
//! `domain` stays serde-free (FR-DI-01) by carrying `ExtraField`; this crate is
//! the serde bridge that turns `ExtraField` into `serde_json::Value`.
//!
//! Direct deps: domain, serde, serde_json.
#![forbid(unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub mod opencode;
pub use opencode::OpencodeTelemetry;

use domain::{BoxError, ExtraField, TelemetryEvent, TelemetryPort, TelemetryTotals};
use serde::{Deserialize, Serialize};

/// JSONL event emitter + report writer. `out` is stdout in headless mode and an
/// in-memory sink in the TUI (FR-IFACE-04); the report file is always written.
pub struct JsonTelemetry {
    out: Box<dyn Write + Send>,
    report_dir: PathBuf,
    totals: TelemetryTotals,
    start: Instant,
}

impl JsonTelemetry {
    pub fn new(out: Box<dyn Write + Send>, report_dir: PathBuf) -> Self {
        if !report_dir.as_os_str().is_empty() {
            fs::create_dir_all(&report_dir).ok();
        }
        Self {
            out,
            report_dir,
            totals: TelemetryTotals {
                model: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
                steps: 0,
                execution_time_ms: 0,
                session_id: String::new(),
                finish_reason: String::new(),
                truncated: false,
                cost_usd: None,
            },
            start: Instant::now(),
        }
    }

    /// Bridge `ExtraField` → `serde_json::Value` (domain→JSON, FR-DI-01).
    fn extra_to_json(extra: &[(String, ExtraField)]) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        for (k, v) in extra {
            map.insert((*k).clone(), extra_field_to_value(v));
        }
        map
    }

    pub fn report_dir(&self) -> &Path {
        &self.report_dir
    }
}

/// Recursively convert an `ExtraField` into a `serde_json::Value`.
fn extra_field_to_value(v: &ExtraField) -> serde_json::Value {
    match v {
        ExtraField::Null => serde_json::Value::Null,
        ExtraField::Bool(b) => serde_json::Value::Bool(*b),
        ExtraField::Number(n) => serde_json::Value::from(*n),
        ExtraField::Text(s) => serde_json::Value::String(s.clone()),
        ExtraField::Object(pairs) => {
            let mut m = serde_json::Map::new();
            for (k, v) in pairs.iter() {
                m.insert((*k).clone(), extra_field_to_value(v));
            }
            serde_json::Value::Object(m)
        }
        ExtraField::Array(items) => {
            serde_json::Value::Array(items.iter().map(extra_field_to_value).collect())
        }
    }
}

impl TelemetryPort for JsonTelemetry {
    fn emit(&mut self, ev: TelemetryEvent) {
        // execution_time: prefer the engine-provided value, fall back to the
        // elapsed wall-clock since construction when the event omits it.
        let elapsed_ms = if ev.execution_time_ms == 0 {
            self.start.elapsed().as_millis() as u64
        } else {
            ev.execution_time_ms
        };
        // Accumulate authoritative totals (max over provider-reported +
        // engine-supplied counts; see DQ2 — provider-reported usage wins).
        self.totals.input_tokens = self.totals.input_tokens.max(ev.input_tokens);
        self.totals.output_tokens = self.totals.output_tokens.max(ev.output_tokens);
        self.totals.cache_tokens = self.totals.cache_tokens.max(ev.cache_tokens);
        self.totals.steps = self.totals.steps.max(ev.steps);
        self.totals.execution_time_ms = self.totals.execution_time_ms.max(elapsed_ms);
        if !ev.model.is_empty() {
            self.totals.model = ev.model.clone();
        }
        // Carry `finish_reason`/`truncated` from the finish event's extra so the
        // report is complete even if the engine passes a thin totals struct.
        for (k, v) in ev.extra.iter() {
            match (k.as_str(), v) {
                ("reason", ExtraField::Text(s)) => self.totals.finish_reason = s.clone(),
                ("truncated", ExtraField::Bool(b)) => self.totals.truncated = *b,
                _ => {}
            }
        }

        // Build one JSON object: base fields + flattened extra (FR-OUTPUT-01).
        let mut obj = serde_json::json!({
            "kind": ev.kind,
            "model": ev.model,
            "input_tokens": ev.input_tokens,
            "output_tokens": ev.output_tokens,
            "cache_tokens": ev.cache_tokens,
            "steps": ev.steps,
            "execution_time_ms": elapsed_ms,
            "session_id": ev.session_id,
        });
        let base = obj.as_object_mut().unwrap();
        base.extend(Self::extra_to_json(&ev.extra));
        let line = serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string());
        let _ = writeln!(self.out, "{line}");
    }

    fn flush_report(
        &mut self,
        session_id: &str,
        total: TelemetryTotals,
    ) -> Result<PathBuf, BoxError> {
        // Merge accumulated emit-totals with the engine-provided finish info:
        // numeric fields use max so provider-reported usage wins (DQ2),
        // finish_reason/truncated prefer the explicit engine-passed value.
        let merged = TelemetryTotals {
            model: if total.model.is_empty() {
                self.totals.model.clone()
            } else {
                total.model
            },
            input_tokens: self.totals.input_tokens.max(total.input_tokens),
            output_tokens: self.totals.output_tokens.max(total.output_tokens),
            cache_tokens: self.totals.cache_tokens.max(total.cache_tokens),
            steps: self.totals.steps.max(total.steps),
            execution_time_ms: self.totals.execution_time_ms.max(total.execution_time_ms),
            session_id: session_id.to_string(),
            finish_reason: if total.finish_reason.is_empty() {
                self.totals.finish_reason.clone()
            } else {
                total.finish_reason
            },
            truncated: total.truncated || self.totals.truncated,
            // A priced run wins over an unpriced one, so a thin totals struct
            // cannot erase a cost the emit stream already established.
            cost_usd: total.cost_usd.or(self.totals.cost_usd),
        };
        self.totals = merged.clone();
        let report = ReportFile {
            version: 1,
            session_id: merged.session_id,
            model: merged.model,
            input_tokens: merged.input_tokens,
            output_tokens: merged.output_tokens,
            cache_tokens: merged.cache_tokens,
            steps: merged.steps,
            execution_time_ms: merged.execution_time_ms,
            finish_reason: merged.finish_reason,
            truncated: merged.truncated,
            cost_usd: merged.cost_usd,
        };
        let json = serde_json::to_string_pretty(&report)?;
        let path = self
            .report_dir
            .join(format!("{}-{}.json", now_iso(), session_id));
        atomic_write(&path, &json)?;
        Ok(path)
    }
}

impl JsonTelemetry {
    /// Direct accessor for tests / TUI replay without going through the trait.
    pub fn totals(&self) -> &TelemetryTotals {
        &self.totals
    }
}

/// The on-disk report schema (M1.7). Exactly the documented keys.
#[derive(Debug, Serialize, Deserialize)]
struct ReportFile {
    version: u32,
    session_id: String,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_tokens: u64,
    steps: u64,
    execution_time_ms: u64,
    finish_reason: String,
    truncated: bool,
    /// Estimated USD spend. Absent — not zero — when the model is unpriced.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    cost_usd: Option<f64>,
}

/// Atomic write: `<path>.tmp` then `fs::rename` (same-filesystem rename is
/// atomic; a crash mid-write leaves a `.tmp` that is ignored on re-read).
fn atomic_write(path: &Path, json: &str) -> Result<(), std::io::Error> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// UTC ISO-8601 `Z` timestamp using pure-calendar math (no `chrono`).
fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso8601_from_secs(secs)
}

fn iso8601_from_secs(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = days_to_ymd(days);
    let hours = rem / 3600;
    let mins = (rem % 3600) / 60;
    let secs_part = rem % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, mins, secs_part
    )
}

fn days_to_ymd(days: u64) -> (u64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + if m <= 2 { 1 } else { 0 }) as u64;
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// A `Write` that buffers into a shared `Vec<u8>` so tests can read the JSONL
    /// output back without downcasting `Box<dyn Write>`.
    #[derive(Clone, Default)]
    struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CapturingWriter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    fn te(model: &str) -> TelemetryTotals {
        TelemetryTotals {
            model: model.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_tokens: 0,
            steps: 0,
            execution_time_ms: 0,
            session_id: String::new(),
            finish_reason: String::new(),
            truncated: false,
            cost_usd: None,
        }
    }

    #[test]
    fn emit_writes_one_jsonl_line() {
        let cap = CapturingWriter::default();
        let out: Box<dyn Write + Send> = Box::new(cap.clone());
        let mut tel = JsonTelemetry::new(out, PathBuf::from(""));
        tel.emit(TelemetryEvent {
            kind: "llm_delta".into(),
            model: "openai/gpt-4o-mini".into(),
            input_tokens: 0,
            output_tokens: 3,
            cache_tokens: 0,
            steps: 1,
            execution_time_ms: 42,
            session_id: "s".into(),
            extra: Box::new([("delta".into(), ExtraField::Text("Hel".into()))]),
        });
        tel.emit(TelemetryEvent {
            kind: "llm_delta".into(),
            model: "openai/gpt-4o-mini".into(),
            input_tokens: 0,
            output_tokens: 2,
            cache_tokens: 0,
            steps: 1,
            execution_time_ms: 50,
            session_id: "s".into(),
            extra: Box::new([("delta".into(), ExtraField::Text("lo".into()))]),
        });
        tel.emit(TelemetryEvent {
            kind: "finish".into(),
            model: "openai/gpt-4o-mini".into(),
            input_tokens: 128,
            output_tokens: 64,
            cache_tokens: 0,
            steps: 3,
            execution_time_ms: 1200,
            session_id: "s".into(),
            extra: Box::new([("reason".into(), ExtraField::Text("stop".into()))]),
        });

        let written = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 3);
        // NFR-OBS-01: every line is valid JSON.
        for (i, line) in lines.iter().enumerate() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("line {i} not json: {e}"));
        }
        // Base fields present on the finish line.
        let finish = serde_json::from_str::<serde_json::Value>(lines[2]).unwrap();
        assert_eq!(finish["kind"], "finish");
        assert_eq!(finish["input_tokens"], 128);
        assert_eq!(finish["output_tokens"], 64);
        assert_eq!(finish["reason"], "stop");
        // Flattened extra merged.
        assert_eq!(finish["delta"], serde_json::Value::Null);
    }

    #[test]
    fn flush_report_has_required_schema() {
        let dir = tempfile::tempdir().unwrap().keep();
        let out: Box<dyn Write + Send> = Box::new(CapturingWriter::default());
        let mut tel = JsonTelemetry::new(out, dir.clone());
        let report_path = tel
            .flush_report("sess-1", te("openai/gpt-4o-mini"))
            .unwrap();
        assert!(report_path.exists());
        let content = fs::read_to_string(&report_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let obj = v.as_object().unwrap();
        for key in [
            "version",
            "session_id",
            "model",
            "input_tokens",
            "output_tokens",
            "cache_tokens",
            "steps",
            "execution_time_ms",
            "finish_reason",
            "truncated",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert_eq!(v["version"], 1);
        assert_eq!(v["session_id"], "sess-1");
    }

    #[test]
    fn report_written_atomically() {
        let dir = tempfile::tempdir().unwrap().keep();
        let out: Box<dyn Write + Send> = Box::new(CapturingWriter::default());
        let mut tel = JsonTelemetry::new(out, dir.clone());
        let report_path = tel.flush_report("s", te("m")).unwrap();
        // Simulate a crashed half-write: leave a garbage .tmp next to the report.
        let tmp = report_path.with_extension("json.tmp");
        fs::write(&tmp, "{ corrupt").unwrap();
        // Re-flush: the real report should still parse cleanly.
        tel.flush_report("s", te("m")).unwrap();
        let content = fs::read_to_string(&report_path).unwrap();
        serde_json::from_str::<serde_json::Value>(&content).unwrap();
    }

    #[test]
    fn extra_fields_serialize() {
        let dir = PathBuf::from("");
        let cap = CapturingWriter::default();
        let out: Box<dyn Write + Send> = Box::new(cap.clone());
        let mut tel = JsonTelemetry::new(out, dir);
        // Object / Array / Number / Bool / Text / Null bridge.
        let extra: Box<[(String, ExtraField)]> = Box::new([
            ("n".into(), ExtraField::Number(1.5)),
            ("b".into(), ExtraField::Bool(true)),
            ("t".into(), ExtraField::Text("x".into())),
            (
                "obj".into(),
                ExtraField::Object(Box::new([("k".into(), ExtraField::Text("v".into()))])),
            ),
            (
                "arr".into(),
                ExtraField::Array(Box::new([ExtraField::Number(1.0), ExtraField::Null])),
            ),
            ("null".into(), ExtraField::Null),
        ]);
        tel.emit(TelemetryEvent {
            kind: "t".into(),
            model: "m".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_tokens: 0,
            steps: 0,
            execution_time_ms: 0,
            session_id: "s".into(),
            extra,
        });
        // The extra map is merged into the JSONL line.
        let written = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        let line = written.lines().next().unwrap();
        let v = serde_json::from_str::<serde_json::Value>(line).unwrap();
        assert_eq!(v["n"], 1.5);
        assert_eq!(v["b"], true);
        assert_eq!(v["t"], "x");
        assert_eq!(v["obj"]["k"], "v");
        assert_eq!(v["arr"][0], 1.0);
        assert_eq!(v["arr"][1], serde_json::Value::Null);
        assert_eq!(v["null"], serde_json::Value::Null);
    }

    #[test]
    fn accumulates_totals() {
        let dir = tempfile::tempdir().unwrap().keep();
        let out: Box<dyn Write + Send> = Box::new(CapturingWriter::default());
        let mut tel = JsonTelemetry::new(out, dir.clone());
        // delta carries output=3 (partial); finish carries the authoritative 128/64.
        tel.emit(TelemetryEvent {
            kind: "llm_delta".into(),
            model: "openai/gpt-4o-mini".into(),
            input_tokens: 0,
            output_tokens: 3,
            cache_tokens: 0,
            steps: 1,
            execution_time_ms: 42,
            session_id: "s".into(),
            extra: Box::new([("delta".into(), ExtraField::Text("Hel".into()))]),
        });
        tel.emit(TelemetryEvent {
            kind: "finish".into(),
            model: "openai/gpt-4o-mini".into(),
            input_tokens: 128,
            output_tokens: 64,
            cache_tokens: 0,
            steps: 3,
            execution_time_ms: 1200,
            session_id: "s".into(),
            extra: Box::new([("reason".into(), ExtraField::Text("stop".into()))]),
        });
        // Engine passes finish info; report uses accumulated token max.
        let mut t = te("openai/gpt-4o-mini");
        t.finish_reason = "stop".into();
        let path = tel.flush_report("s", t).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["input_tokens"], 128);
        assert_eq!(v["output_tokens"], 64);
        assert_eq!(v["cache_tokens"], 0);
        assert_eq!(v["finish_reason"], "stop");
    }

    #[test]
    fn report_dir_created_if_missing() {
        let dir = tempfile::tempdir().unwrap().keep();
        let nested = dir.join(".zcode").join("reports");
        let out: Box<dyn Write + Send> = Box::new(CapturingWriter::default());
        let _ = JsonTelemetry::new(out, nested.clone());
        assert!(nested.exists());
    }

    #[test]
    fn iso8601_is_utc_z() {
        let s = now_iso();
        assert!(s.ends_with('Z'));
        assert_eq!(s.len(), 20);
    }
}
