//! Known context-window sizes per model, used to stop a `max_tokens` request
//! from being sent as a bigger reservation than the window has room for.
//!
//! `max_tokens` is not a target, it is a *reservation*: a provider requires
//! prompt tokens + `max_tokens` to fit inside the model's context window
//! before it will even accept the request, regardless of how many tokens the
//! model goes on to actually generate. A value picked for one model (or
//! copied from an example, or raised "for headroom" on a long task) silently
//! reserves most or all of a smaller window, leaving no room for the prompt —
//! that is what turns an ordinary tool result into a 400, not the size of the
//! codebase being read.
//!
//! Same shape as `domain::pricing`, deliberately: a public, best-effort table
//! (longest matching prefix of the normalised model id wins — see
//! [`crate::model_id::normalize`]), never a source of a *wrong* confident
//! answer. An unknown model returns `None` from [`WindowTable::lookup`] and
//! [`WindowTable::clamp`] passes the caller's `requested` value through
//! unchanged — exactly the behaviour before this table existed. Overriding an
//! entry (`[[context_window]]` in the config file) replaces the built-in
//! figure; entries are prepended, so the override wins ties the same way a
//! `[[pricing]]` override does.
//!
//! Stdlib only, like the rest of `domain` (FR-DI-01).

use crate::model_id::normalize;

/// A model's context window, in tokens (input + output combined — the figure
/// every major provider publishes and bills against as one pool).
#[derive(Clone, Debug, PartialEq)]
pub struct WindowEntry {
    /// Matched as a prefix of the model id, vendor namespace stripped.
    pub model: String,
    pub tokens: u64,
}

impl WindowEntry {
    pub fn new(model: &str, tokens: u64) -> Self {
        Self {
            model: model.to_string(),
            tokens,
        }
    }
}

/// Headroom kept beyond the estimated prompt for what `domain::tokens`'
/// whitespace-split heuristic undercounts: tool schemas sent alongside the
/// messages, provider-side message framing, and the heuristic's own error
/// margin against a real tokenizer. Cheap insurance against clamping to a
/// number that still gets rejected.
const SAFETY_MARGIN_TOKENS: u64 = 4_000;

/// Floor for a clamped request. Below this a turn cannot produce anything
/// useful anyway, so clamping further only trades one failure (the
/// provider's own 400, which names the real cause) for a quieter one (a
/// request that "succeeds" with an answer cut off after a sentence).
const MIN_MAX_TOKENS: u64 = 1_024;

/// An ordered set of context-window rules. Earlier entries win, so configured
/// overrides are simply prepended to the built-ins — see
/// [`domain::pricing::PriceTable`] for the identical rationale.
#[derive(Clone, Debug)]
pub struct WindowTable {
    entries: Vec<WindowEntry>,
}

impl Default for WindowTable {
    fn default() -> Self {
        Self::builtin()
    }
}

impl WindowTable {
    pub fn builtin() -> Self {
        Self {
            entries: builtin_entries(),
        }
    }

    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Put user-supplied windows ahead of the built-in ones.
    pub fn with_overrides(mut overrides: Vec<WindowEntry>) -> Self {
        overrides.extend(builtin_entries());
        Self { entries: overrides }
    }

    pub fn entries(&self) -> &[WindowEntry] {
        &self.entries
    }

    /// Longest matching prefix wins, so `gpt-4o-mini` is not windowed as
    /// `gpt-4o`. Matching is case-insensitive and ignores an OpenRouter-style
    /// `vendor/` namespace and a trailing `:free`-style suffix.
    pub fn lookup(&self, model: &str) -> Option<u64> {
        let key = normalize(model);
        // Longest prefix wins; on a tie the *earlier* entry wins, which is
        // what makes a configured override beat the built-in it shadows.
        self.entries
            .iter()
            .filter(|e| key.starts_with(&normalize(&e.model)))
            .fold(None::<&WindowEntry>, |best, e| match best {
                Some(b) if b.model.len() >= e.model.len() => Some(b),
                _ => Some(e),
            })
            .map(|e| e.tokens)
    }

