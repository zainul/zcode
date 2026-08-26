//! Tool-name canonicalisation (pure, stdlib-only).
//!
//! The PRD spells namespaced tools `mcp::<server>::<tool>`, `lsp::hover` and
//! `zcode:skill`. Provider function-calling APIs (OpenAI, Anthropic, OpenRouter)
//! only accept `[A-Za-z0-9_-]{1,64}` for a function name, so `:` cannot go on
//! the wire. The canonical form replaces every `:` with `_` — giving
//! `mcp__<server>__<tool>`, `lsp__hover`, `zcode_skill` — while the PRD spelling
//! stays valid as an alias everywhere a tool is looked up.
//!
//! Both the registry (dispatch) and the mode policy (gating) canonicalise
//! through this one function so they can never disagree about what a name means.

/// Canonical wire form of a tool name.
pub fn canonical_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        match ch {
            ':' => out.push('_'),
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => out.push(c),
            // Anything else a provider would reject (spaces, dots, slashes).
            _ => out.push('_'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_prd_spellings_to_wire_names() {
        assert_eq!(canonical_tool_name("zcode:skill"), "zcode_skill");
        assert_eq!(canonical_tool_name("lsp::hover"), "lsp__hover");
        assert_eq!(
            canonical_tool_name("mcp::everything::echo"),
            "mcp__everything__echo"
        );
    }

    #[test]
    fn leaves_already_canonical_names_untouched() {
        assert_eq!(canonical_tool_name("read"), "read");
        assert_eq!(
            canonical_tool_name("str_replace_editor"),
            "str_replace_editor"
        );
        assert_eq!(canonical_tool_name("mcp__srv__tool"), "mcp__srv__tool");
    }

    #[test]
    fn sanitises_characters_providers_reject() {
        assert_eq!(canonical_tool_name(" read file.rs "), "read_file_rs");
    }
}
