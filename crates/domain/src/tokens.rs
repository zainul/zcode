//! Token estimation (FR-BUDGET-02, CE-DQ10, CE-DQ11).
//!
//! Provider-reported usage is authoritative (DQ2). Everything here exists for
//! the moments the engine has to decide *before* a provider has said anything:
//! how big the next request will be, whether it is time to compact, how much
//! of a tool budget a result uses. A tokenizer crate is not an option in
//! `domain` (FR-DI-01), so the estimate is a character heuristic that is then
//! *calibrated* against what the provider actually reports.

use crate::ports::{LlmFinish, LlmMessage};

/// Characters per token for text dense in punctuation and symbols (code,
/// JSON, diffs). BPE tokenizers split operators and brackets finely.
const CODE_DIVISOR: f64 = 3.6;
/// Characters per token for ordinary prose.
const PROSE_DIVISOR: f64 = 4.2;
/// Above this share of ASCII punctuation, text is treated as code-like.
const CODE_SYMBOL_SHARE: f64 = 0.12;

/// Estimated token count of `text`.
///
/// Replaces the v0.2 `words × 4` rule, which undercounted code badly: a line
/// like `foo(bar.baz[0]);` is one "word" but half a dozen tokens.
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let (mut chars, mut symbols) = (0u64, 0u64);
    for c in text.chars() {
        chars += 1;
        if c.is_ascii_punctuation() {
            symbols += 1;
        }
    }
    let divisor = if symbols as f64 / chars as f64 > CODE_SYMBOL_SHARE {
        CODE_DIVISOR
    } else {
        PROSE_DIVISOR
    };
    (chars as f64 / divisor).ceil() as u64
}

/// Estimated tokens of a run of transcript messages: text content, tool-call
/// names and arguments, and tool-result bodies all go on the wire.
///
/// Counting `content` alone — as v0.2..v0.6 did — misses every tool result,
/// since those live in `tool_result.content`: the one kind of message most
/// likely to be large was invisible to the window clamp.
pub fn estimate_messages(messages: &[LlmMessage]) -> u64 {
    messages
        .iter()
        .map(|m| {
            estimate_tokens(&m.content)
                + m.tool_calls
                    .iter()
                    .map(|c| estimate_tokens(&c.name) + estimate_tokens(&c.arguments))
                    .sum::<u64>()
                + m.tool_result
                    .as_ref()
                    .map_or(0, |r| estimate_tokens(&r.content))
        })
        .sum()
}

/// Learns how far [`estimate_tokens`] is off for the model in use, from the
/// provider's own reported prompt sizes, and corrects later estimates by the
/// same ratio.
///
/// An exponential moving average rather than the last ratio alone: one step
/// dominated by an image or a cached prefix should not swing every estimate
/// after it. Clamped so a single nonsensical report cannot poison the run.
#[derive(Clone, Copy, Debug)]
pub struct TokenCalibrator {
    ratio: f64,
}

impl Default for TokenCalibrator {
    fn default() -> Self {
        Self { ratio: 1.0 }
    }
}

impl TokenCalibrator {
    const ALPHA: f64 = 0.3;
    const MIN: f64 = 0.5;
    const MAX: f64 = 2.0;

    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a prompt estimated at `estimated` tokens was reported as
    /// `reported`. Either being zero carries no information and is ignored.
    pub fn observe(&mut self, reported: u64, estimated: u64) {
        if reported == 0 || estimated == 0 {
            return;
        }
        let sample = (reported as f64 / estimated as f64).clamp(Self::MIN, Self::MAX);
        self.ratio =
            (Self::ALPHA * sample + (1.0 - Self::ALPHA) * self.ratio).clamp(Self::MIN, Self::MAX);
    }

    /// `estimated` corrected by what has been learned so far.
    pub fn apply(&self, estimated: u64) -> u64 {
        (estimated as f64 * self.ratio).round() as u64
    }

    pub fn ratio(&self) -> f64 {
        self.ratio
    }
}

