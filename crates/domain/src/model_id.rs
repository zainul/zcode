//! Model-id normalisation shared by every table that is keyed on a model
//! name (`pricing`, `context_window`).
//!
//! Both tables must agree on what counts as "the same model spelled two
//! ways" — a mismatch here would mean a model priced under one spelling and
//! window-clamped under another, silently disagreeing about the same
//! request. One function, used by both, is what keeps that impossible.
//!
//! Stdlib only, like the rest of `domain` (FR-DI-01).

/// Strip the vendor namespace (`openai/gpt-4o` → `gpt-4o`), any provider
/// routing suffix (`…:nitro`), and case, so one entry covers a model however
/// it is addressed.
///
/// Dots in version numbers become dashes, because the same model is spelled
/// both ways depending on who routes it: Anthropic calls it
/// `claude-3-5-haiku`, OpenRouter calls it `anthropic/claude-3.5-haiku`, and
/// an unrecognised spelling silently drops the model out of the table.
pub fn normalize(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    let base = base.split(':').next().unwrap_or(base);
    base.trim().to_ascii_lowercase().replace('.', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_vendor_namespace_suffix_and_case() {
        assert_eq!(
            normalize("Anthropic/Claude-3.5-Haiku:nitro"),
            "claude-3-5-haiku"
        );
    }

    #[test]
    fn leaves_a_bare_model_id_alone() {
        assert_eq!(normalize("gpt-4o-mini"), "gpt-4o-mini");
    }
}
