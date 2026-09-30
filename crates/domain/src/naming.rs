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

/// Which stage of the context pipeline a tool belongs to (FR-BUDGET-01, PRD
/// §4.1), for attributing the tokens its results cost.
///
/// Tools that do not exist yet in a given build are listed anyway, so adding
/// one never needs an edit here. `str_replace_editor` is a `change` tool even
/// though its `view` command reads — the engine refines that per call with
/// [`tool_category_for_call`].
pub fn tool_category(name: &str) -> &'static str {
    match canonical_tool_name(name).as_str() {
        "list_dir" | "glob" => "discover",
        "grep" | "symbols" | "related" => "locate",
        "read" | "outline" | "lsp__hover" | "lsp__goto_definition" | "lsp__find_references" => {
            "inspect"
        }
        "write" | "str_replace_editor" | "apply_patch" | "edit_symbol" | "lsp__rename_symbol" => {
            "change"
        }
        "lsp__diagnostics" => "verify",
        "shell" => "shell",
        n if n.starts_with("mcp__") => "mcp",
        _ => "other",
    }
}

/// [`tool_category`], refined by the call's raw arguments where one tool
/// spans two stages. A substring check rather than a JSON parse: `domain` has
/// no JSON parser (FR-DI-01), and a misclassified call only mislabels a
/// telemetry bucket.
pub fn tool_category_for_call(name: &str, args_json: &str) -> &'static str {
    let category = tool_category(name);
    if category == "change"
        && canonical_tool_name(name) == "str_replace_editor"
        && (args_json.contains("\"command\":\"view\"")
            || args_json.contains("\"command\": \"view\""))
    {
        return "inspect";
    }
    category
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

    #[test]
    fn every_native_tool_has_a_real_category() {
        for name in [
            "read",
            "write",
            "str_replace_editor",
            "apply_patch",
            "list_dir",
            "shell",
            "grep",
            "glob",
            "outline",
            "symbols",
            "related",
            "edit_symbol",
            "lsp__hover",
            "lsp__goto_definition",
            "lsp__find_references",
            "lsp__rename_symbol",
            "lsp__diagnostics",
        ] {
            assert_ne!(tool_category(name), "other", "{name}");
        }
        assert_eq!(tool_category("mcp__notion__search"), "mcp");
        assert_eq!(tool_category("zcode_skill"), "other");
    }

    #[test]
    fn editor_view_is_inspection_not_change() {
        let view = r#"{"command":"view","path":"a.rs"}"#;
        let edit = r#"{"command":"str_replace","path":"a.rs"}"#;
        assert_eq!(
            tool_category_for_call("str_replace_editor", view),
            "inspect"
        );
        assert_eq!(tool_category_for_call("str_replace_editor", edit), "change");
        assert_eq!(tool_category_for_call("read", view), "inspect");
    }
}