/// The true size of a finished call's prompt (CE-DQ10, FR-CACHE-06).
///
/// Providers disagree about what `input_tokens` means. OpenAI-family APIs
/// count cached tokens *inside* it; Anthropic reports only the uncached
/// remainder there and the cached part separately — so on a well-cached
/// Anthropic session `input_tokens` alone can read 4K on a 150K prompt.
/// Anything that reacts to context size must use this instead.
pub fn prompt_size(finish: &LlmFinish, cache_within_input: bool) -> u64 {
    if cache_within_input {
        finish.input_tokens
    } else {
        finish.input_tokens + finish.cache_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::LlmFinishReason;

    /// Real token counts, measured with the `cl100k_base` BPE vocabulary
    /// (Hugging Face `tokenizers`, `Xenova/gpt-4` tokenizer.json) and
    /// committed as constants, so the estimator's error is measured against
    /// a real tokenizer rather than against itself. Measured MAPE: ~15%.
    const SAMPLES: &[(&str, u64)] = &[
        ("fn main() { println!(\"hello, world\"); }", 11),
        ("let x: Vec<u32> = (0..10).map(|i| i * 2).collect();", 24),
        (
            "impl Foo for Bar {\n    fn baz(&self) -> Result<(), Error> {\n        Ok(())\n    }\n}",
            23,
        ),
        ("{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}", 13),
        ("const x = await fetch(`/api/users/${id}`);", 12),
        ("export function add(a: number, b: number): number { return a + b; }", 19),
        ("def greet(name):\n    return f\"Hello, {name}!\"", 14),
        ("SELECT id, name FROM users WHERE active = 1 ORDER BY name;", 15),
        ("The quick brown fox jumps over the lazy dog.", 10),
        (
            "Make the retry logic back off for thirty seconds when the provider rate limits us.",
            16,
        ),
        (
            "This function reads the configuration file and returns the merged result.",
            12,
        ),
        ("error[E0308]: mismatched types\n --> src/main.rs:4:18", 17),
        ("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@", 22),
        ("    if let Some(v) = map.get(&key) { total += *v; }", 19),
        ("#[derive(Clone, Debug, PartialEq)]\npub struct Point { x: f64, y: f64 }", 22),
        (
            "Please summarise the changes made so far and list the next steps clearly.",
            15,
        ),
        ("package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(\"hi\") }", 17),
        ("<div className=\"flex items-center\">{children}</div>", 11),
        ("pip install -r requirements.txt && pytest -q tests/", 12),
        (
            "Tokens are the units a language model reads; words are usually one or two.",
            16,
        ),
    ];

    #[test]
    fn message_estimate_counts_tool_results_and_arguments() {
        use crate::ports::{LlmToolCall, LlmToolResult};
        let body = "x".repeat(4_200);
        let result = LlmMessage::tool_result_message(LlmToolResult {
            tool_call_id: "c".into(),
            content: body.clone(),
        });
        assert!(estimate_messages(std::slice::from_ref(&result)) >= 1_000);
        let mut call = LlmMessage::assistant("");
        call.tool_calls = Box::new([LlmToolCall {
            id: "c".into(),
            name: "write".into(),
            arguments: body,
        }]);
        assert!(estimate_messages(&[call]) >= 1_000);
    }

    #[test]
    fn empty_text_is_zero_tokens() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimator_is_within_25pct_on_the_fixture_set() {
        let mape: f64 = SAMPLES
            .iter()
            .map(|(text, real)| {
                let est = estimate_tokens(text) as f64;
                ((est - *real as f64) / *real as f64).abs()
            })
            .sum::<f64>()
            / SAMPLES.len() as f64;
        assert!(mape <= 0.25, "mean absolute percentage error {mape:.3}");
    }

    #[test]
    fn code_counts_denser_than_prose_of_equal_length() {
        let code: String = "a.b(c)[d];".repeat(30);
        let prose: String = "the cat sat".repeat(30).chars().take(code.len()).collect();
        assert_eq!(code.len(), prose.len());
        assert!(estimate_tokens(&code) > estimate_tokens(&prose));
    }

    #[test]
    fn calibrator_converges_to_within_10pct() {
        let mut cal = TokenCalibrator::new();
        for _ in 0..10 {
            cal.observe(1_300, 1_000);
        }
        let corrected = cal.apply(1_000) as f64;
        assert!((corrected - 1_300.0).abs() / 1_300.0 <= 0.10, "{corrected}");
    }

    #[test]
    fn calibrator_ignores_zero_and_clamps_outliers() {
        let mut cal = TokenCalibrator::new();
        cal.observe(0, 1_000);
        cal.observe(1_000, 0);
        assert_eq!(cal.ratio(), 1.0);
        for _ in 0..50 {
            cal.observe(1_000_000, 1);
        }
        assert!(cal.ratio() <= 2.0);
    }

    fn finish(input: u64, cache: u64) -> LlmFinish {
        LlmFinish {
            reason: LlmFinishReason::Stop,
            input_tokens: input,
            output_tokens: 0,
            cache_tokens: cache,
            cost_usd: None,
        }
    }

    #[test]
    fn prompt_size_adds_cache_for_anthropic_but_not_openai() {
        assert_eq!(prompt_size(&finish(1_200, 51_000), false), 52_200);
        assert_eq!(prompt_size(&finish(52_200, 51_000), true), 52_200);
    }
}