    /// The `max_tokens` to actually send for `model`, given how many tokens
    /// the running transcript is already estimated to cost.
    ///
    /// Unknown models pass `requested` straight through: no data beats
    /// guessed data, and the table not recognising a model must never make a
    /// working request fail.
    pub fn clamp(&self, model: &str, requested: u64, estimated_prompt_tokens: u64) -> u64 {
        let Some(window) = self.lookup(model) else {
            return requested;
        };
        let budget = window
            .saturating_sub(estimated_prompt_tokens)
            .saturating_sub(SAFETY_MARGIN_TOKENS);
        requested.min(budget.max(MIN_MAX_TOKENS))
    }

    /// Record a window learned directly from a provider's own rejection —
    /// stronger evidence than anything already in the table, built-in or
    /// configured, so it is inserted ahead of everything present. A later
    /// call for the same model naturally supersedes an earlier one: `lookup`
    /// keeps the first of equal-length matches, and `insert(0, ..)` always
    /// puts the newest one first.
    pub fn learn(&mut self, model: &str, tokens: u64) {
        self.entries.insert(0, WindowEntry::new(model, tokens));
    }
}

/// Extracts the real window a provider names when it refuses a request for
/// asking too much — OpenRouter's exact wording is `"...maximum context
/// length is 262144 tokens. However, you requested about 264924 tokens
/// (...)"`. Anchored on the phrase `"context length is "` rather than just
/// any number followed by `"tokens"`, because that same message also
/// contains the *requested* total and the *output* count right next to the
/// real figure — either of which a looser pattern would happily return
/// instead.
pub fn parse_window_from_error(text: &str) -> Option<u64> {
    const ANCHOR: &str = "context length is ";
    let lower = text.to_ascii_lowercase();
    // `to_ascii_lowercase` only touches ASCII bytes and never changes length,
    // so a byte offset found in `lower` is still valid to slice `text` with.
    let start = lower.find(ANCHOR)? + ANCHOR.len();
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

/// Figures published by each provider, current as of this table's writing.
/// Grouped by family with a broad prefix (`"claude"`, `"gemini-2"`) rather
/// than one entry per version where a family shares a window, so a new model
/// release is covered automatically instead of reading as "unknown" until
/// someone adds it — the same trade `domain::pricing` makes for the built-in
/// rate table.
///
/// Best-effort, not authoritative: a provider can change these, and OpenRouter
/// in particular reports a route's *actual* window in the 400 this table
/// exists to prevent — add a `[[context_window]]` entry with that figure to
/// correct or extend this list without waiting on a new zcode release.
fn builtin_entries() -> Vec<WindowEntry> {
    vec![
        // Anthropic: every Claude 3/3.5/3.7/4-class model publishes a
        // 200K-token window (a 1M beta exists for some, but 200K is the safe
        // floor to reserve against).
        WindowEntry::new("claude", 200_000),
        // OpenAI.
        WindowEntry::new("gpt-3.5", 16_385),
        WindowEntry::new("gpt-4o", 128_000),
        WindowEntry::new("gpt-4-turbo", 128_000),
        WindowEntry::new("gpt-4.1", 1_000_000),
        WindowEntry::new("gpt-4", 8_192),
        WindowEntry::new("o1", 200_000),
        WindowEntry::new("o3", 200_000),
        // Google.
        WindowEntry::new("gemini-1.5-pro", 2_000_000),
        WindowEntry::new("gemini-1.5-flash", 1_000_000),
        WindowEntry::new("gemini-2", 1_000_000),
        // DeepSeek.
        WindowEntry::new("deepseek", 64_000),
        // Meta Llama 3.x.
        WindowEntry::new("llama-3", 128_000),
        // Mistral.
        WindowEntry::new("mistral-large", 128_000),
        // Alibaba Qwen 2.5+.
        WindowEntry::new("qwen2.5", 128_000),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins_over_a_shorter_family_match() {
        let table = WindowTable::builtin();
        assert_eq!(table.lookup("gpt-4o-mini"), Some(128_000));
        assert_eq!(table.lookup("gpt-4"), Some(8_192));
        assert_eq!(table.lookup("gpt-4-turbo"), Some(128_000));
    }

    #[test]
    fn matches_regardless_of_vendor_namespace_or_routing_suffix() {
        let table = WindowTable::builtin();
        assert_eq!(
            table.lookup("anthropic/claude-3.5-haiku"),
            table.lookup("claude-3-5-haiku")
        );
        assert_eq!(
            table.lookup("openrouter/anthropic/claude-sonnet-4.5:nitro"),
            Some(200_000)
        );
    }

    #[test]
    fn unknown_model_has_no_window() {
        assert_eq!(WindowTable::builtin().lookup("some-future-model-9"), None);
    }

    #[test]
    fn an_override_beats_the_builtin_it_shadows() {
        // This is exactly the escape hatch for the case that motivates the
        // table: a provider (OpenRouter) reports the *real* window for a
        // route zcode has never heard of, right there in the 400.
        let table =
            WindowTable::with_overrides(vec![WindowEntry::new("z-ai/glm-5.3-flash", 262_144)]);
        assert_eq!(table.lookup("z-ai/glm-5.3-flash"), Some(262_144));
    }

    #[test]
    fn clamp_leaves_an_unknown_model_untouched() {
        let table = WindowTable::empty();
        assert_eq!(table.clamp("mystery-model", 202_144, 60_428), 202_144);
    }

    #[test]
    fn clamp_shrinks_a_reservation_that_would_not_fit() {
        // The exact shape of the bug report this table exists to prevent:
        // max_tokens requested almost equal to the whole window, on top of a
        // real prompt, on a 262144-token model.
        let table = WindowTable::with_overrides(vec![WindowEntry::new("glm-5.3-flash", 262_144)]);
        let clamped = table.clamp("glm-5.3-flash", 202_144, 60_428);
        assert!(clamped < 202_144, "{clamped}");
        // And the request that follows must actually fit: prompt + clamped
        // output + the safety margin, inside the window.
        assert!(60_428 + clamped + SAFETY_MARGIN_TOKENS <= 262_144);
    }

    #[test]
    fn clamp_never_drops_below_the_floor() {
        // A prompt that already fills almost the whole window still gets a
        // usable request rather than one asking for near-zero tokens.
        let table = WindowTable::with_overrides(vec![WindowEntry::new("tiny-model", 8_000)]);
        assert_eq!(table.clamp("tiny-model", 16_000, 7_999), MIN_MAX_TOKENS);
    }

    #[test]
    fn clamp_never_raises_a_request_that_already_fits() {
        // The table only ever shrinks; a caller's smaller, deliberate value
        // must not be overridden upward toward the window.
        let table = WindowTable::builtin();
        assert_eq!(table.clamp("claude-sonnet-4-5", 4_096, 1_000), 4_096);
    }

    #[test]
    fn a_learned_window_beats_a_previous_learned_value_for_the_same_model() {
        let mut table = WindowTable::empty();
        table.learn("glm-5.3-flash", 200_000);
        table.learn("glm-5.3-flash", 262_144);
        assert_eq!(table.lookup("glm-5.3-flash"), Some(262_144));
    }

    #[test]
    fn a_learned_window_takes_effect_immediately_for_clamping() {
        let mut table = WindowTable::empty();
        assert_eq!(table.clamp("mystery-model", 100_000, 0), 100_000);
        table.learn("mystery-model", 2_000);
        assert!(table.clamp("mystery-model", 100_000, 0) < 100_000);
    }

    #[test]
    fn parses_the_window_from_openrouters_exact_wording() {
        let msg = "openrouter request failed (400 Bad Request): This endpoint's maximum \
                    context length is 262144 tokens. However, you requested about 264924 \
                    tokens (60428 of text input, 2352 of tool input, 202144 in the output). \
                    Please reduce the length of either one, or use the context-compression \
                    plugin to compress your prompt automatically.";
        assert_eq!(parse_window_from_error(msg), Some(262_144));
    }

    #[test]
    fn parses_a_comma_formatted_figure() {
        let msg = "the maximum context length is 1,000,000 tokens for this model";
        assert_eq!(parse_window_from_error(msg), Some(1_000_000));
    }

    #[test]
    fn an_unrelated_error_has_no_window_to_parse() {
        assert_eq!(
            parse_window_from_error("openai request failed (401): invalid api key"),
            None
        );
    }

    #[test]
    fn the_anchor_phrase_with_nothing_numeric_after_it_parses_to_nothing() {
        assert_eq!(
            parse_window_from_error("the context length is unknown right now"),
            None
        );
    }
}
