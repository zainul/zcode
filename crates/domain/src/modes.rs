//! Per-mode system prompt templates (FR-MODE-03). These are orchestration-only
//! constants — no external files — kept in `domain` so the engine owns the
//! mode policy. `build` mode makes edits directly; `planning` mode refuses
//! execute-side tools and asks for confirmation.

use crate::AgentMode;

pub fn system_prompt(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Planning => {
            "You are a planning coding agent. Propose edits, ask for confirmation. \
             Do NOT call write, str_replace_editor, shell, or rename tools.\
             Only use read-only tools (read, list_dir, hover, find_references, \
             MCP read tools) to investigate, then describe the plan."
        }
        AgentMode::Build => {
            "You are an autonomous coding agent. Make edits directly using the \
             available tools. Be efficient and iterative: edit, then verify."
        }
    }
}

/// The set of native tool names that mutate the filesystem / execute shell.
/// In `Planning` mode the engine refuses these (FR-MODE-01/02).
pub fn execute_only_tool_names() -> &'static [&'static str] {
    &["write", "str_replace_editor", "shell", "ag:skill"]
}

/// Returns true if `name` is an execute-side native tool (or an LSP rename).
pub fn is_execute_only(name: &str) -> bool {
    if name == "lsp::rename_symbol" {
        return true;
    }
    execute_only_tool_names().iter().any(|n| name == *n)
}
