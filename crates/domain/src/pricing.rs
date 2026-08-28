//! Token pricing and cost estimation (FR-OUTPUT-01, M2 "showing cost").
//!
//! Rates are USD per *million* tokens. The built-in table is a convenience so
//! the TUI can show a running cost without any configuration; it is a public
//! list-price **estimate**, not a bill. Providers change prices, negotiate
//! discounts, and meter cache reads differently, so anything that matters
//! should be reconciled against the provider's own dashboard. Overriding an
//! entry (`[[pricing]]` in the config file) replaces the built-in rate.
//!
//! Stdlib only, like the rest of `domain` (FR-DI-01).

/// USD per million tokens for one model.
#[derive(Clone, Debug, PartialEq)]
pub struct PriceEntry {
    /// Matched as a prefix of the model id, vendor namespace stripped.
    pub model: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// Rate for tokens the provider reports as cached.
    pub cache_per_mtok: f64,
    /// True when the provider's reported `input_tokens` *already includes* the
    /// cached tokens (OpenAI family). Anthropic reports them separately, so
    /// charging both would double-count the prompt.
    pub cache_within_input: bool,
}

impl PriceEntry {
    pub fn new(model: &str, input: f64, output: f64, cache: f64, cache_within_input: bool) -> Self {
        Self {
            model: model.to_string(),
            input_per_mtok: input,
            output_per_mtok: output,
            cache_per_mtok: cache,
            cache_within_input,
        }
    }

    /// A model that costs nothing to run — anything self-hosted.
    pub fn free(model: &str) -> Self {
        Self::new(model, 0.0, 0.0, 0.0, true)
    }
}

/// The breakdown behind a cost figure, so the UI can explain the number.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub input_usd: f64,
    pub output_usd: f64,
    pub cache_usd: f64,
    /// False when no rate was known for the model, so the UI can say
    /// "unpriced" instead of showing a confident $0.00.
    pub priced: bool,
}

impl Cost {
    pub fn total_usd(&self) -> f64 {
        self.input_usd + self.output_usd + self.cache_usd
    }

    /// Adopt a figure the provider reported, in place of the estimate.
    ///
    /// Authoritative where available: it is what will appear on the bill, for
    /// the model actually served, at the rate actually charged — including
    /// models the local table has never heard of, which is otherwise the one
    /// case that renders `n/a` while real money is being spent. Recorded as
    /// input cost because the split is not reported and inventing one would be
    /// a worse lie than not splitting it.
    pub fn from_reported_usd(total: f64) -> Self {
        Self {
            input_usd: total,
            output_usd: 0.0,
            cache_usd: 0.0,
            priced: true,
        }
    }

    /// Accumulate another run's cost into this one, as the TUI does across
    /// the turns of a session. A single priced turn makes the total priced:
    /// reporting "n/a" for a session that did cost money would be worse than
    /// reporting a floor.
    pub fn add(&mut self, other: Cost) {
        self.input_usd += other.input_usd;
        self.output_usd += other.output_usd;
        self.cache_usd += other.cache_usd;
        self.priced |= other.priced;
    }

    /// Compact rendering for a status bar. Sub-cent runs still deserve a
    /// number, so the precision grows as the amount shrinks.
    pub fn render(&self) -> String {
        if !self.priced {
            return "n/a".to_string();
        }
        let total = self.total_usd();
        if total == 0.0 {
            "$0.00".to_string()
        } else if total < 0.0001 {
            // Four decimals would render a real charge as "$0.0000", which
            // reads as free. A tiny number is still not zero.
            "<$0.0001".to_string()
        } else if total < 0.01 {
            format!("${total:.4}")
        } else if total < 1.0 {
            format!("${total:.3}")
        } else {
            format!("${total:.2}")
        }
    }
}

/// An ordered set of price rules. Earlier entries win, so configured overrides
/// are simply prepended to the built-ins.
#[derive(Clone, Debug)]
pub struct PriceTable {
    entries: Vec<PriceEntry>,
}

impl Default for PriceTable {
    fn default() -> Self {
        Self::builtin()
    }
}

impl PriceTable {
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

    /// Put user-supplied rates ahead of the built-in ones.
    pub fn with_overrides(mut overrides: Vec<PriceEntry>) -> Self {
        overrides.extend(builtin_entries());
        Self { entries: overrides }
    }

    pub fn entries(&self) -> &[PriceEntry] {
        &self.entries
    }

