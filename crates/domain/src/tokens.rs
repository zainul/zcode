//! Pure token-estimation heuristic used only as a fallback when a provider
//! omits `usage` (e.g. some Ollama builds). Provider-reported counts are
//! authoritative (DQ2); this stays in `domain` (stdlib-only) on purpose.

/// Rough estimate: ~4 chars per token via whitespace splitting (FR-DI-01:
/// no tokenizer crate in domain).
pub fn estimate_tokens(text: &str) -> u64 {
    text.split_whitespace().count() as u64 * 4
}
