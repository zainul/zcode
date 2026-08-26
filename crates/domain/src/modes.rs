//! Per-mode system prompt templates (FR-MODE-03). These are orchestration-only
//! constants — no external files — kept in `domain` so the engine owns the
//! mode policy. `build` mode makes edits directly; `planning` mode refuses
//! execute-side tools and asks for confirmation.

use crate::AgentMode;

pub fn system_prompt(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Planning => {
            "You are a planning coding agent. Propose edits, ask for confirmation. \
             Do NOT call write, str_replace_editor, apply_patch, shell, or rename tools.\
             Only use read-only tools (read, list_dir, hover, find_references, \
             MCP read tools) to investigate, then describe the plan."
        }
        AgentMode::Build => {
            "You are an autonomous coding agent. Make edits directly using the \
             available tools. Be efficient and iterative: edit, then verify."
        }
    }
}

/// The set of tool names that mutate the filesystem, execute shell commands,
/// or rewrite symbols. In `Planning` mode the engine refuses these
/// (FR-MODE-01/02). Names are given in canonical wire form; see
/// [`crate::canonical_tool_name`].
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
    fn read_only_tools_stay_available_in_planning() {
        assert!(!is_execute_only("read"));
        // Loading a markdown note is read-only: planning mode is exactly when
        // house conventions should inform the proposal.
        assert!(!is_execute_only("zcode_skill"));
        assert!(!is_execute_only("zcode:skill"));
        assert!(!is_execute_only("list_dir"));
        assert!(!is_execute_only("lsp__hover"));
        assert!(!is_execute_only("lsp__find_references"));
        assert!(!is_execute_only("mcp__everything__search"));
    }

    #[test]
    fn planning_prompt_forbids_edits() {
        assert!(system_prompt(AgentMode::Planning).contains("Do NOT call"));
        assert!(system_prompt(AgentMode::Build).contains("autonomous"));
    }
}