    /// Longest matching prefix wins, so `gpt-4o-mini` is not priced as
    /// `gpt-4o`. Matching is case-insensitive and ignores an OpenRouter-style
    /// `vendor/` namespace and a trailing `:free`-style suffix.
    pub fn lookup(&self, model: &str) -> Option<&PriceEntry> {
        let key = normalize(model);
        // Longest prefix wins; on a tie the *earlier* entry wins, which is
        // what makes a configured override beat the built-in it shadows.
        // `max_by_key` keeps the last maximum, so fold explicitly.
        self.entries
            .iter()
            .filter(|e| key.starts_with(&normalize(&e.model)))
            .fold(None::<&PriceEntry>, |best, e| match best {
                Some(b) if b.model.len() >= e.model.len() => Some(b),
                _ => Some(e),
            })
    }

    /// Whether a cost can be quoted for this model at all.
    ///
    /// Not the same as "has an entry": an OpenRouter `:free` route has no
    /// entry and still costs a knowable zero. Callers that need to seed a
    /// running total must ask this rather than `lookup(..).is_some()`, or a
    /// free model reads "n/a" until the first turn contradicts it.
    pub fn knows(&self, model: &str) -> bool {
        self.estimate(model, 0, 0, 0).priced
    }

    /// Estimate the cost of one run. Unknown models yield `priced: false`
    /// rather than a misleading zero.
    pub fn estimate(&self, model: &str, input: u64, output: u64, cache: u64) -> Cost {
        let entry = match self.lookup(model) {
            Some(entry) => entry,
            None if is_free_route(model) => {
                return Cost {
                    priced: true,
                    ..Default::default()
                }
            }
            None => return Cost::default(),
        };
        // Do not bill the cached prefix twice: OpenAI-family `prompt_tokens`
        // already counts it, Anthropic's `input_tokens` does not.
        let billable_input = if entry.cache_within_input {
            input.saturating_sub(cache)
        } else {
            input
        };
        Cost {
            input_usd: per_mtok(billable_input, entry.input_per_mtok),
            output_usd: per_mtok(output, entry.output_per_mtok),
            cache_usd: per_mtok(cache, entry.cache_per_mtok),
            priced: true,
        }
    }
}

fn per_mtok(tokens: u64, rate: f64) -> f64 {
    tokens as f64 * rate / 1_000_000.0
}

/// Strip the vendor namespace (`openai/gpt-4o` → `gpt-4o`), any provider
/// routing suffix (`…:nitro`), and case, so one entry covers a model however
/// it is addressed.
///
/// Dots in version numbers become dashes, because the same model is spelled
/// both ways depending on who routes it: Anthropic calls it
/// `claude-3-5-haiku`, OpenRouter calls it `anthropic/claude-3.5-haiku`, and
/// an unrecognised spelling silently costs the user their cost display.
fn normalize(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    let base = base.split(':').next().unwrap_or(base);
    base.trim().to_ascii_lowercase().replace('.', "-")
}

/// OpenRouter marks zero-cost routes with a `:free` suffix. Trusting it beats
/// reporting "n/a" for a model we know is free.
fn is_free_route(model: &str) -> bool {
    model.contains(':')
        && model
            .rsplit(':')
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case("free"))
}

