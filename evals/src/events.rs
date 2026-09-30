//! Turn one run's `zcode run --json` stream (and its report file) into the
//! numbers PRD §2.3 is judged on.

use serde_json::Value;

/// Shell commands whose output is a search — the leading indicator "search
/// through `grep` rather than `shell`" (PRD §2.3).
const SEARCH_COMMANDS: &[&str] = &["grep", "rg", "ag", "ack", "find", "fd", "egrep", "fgrep"];

/// Categories that make up "discovery tokens" (PRD M3).
const DISCOVERY: &[&str] = &["discover", "locate", "inspect"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    pub steps: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: Option<f64>,
    pub discovery_tokens: u64,
    pub tool_result_tokens: u64,
    pub shell_search_calls: u64,
    pub compactions: u64,
    pub context_errors: u64,
    pub peak_context_tokens: u64,
    pub peak_pct: Option<f64>,
    pub final_text: String,
    pub finish_reason: String,
}

/// First word of a shell command, after `cd … &&` and `env VAR=…` prefixes.
fn shell_program(command: &str) -> &str {
    let mut rest = command.trim();
    loop {
        if let Some(idx) = rest.find("&&") {
            let head = rest[..idx].trim_start();
            if head.starts_with("cd ") {
                rest = rest[idx + 2..].trim_start();
                continue;
            }
        }
        break;
    }
    let mut words = rest.split_whitespace().peekable();
    while let Some(w) = words.peek() {
        if *w == "env" || w.contains('=') {
            words.next();
        } else {
            break;
        }
    }
    words.next().unwrap_or("")
}

pub fn is_shell_search(command: &str) -> bool {
    SEARCH_COMMANDS.contains(&shell_program(command))
}

