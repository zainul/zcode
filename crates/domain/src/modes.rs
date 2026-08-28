//! Per-mode system prompts and tool gating (FR-MODE-01/02/03).
//!
//! These are orchestration-only constants — no external files — kept in
//! `domain` so the engine owns the mode policy and no adapter can quietly
//! disagree with it.
//!
//! The three modes form a ladder (see [`crate::AgentMode`]): `planning` may
//! only read, `editing` may also write files, `auto` may also run commands.
//! Gating is expressed as *what each mode denies* so adding a tool without
//! classifying it fails closed for the restrictive modes.

use crate::AgentMode;

pub fn system_prompt(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Planning => {
            "You are a planning coding agent. Propose edits, ask for confirmation. \
             Do NOT call write, str_replace_editor, apply_patch, shell, or rename tools.\
             Only use read-only tools (read, list_dir, hover, find_references, \
             MCP read tools) to investigate, then describe the plan."
        }
        AgentMode::Editing => {
            "You are a coding agent working in edit-only mode. Make edits directly \
             with the file tools (write, str_replace_editor, apply_patch). You may \
             NOT run shell commands — the `shell` tool is disabled, so do not call \
             it and do not plan around running builds or tests yourself. When a \
             change needs verifying, say which command the user should run."
        }
        AgentMode::Auto => {
            "You are an autonomous coding agent. Make edits directly using the \
             available tools. Be efficient and iterative: edit, then verify."
        }
    }
}

/// Tools that mutate the filesystem or rewrite symbols. Denied in `planning`.
pub fn write_tool_names() -> &'static [&'static str] {
    &[
        "write",
        "str_replace_editor",
        "apply_patch",
        "lsp__rename_symbol",
    ]
}

/// Tools that execute arbitrary commands. Denied in `planning` *and* `editing`.
pub fn shell_tool_names() -> &'static [&'static str] {
    &["shell"]
}

/// Every tool that is not purely read-only, i.e. everything `planning` refuses.
/// Names are given in canonical wire form; see [`crate::canonical_tool_name`].
pub fn execute_only_tool_names() -> &'static [&'static str] {
    &[
        "write",
        "str_replace_editor",
        "apply_patch",
        "shell",
        "lsp__rename_symbol",
    ]
}

/// Returns true if `name` is an execute-side tool. The name is canonicalised
/// first, so the PRD spellings (`zcode:skill`, `lsp::rename_symbol`) and the wire
/// spellings (`zcode_skill`, `lsp__rename_symbol`) are both recognised — a
/// planning-mode gate that missed an alias would be a security hole.
pub fn is_execute_only(name: &str) -> bool {
    let canonical = crate::canonical_tool_name(name);
    execute_only_tool_names().iter().any(|n| canonical == *n)
}

/// Whether `mode` refuses to call `name`.
///
/// This is the single authority: both the tool-spec filter (what the model is
/// even told about) and the dispatch gate (what actually runs) call it, so the
/// list the model sees can never disagree with the list it is allowed to use.
pub fn denies(mode: AgentMode, name: &str) -> bool {
    let canonical = crate::canonical_tool_name(name);
    let in_set = |set: &[&str]| set.iter().any(|n| canonical == *n);
    match mode {
        AgentMode::Planning => in_set(write_tool_names()) || in_set(shell_tool_names()),
        AgentMode::Editing => in_set(shell_tool_names()),
        AgentMode::Auto => false,
    }
}

/// Why a tool was refused, for the message fed back to the model and the user.
pub fn denial_reason(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Planning => "planning mode is read-only",
        AgentMode::Editing => "editing mode does not allow shell commands",
        AgentMode::Auto => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates_execute_side_tools_in_both_spellings() {
        assert!(is_execute_only("write"));
        assert!(is_execute_only("str_replace_editor"));
        assert!(is_execute_only("apply_patch"));
        assert!(is_execute_only("shell"));
        assert!(is_execute_only("lsp::rename_symbol"));
        assert!(is_execute_only("lsp__rename_symbol"));
    }

    #[test]
    fn read_only_tools_are_never_gated() {
        for name in ["read", "list_dir", "zcode_skill", "lsp__hover"] {
            assert!(!is_execute_only(name), "{name} must stay available");
            for mode in AgentMode::all() {
                assert!(!denies(*mode, name), "{mode:?} must allow {name}");
            }
        }
    }

    #[test]
    fn planning_denies_writes_and_shell() {
        assert!(denies(AgentMode::Planning, "write"));
        assert!(denies(AgentMode::Planning, "apply_patch"));
        assert!(denies(AgentMode::Planning, "shell"));
        assert!(denies(AgentMode::Planning, "lsp::rename_symbol"));
    }

    #[test]
    fn editing_allows_writes_but_denies_shell() {
        assert!(!denies(AgentMode::Editing, "write"));
        assert!(!denies(AgentMode::Editing, "apply_patch"));
        assert!(!denies(AgentMode::Editing, "str_replace_editor"));
        assert!(!denies(AgentMode::Editing, "lsp__rename_symbol"));
        assert!(denies(AgentMode::Editing, "shell"));
    }

    #[test]
    fn auto_allows_everything() {
        for name in execute_only_tool_names() {
            assert!(!denies(AgentMode::Auto, name));
        }
    }

    #[test]
    fn every_execute_only_tool_is_classified() {
        // A tool added to the deny list but to neither category would silently
        // stay available in `editing`.
        for name in execute_only_tool_names() {
            let known = write_tool_names().contains(name) || shell_tool_names().contains(name);
            assert!(known, "{name} is unclassified");
        }
    }

    #[test]
    fn prompts_describe_their_mode() {
        assert!(system_prompt(AgentMode::Planning).contains("planning"));
        assert!(system_prompt(AgentMode::Editing).contains("edit-only"));
        assert!(system_prompt(AgentMode::Auto).contains("autonomous"));
    }

    #[test]
    fn modes_cycle_through_every_variant() {
        let mut seen = vec![AgentMode::Planning];
        let mut mode = AgentMode::Planning;
        for _ in 0..2 {
            mode = mode.next();
            seen.push(mode);
        }
        assert_eq!(seen, AgentMode::all().to_vec());
        assert_eq!(mode.next(), AgentMode::Planning);
    }
}