/// Public list prices, USD per million tokens. Kept sorted by family for
/// review; lookup order does not depend on position.
fn builtin_entries() -> Vec<PriceEntry> {
    vec![
        // -- OpenAI ---------------------------------------------------------
        PriceEntry::new("gpt-4o-mini", 0.15, 0.60, 0.075, true),
        PriceEntry::new("gpt-4o", 2.50, 10.00, 1.25, true),
        PriceEntry::new("gpt-4.1-nano", 0.10, 0.40, 0.025, true),
        PriceEntry::new("gpt-4.1-mini", 0.40, 1.60, 0.10, true),
        PriceEntry::new("gpt-4.1", 2.00, 8.00, 0.50, true),
        PriceEntry::new("o4-mini", 1.10, 4.40, 0.275, true),
        PriceEntry::new("o3-mini", 1.10, 4.40, 0.55, true),
        PriceEntry::new("o3", 2.00, 8.00, 0.50, true),
        // -- Anthropic ------------------------------------------------------
        PriceEntry::new("claude-haiku-4", 1.00, 5.00, 0.10, false),
        PriceEntry::new("claude-sonnet-4", 3.00, 15.00, 0.30, false),
        PriceEntry::new("claude-opus-4", 15.00, 75.00, 1.50, false),
        PriceEntry::new("claude-3-5-haiku", 0.80, 4.00, 0.08, false),
        PriceEntry::new("claude-3-5-sonnet", 3.00, 15.00, 0.30, false),
        PriceEntry::new("claude-3-opus", 15.00, 75.00, 1.50, false),
        // -- DeepSeek -------------------------------------------------------
        PriceEntry::new("deepseek-reasoner", 0.55, 2.19, 0.14, true),
        PriceEntry::new("deepseek-chat", 0.27, 1.10, 0.07, true),
        // -- Self-hosted: metered in electricity, not dollars ----------------
        PriceEntry::free("llama"),
        PriceEntry::free("qwen"),
        PriceEntry::free("mistral"),
        PriceEntry::free("gemma"),
        PriceEntry::free("codellama"),
        PriceEntry::free("deepseek-coder-v2"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        let table = PriceTable::builtin();
        // Without longest-prefix matching `gpt-4o-mini` would be priced as the
        // 16x more expensive `gpt-4o`.
        assert_eq!(table.lookup("gpt-4o-mini").unwrap().input_per_mtok, 0.15);
        assert_eq!(table.lookup("gpt-4o").unwrap().input_per_mtok, 2.50);
        assert_eq!(table.lookup("gpt-4.1-nano").unwrap().input_per_mtok, 0.10);
        assert_eq!(table.lookup("gpt-4.1").unwrap().input_per_mtok, 2.00);
    }

    #[test]
    fn vendor_namespace_and_routing_suffix_are_ignored() {
        let table = PriceTable::builtin();
        assert_eq!(
            table.lookup("openai/gpt-4o-mini").unwrap().model,
            "gpt-4o-mini"
        );
        assert_eq!(
            table.lookup("anthropic/claude-sonnet-4-5").unwrap().model,
            "claude-sonnet-4"
        );
        assert_eq!(
            table.lookup("openai/gpt-4o-mini:nitro").unwrap().model,
            "gpt-4o-mini"
        );
        assert_eq!(table.lookup("GPT-4O-MINI").unwrap().model, "gpt-4o-mini");
    }

    #[test]
    fn openrouter_dotted_spellings_resolve() {
        // Regression: `anthropic/claude-3.5-haiku` priced as n/a because the
        // table only carried Anthropic's own dashed spelling.
        let table = PriceTable::builtin();
        assert_eq!(
            table.lookup("anthropic/claude-3.5-haiku").unwrap().model,
            "claude-3-5-haiku"
        );
        assert_eq!(
            table.lookup("anthropic/claude-sonnet-4.5").unwrap().model,
            "claude-sonnet-4"
        );
        assert_eq!(
            table.lookup("openai/gpt-4.1-mini").unwrap().model,
            "gpt-4.1-mini"
        );
    }

    #[test]
    fn a_free_openrouter_route_costs_nothing_and_says_so() {
        let table = PriceTable::builtin();
        let cost = table.estimate("poolside/laguna-s-2.1:free", 100_000, 100_000, 0);
        assert!(
            cost.priced,
            "a :free route is known to be free, not unknown"
        );
        assert_eq!(cost.total_usd(), 0.0);
        // …but the same model without the suffix stays unpriced.
        assert!(!table.estimate("poolside/laguna-s-2.1", 1, 1, 0).priced);
    }

    #[test]
    fn knows_covers_free_routes_as_well_as_table_entries() {
        let table = PriceTable::builtin();
        assert!(table.knows("gpt-4o-mini"));
        assert!(table.knows("poolside/laguna-s-2.1:free"));
        assert!(!table.knows("poolside/laguna-s-2.1"));
        assert!(!table.knows("some-private-finetune"));
    }

    #[test]
    fn a_prefix_does_not_swallow_an_unrelated_model() {
        // `phind-codellama` must not be priced by a bare `phi` entry.
        assert!(PriceTable::builtin()
            .lookup("phind-codellama-34b")
            .is_none());
    }

    #[test]
    fn unknown_model_is_unpriced_not_free() {
        let table = PriceTable::builtin();
        let cost = table.estimate("some-private-finetune", 1000, 1000, 0);
        assert!(!cost.priced);
        assert_eq!(cost.render(), "n/a");
    }

    #[test]
    fn openai_style_cache_is_not_billed_twice() {
        let table = PriceTable::builtin();
        // 10k prompt tokens of which 8k were cached: 2k at full rate.
        let cost = table.estimate("gpt-4o-mini", 10_000, 0, 8_000);
        let expected = 2_000.0 * 0.15 / 1e6 + 8_000.0 * 0.075 / 1e6;
        assert!((cost.total_usd() - expected).abs() < 1e-12, "{cost:?}");
    }

    #[test]
    fn anthropic_style_cache_is_additive() {
        let table = PriceTable::builtin();
        // Anthropic reports input_tokens *excluding* the cached prefix.
        let cost = table.estimate("claude-sonnet-4-5", 10_000, 0, 8_000);
        let expected = 10_000.0 * 3.00 / 1e6 + 8_000.0 * 0.30 / 1e6;
        assert!((cost.total_usd() - expected).abs() < 1e-12, "{cost:?}");
    }

    #[test]
    fn output_dominates_a_typical_turn() {
        let table = PriceTable::builtin();
        let cost = table.estimate("claude-sonnet-4-5", 1_000, 1_000, 0);
        assert!(cost.output_usd > cost.input_usd);
        assert_eq!(cost.render(), "$0.018");
    }

    #[test]
    fn local_models_are_free_but_priced() {
        let table = PriceTable::builtin();
        let cost = table.estimate("llama3.2", 100_000, 100_000, 0);
        assert!(cost.priced);
        assert_eq!(cost.total_usd(), 0.0);
        assert_eq!(cost.render(), "$0.00");
    }

    #[test]
    fn overrides_beat_builtins() {
        let table =
            PriceTable::with_overrides(vec![PriceEntry::new("gpt-4o-mini", 99.0, 99.0, 0.0, true)]);
        assert_eq!(table.lookup("gpt-4o-mini").unwrap().input_per_mtok, 99.0);
        // …and leave everything else alone.
        assert_eq!(table.lookup("gpt-4o").unwrap().input_per_mtok, 2.50);
    }

    #[test]
    fn override_can_price_a_model_the_table_does_not_know() {
        let table =
            PriceTable::with_overrides(vec![PriceEntry::new("acme-1", 1.0, 2.0, 0.0, true)]);
        let cost = table.estimate("acme-1-preview", 1_000_000, 1_000_000, 0);
        assert!(cost.priced);
        assert_eq!(cost.total_usd(), 3.0);
    }

    #[test]
    fn rendering_scales_precision_to_the_amount() {
        let cheap = Cost {
            input_usd: 0.00012,
            priced: true,
            ..Default::default()
        };
        assert_eq!(cheap.render(), "$0.0001");
        // A real charge must never render as "$0.0000".
        let tiny = Cost {
            input_usd: 0.0000228,
            priced: true,
            ..Default::default()
        };
        assert_eq!(tiny.render(), "<$0.0001");
        // …and genuinely free stays "$0.00".
        let free = Cost {
            priced: true,
            ..Default::default()
        };
        assert_eq!(free.render(), "$0.00");
        let mid = Cost {
            input_usd: 0.125,
            priced: true,
            ..Default::default()
        };
        assert_eq!(mid.render(), "$0.125");
        let dear = Cost {
            input_usd: 12.5,
            priced: true,
            ..Default::default()
        };
        assert_eq!(dear.render(), "$12.50");
    }

    #[test]
    fn costs_accumulate_across_turns() {
        let table = PriceTable::builtin();
        let mut total = Cost::default();
        assert!(!total.priced);
        for _ in 0..3 {
            total.add(table.estimate("gpt-4o-mini", 1_000, 1_000, 0));
        }
        let one = table.estimate("gpt-4o-mini", 1_000, 1_000, 0);
        assert!((total.total_usd() - one.total_usd() * 3.0).abs() < 1e-12);
        assert!(total.priced);
    }

    #[test]
    fn one_priced_turn_makes_the_session_priced() {
        let table = PriceTable::builtin();
        let mut total = table.estimate("private-model", 1_000, 1_000, 0);
        total.add(table.estimate("gpt-4o-mini", 1_000, 1_000, 0));
        assert!(
            total.priced,
            "a session that spent money must show a figure"
        );
    }

    #[test]
    fn empty_table_prices_nothing() {
        assert!(!PriceTable::empty().estimate("gpt-4o", 1, 1, 0).priced);
    }
}