fn u(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Parse the JSONL stream. Unknown kinds and malformed lines are skipped: a
/// harness must survive the tool it measures adding events.
pub fn parse_stream(jsonl: &str) -> Observed {
    let mut o = Observed::default();
    let mut shell_search_ids: Vec<String> = Vec::new();
    let mut last_step_text = String::new();
    for line in jsonl.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match ev.get("kind").and_then(Value::as_str).unwrap_or("") {
            "loop_start" => last_step_text.clear(),
            "llm_delta" => {
                if let Some(t) = ev.get("text").and_then(Value::as_str) {
                    last_step_text.push_str(t);
                }
            }
            "tool_call" => {
                let tool = ev.get("tool").and_then(Value::as_str).unwrap_or("");
                if tool == "shell" {
                    let args = ev.get("arguments").and_then(Value::as_str).unwrap_or("");
                    let command = serde_json::from_str::<Value>(args)
                        .ok()
                        .and_then(|a| a.get("command").and_then(Value::as_str).map(String::from))
                        .unwrap_or_default();
                    let id = ev
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if is_shell_search(&command) {
                        o.shell_search_calls += 1;
                        shell_search_ids.push(id);
                    }
                }
            }
            "tool_result" => {
                let tokens = ev.get("tokens_est").and_then(Value::as_f64).unwrap_or(0.0) as u64;
                let category = ev.get("category").and_then(Value::as_str).unwrap_or("");
                let id = ev.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
                o.tool_result_tokens += tokens;
                if DISCOVERY.contains(&category) || shell_search_ids.iter().any(|s| s == id) {
                    o.discovery_tokens += tokens;
                }
            }
            "context_compacted" => o.compactions += 1,
            "finish" => {
                o.steps = u(&ev, "steps");
                o.input_tokens = u(&ev, "input_tokens");
                o.output_tokens = u(&ev, "output_tokens");
                // Split fields (v0.7+) win; a v0.6 stream has one `cache_tokens`,
                // counted as reads — the conservative reading for cost.
                if ev.get("cache_read_tokens").is_some() {
                    o.cache_read_tokens = u(&ev, "cache_read_tokens");
                    o.cache_write_tokens = u(&ev, "cache_write_tokens");
                } else {
                    o.cache_read_tokens = u(&ev, "cache_tokens");
                }
                o.cost_usd = ev.get("cost_usd").and_then(Value::as_f64);
                o.peak_context_tokens = u(&ev, "peak_context_tokens");
                if let Some(w) = ev.get("context_window").and_then(Value::as_f64) {
                    if w > 0.0 {
                        o.peak_pct = Some(o.peak_context_tokens as f64 / w);
                    }
                }
                o.finish_reason = ev
                    .get("stop_cause")
                    .and_then(Value::as_str)
                    .or_else(|| ev.get("reason").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                o.final_text = last_step_text.clone();
            }
            _ => {}
        }
        if line.contains("context length") || line.contains("context_length_exceeded") {
            o.context_errors += 1;
        }
    }
    o
}

/// Effective input cost in base-input-token equivalents (PRD Appendix A):
/// uncached input + 1.25 × cache writes + 0.1 × cache reads.
pub fn effective_input(o: &Observed, cache_within_input: bool) -> f64 {
    let uncached = if cache_within_input {
        o.input_tokens.saturating_sub(o.cache_read_tokens)
    } else {
        o.input_tokens
    };
    uncached as f64 + 1.25 * o.cache_write_tokens as f64 + 0.1 * o.cache_read_tokens as f64
}

/// Raw prompt tokens: what was sent before any caching discount (PRD M2).
pub fn raw_prompt(o: &Observed, cache_within_input: bool) -> u64 {
    if cache_within_input {
        o.input_tokens
    } else {
        o.input_tokens + o.cache_read_tokens + o.cache_write_tokens
    }
}

/// Share of the prompt served from cache (PRD M4/M5).
pub fn hit_ratio(o: &Observed, cache_within_input: bool) -> Option<f64> {
    let raw = raw_prompt(o, cache_within_input);
    (raw > 0).then(|| o.cache_read_tokens as f64 / raw as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_program_skips_cd_and_env_prefixes() {
        assert_eq!(shell_program("cd src && rg foo"), "rg");
        assert_eq!(shell_program("env LC_ALL=C grep -r x ."), "grep");
        assert_eq!(shell_program("FOO=1 find . -name '*.rs'"), "find");
        assert_eq!(shell_program("cargo test"), "cargo");
        assert!(is_shell_search("grep -rn TODO ."));
        assert!(!is_shell_search("cargo build"));
    }

    #[test]
    fn stream_is_folded_into_observed_metrics() {
        let jsonl = [
            r#"{"kind":"loop_start","steps":1}"#,
            r#"{"kind":"tool_call","tool":"shell","tool_call_id":"c1","arguments":"{\"command\":\"rg foo\"}"}"#,
            r#"{"kind":"tool_result","tool":"shell","tool_call_id":"c1","tokens_est":300,"category":"shell"}"#,
            r#"{"kind":"tool_call","tool":"read","tool_call_id":"c2","arguments":"{}"}"#,
            r#"{"kind":"tool_result","tool":"read","tool_call_id":"c2","tokens_est":900,"category":"inspect"}"#,
            r#"{"kind":"tool_result","tool":"write","tool_call_id":"c3","tokens_est":20,"category":"change"}"#,
            r#"{"kind":"loop_start","steps":2}"#,
            r#"{"kind":"llm_delta","text":"The answer "}"#,
            r#"{"kind":"llm_delta","text":"is 42."}"#,
            r#"{"kind":"finish","steps":2,"input_tokens":1000,"output_tokens":50,"cache_read_tokens":800,"cache_write_tokens":100,"peak_context_tokens":5000,"context_window":200000,"reason":"stop","stop_cause":null}"#,
            "not json",
        ]
        .join("\n");
        let o = parse_stream(&jsonl);
        assert_eq!(o.steps, 2);
        assert_eq!(o.shell_search_calls, 1);
        assert_eq!(o.discovery_tokens, 1200, "read + the shell search");
        assert_eq!(o.tool_result_tokens, 1220);
        assert_eq!(o.cache_write_tokens, 100);
        assert_eq!(o.final_text, "The answer is 42.");
        assert_eq!(o.peak_pct, Some(0.025));
        assert_eq!(o.finish_reason, "stop");
    }

    #[test]
    fn a_v06_stream_counts_its_single_cache_figure_as_reads() {
        let o = parse_stream(r#"{"kind":"finish","steps":1,"input_tokens":10,"cache_tokens":7}"#);
        assert_eq!(o.cache_read_tokens, 7);
        assert_eq!(o.cache_write_tokens, 0);
    }

    #[test]
    fn effective_cost_formula_matches_the_prd() {
        let o = Observed {
            input_tokens: 1_000,
            cache_read_tokens: 10_000,
            cache_write_tokens: 400,
            ..Observed::default()
        };
        // Anthropic: input is already the uncached remainder.
        assert_eq!(effective_input(&o, false), 1_000.0 + 500.0 + 1_000.0);
        // OpenAI: cached tokens sit inside input_tokens.
        let o2 = Observed {
            input_tokens: 11_000,
            cache_read_tokens: 10_000,
            ..Observed::default()
        };
        assert_eq!(effective_input(&o2, true), 1_000.0 + 1_000.0);
        assert_eq!(hit_ratio(&o2, true), Some(10_000.0 / 11_000.0));
    }
}
