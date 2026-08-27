//! The single tool namespace the engine sees (DQ10, FR-MCP-03/04/05, FR-LSP-02).
//!
//! `ToolRegistry` merges three backends behind `domain::ToolRegistryPort`:
//!
//! | backend | wire names                    |
//! |---------|-------------------------------|
//! | native  | `read`, `write`, `str_replace_editor`, `apply_patch`, `list_dir`, `shell`, `zcode_skill` |
//! | MCP     | `mcp__<server>__<tool>`       |
//! | LSP     | `lsp__goto_definition`, `lsp__find_references`, `lsp__hover`, `lsp__rename_symbol` |
//!
//! Names are canonicalised through [`domain::canonical_tool_name`], so the PRD
//! spellings (`mcp::srv::tool`, `zcode:skill`) dispatch identically to the wire
//! spellings a provider will actually emit.
#![cfg_attr(not(test), forbid(unsafe_code))]

pub mod guard;
pub mod native;
pub mod patch;
pub mod skills;

use std::path::{Path, PathBuf};

use domain::{
    canonical_tool_name, BoxError, LspLocation, LspPort, LspWorkspaceEdit, McpPort, Tool,
    ToolRegistryPort, ToolResult, ToolSpec,
};

pub use guard::{allowlist_is_unrestricted, builtin_deny_rule_count, GuardedShell, ShellToolError};
pub use native::{
    ApplyPatchTool, ListDirTool, ReadTool, ShellTool, SkillTool, StrReplaceTool, WriteTool,
    TOOL_APPLY_PATCH, TOOL_LIST_DIR, TOOL_READ, TOOL_SHELL, TOOL_SKILL, TOOL_STR_REPLACE,
    TOOL_WRITE,
};
pub use patch::{apply_patch, parse_unified_diff, PatchError};
pub use skills::{SkillEntry, SkillIndex};

pub const LSP_GOTO_DEFINITION: &str = "lsp__goto_definition";
pub const LSP_FIND_REFERENCES: &str = "lsp__find_references";
pub const LSP_HOVER: &str = "lsp__hover";
pub const LSP_RENAME_SYMBOL: &str = "lsp__rename_symbol";

const MCP_PREFIX: &str = "mcp__";

/// Wire name for a tool exposed by an MCP server.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!(
        "{MCP_PREFIX}{}__{}",
        canonical_tool_name(server),
        canonical_tool_name(tool)
    )
}

struct NativeEntry {
    name: String,
    tool: Box<dyn Tool + Send>,
}

struct McpEntry {
    /// `mcp__<server>__` — the dispatch prefix for this server.
    prefix: String,
    /// (wire name, original tool name, spec) for each discovered tool.
    tools: Vec<(String, String, ToolSpec)>,
    port: Box<dyn McpPort + Send>,
}

struct LspEntry {
    port: Box<dyn LspPort + Send>,
}

/// The merged registry handed to the engine.
pub struct ToolRegistry {
    native: Vec<NativeEntry>,
    mcp: Vec<McpEntry>,
    lsp: Option<LspEntry>,
    /// Working directory used to turn model-supplied relative paths into URIs.
    root: PathBuf,
    /// Non-fatal setup problems (e.g. an MCP server that would not start).
    /// The CLI logs these; the agent runs with whatever did come up (FR-MCP-05).
    warnings: Vec<String>,
}

impl ToolRegistry {
    /// Empty registry rooted at `root`; build it up with the `with_*` methods.
    pub fn new(root: PathBuf) -> Self {
        Self {
            native: Vec::new(),
            mcp: Vec::new(),
            lsp: None,
            root,
            warnings: Vec::new(),
        }
    }

    pub fn with_native(mut self, tool: Box<dyn Tool + Send>) -> Self {
        let name = canonical_tool_name(&tool.spec().name);
        self.native.push(NativeEntry { name, tool });
        self
    }

    /// Register an MCP server, discovering its tools once at boot
    /// (FR-MCP-03). A server whose `tools/list` fails is recorded as a warning
    /// and skipped rather than taking the whole agent down (FR-MCP-05).
    pub fn with_mcp(mut self, server: &str, mut port: Box<dyn McpPort + Send>) -> Self {
        match port.list_tools() {
            Ok(defs) => {
                let tools = defs
                    .iter()
                    .map(|def| {
                        let wire = mcp_tool_name(server, &def.name);
                        let spec = ToolSpec {
                            name: wire.clone(),
                            description: if def.description.is_empty() {
                                format!("MCP tool `{}` from server `{server}`", def.name)
                            } else {
                                def.description.clone()
                            },
                            params_json: def.input_schema.clone(),
                        };
                        (wire, def.name.clone(), spec)
                    })
                    .collect();
                self.mcp.push(McpEntry {
                    prefix: format!("{MCP_PREFIX}{}__", canonical_tool_name(server)),
                    tools,
                    port,
                });
            }
            Err(e) => self
                .warnings
                .push(format!("mcp server `{server}` skipped: {e}")),
        }
        self
    }

    pub fn with_lsp(mut self, port: Box<dyn LspPort + Send>) -> Self {
        self.lsp = Some(LspEntry { port });
        self
    }

    pub fn warn(&mut self, message: String) {
        self.warnings.push(message);
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The full native + MCP + LSP tool set for a working directory, built
    /// from configuration. MCP/LSP servers that fail to start are skipped with
    /// a warning (FR-MCP-05); the agent still runs.
    pub fn from_config(cfg: &infra_config::Config) -> Result<Self, ShellToolError> {
        let root = cfg.working_dir.clone();
        let shell = GuardedShell::with_denylist(
            infra_shell::StdShell::new(),
            &cfg.shell_allowed,
            &cfg.shell_denied,
        )?;

        // `mut` is only used by the feature-gated MCP/LSP blocks below.
        #[allow(unused_mut)]
        let mut registry = Self::new(root.clone())
            .with_native(Box::new(ReadTool::new(root.clone())))
            .with_native(Box::new(WriteTool::new(root.clone())))
            .with_native(Box::new(StrReplaceTool::new(root.clone())))
            .with_native(Box::new(ApplyPatchTool::new(root.clone())))
            .with_native(Box::new(ListDirTool::new(root.clone())))
            .with_native(Box::new(ShellTool::new(
                root.clone(),
                shell,
                cfg.timeout_ms,
            )));

        // Advertising a skill tool with nothing to load wastes prompt budget
        // and invites the model to guess names.
        let skills = SkillIndex::discover(&cfg.skills_dirs());
        if !skills.is_empty() {
            registry = registry.with_native(Box::new(SkillTool::new(skills)));
        }

        #[cfg(feature = "mcp")]
        for server in cfg.mcp_servers.iter() {
            match infra_mcp::McpClient::with_timeout(
                &server.command,
                &server.args,
                &server.env,
                cfg.timeout_ms,
            ) {
                Ok(client) => registry = registry.with_mcp(&server.name, Box::new(client)),
                Err(e) => {
                    registry.warn(format!("mcp server `{}` failed to start: {e}", server.name))
                }
            }
        }

        // One language server per run: the first server that starts wins, and
        // `effective_lsp_servers` has already sorted the project's own
        // language to the front. Multi-server routing by file extension is a
        // v0.3 concern.
        #[cfg(feature = "lsp")]
        for server in cfg.effective_lsp_servers().iter() {
            match infra_lsp::LspClient::start_with_timeout(
                &server.command,
                &server.args,
                &server.env,
                &root,
                cfg.timeout_ms,
            ) {
                Ok(client) => {
                    registry = registry.with_lsp(Box::new(client));
                    break;
                }
                Err(e) => registry.warn(format!(
                    "lsp server `{}` failed to start: {e}",
                    server.language
                )),
            }
        }

        Ok(registry)
    }

    fn native_index(&self, canonical: &str) -> Option<usize> {
        self.native.iter().position(|e| e.name == canonical)
    }

    /// `file://` URI for a model-supplied `uri` or `path` argument.
    fn uri_from_args(&self, args: &serde_json::Value) -> Option<String> {
        if let Some(uri) = args.get("uri").and_then(|v| v.as_str()) {
            return Some(uri.to_string());
        }
        let path = args.get("path").and_then(|v| v.as_str())?;
        let resolved = native::resolve(&self.root, path);
        let absolute = resolved.canonicalize().unwrap_or(resolved);
        Some(file_uri(&absolute))
    }

    /// After a successful edit, push the new text to the language server so
    /// `find_references`/`rename` reflect our changes (FR-LSP-04).
    fn sync_lsp_document(&mut self, args: &serde_json::Value) {
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return;
        };
        let resolved = native::resolve(&self.root, path);
        let absolute = resolved.canonicalize().unwrap_or(resolved);
        if let Ok(text) = std::fs::read_to_string(&absolute) {
            let _ = lsp.port.open_document(&file_uri(&absolute), &text);
        }
    }

    fn call_lsp(&mut self, canonical: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match native::parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        if self.lsp.is_none() {
            return Ok(native::tool_error("no language server is configured"));
        }
        // Resolve the URI before borrowing the port mutably.
        let Some(uri) = self.uri_from_args(&args) else {
            return Ok(native::tool_error(
                "missing required argument `path` (or `uri`)",
            ));
        };
        let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let character = args.get("character").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let lsp = self.lsp.as_mut().expect("checked above");

        let result = match canonical {
            LSP_GOTO_DEFINITION => match lsp.port.goto_definition(&uri, line, character) {
                Ok(loc) => ToolResult::ok(&format_location(&loc)),
                Err(e) => native::tool_error(e.to_string()),
            },
            LSP_FIND_REFERENCES => match lsp.port.find_references(&uri, line, character) {
                Ok(locs) if locs.is_empty() => ToolResult::ok("no references found"),
                Ok(locs) => ToolResult::ok(
                    &locs
                        .iter()
                        .map(format_location)
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Err(e) => native::tool_error(e.to_string()),
            },
            LSP_HOVER => match lsp.port.hover(&uri, line, character) {
                Ok(text) => ToolResult::ok(&text),
                Err(e) => native::tool_error(e.to_string()),
            },
            LSP_RENAME_SYMBOL => {
                let new_name = args
                    .get("new_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if new_name.is_empty() {
                    return Ok(native::tool_error("missing required argument `new_name`"));
                }
                match lsp.port.rename_symbol(&uri, line, character, new_name) {
                    Ok(edit) => ToolResult::ok(&format_workspace_edit(&edit)),
                    Err(e) => native::tool_error(e.to_string()),
                }
            }
            other => native::tool_error(format!("unknown lsp tool `{other}`")),
        };
        Ok(result)
    }
}

impl ToolRegistryPort for ToolRegistry {
    fn list(&self) -> Box<[ToolSpec]> {
        let lsp_count = if self.lsp.is_some() { 4 } else { 0 };
        let mcp_count: usize = self.mcp.iter().map(|e| e.tools.len()).sum();
        let mut specs = Vec::with_capacity(self.native.len() + mcp_count + lsp_count);

        for entry in &self.native {
            specs.push(entry.tool.spec());
        }
        for entry in &self.mcp {
            for (_, _, spec) in &entry.tools {
                specs.push(spec.clone());
            }
        }
        if self.lsp.is_some() {
            specs.extend(lsp_tool_specs());
        }
        specs.into_boxed_slice()
    }

    fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let canonical = canonical_tool_name(name);

        if let Some(index) = self.native_index(&canonical) {
            let result = self.native[index].tool.call(&canonical, args_json)?;
            // Keep the language server's view of edited files current.
            if result.error.is_none()
                && (canonical == TOOL_WRITE || canonical == TOOL_STR_REPLACE)
                && self.lsp.is_some()
            {
                if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_json) {
                    self.sync_lsp_document(&args);
                }
            }
            return Ok(result);
        }

        if canonical.starts_with(MCP_PREFIX) {
            for entry in self.mcp.iter_mut() {
                // Cheap reject on the server prefix before scanning its tools.
                if !canonical.starts_with(&entry.prefix) {
                    continue;
                }
                if let Some((_, original, _)) =
                    entry.tools.iter().find(|(wire, _, _)| wire == &canonical)
                {
                    let original = original.clone();
                    return match entry.port.call(&original, args_json.to_string()) {
                        Ok(content) => Ok(ToolResult::ok(&content)),
                        // An MCP failure is reported to the model, not fatal.
                        Err(e) => Ok(native::tool_error(e.to_string())),
                    };
                }
            }
            return Ok(native::tool_error(format!("unknown MCP tool `{name}`")));
        }

        if canonical.starts_with("lsp__") {
            return self.call_lsp(&canonical, args_json);
        }

        Ok(native::tool_error(format!("unknown tool `{name}`")))
    }

    fn is_native(&self, name: &str) -> bool {
        self.native_index(&canonical_tool_name(name)).is_some()
    }
}

fn lsp_tool_specs() -> Vec<ToolSpec> {
    let position_schema = r#"{"type":"object","properties":{"path":{"type":"string","description":"File path"},"line":{"type":"integer","description":"0-based line"},"character":{"type":"integer","description":"0-based column"}},"required":["path","line","character"]}"#;
    vec![
        ToolSpec {
            name: LSP_GOTO_DEFINITION.into(),
            description: "Resolve the definition site of the symbol at a position.".into(),
            params_json: position_schema.into(),
        },
        ToolSpec {
            name: LSP_FIND_REFERENCES.into(),
            description: "List every reference to the symbol at a position.".into(),
            params_json: position_schema.into(),
        },
        ToolSpec {
            name: LSP_HOVER.into(),
            description: "Type and documentation for the symbol at a position.".into(),
            params_json: position_schema.into(),
        },
        ToolSpec {
            name: LSP_RENAME_SYMBOL.into(),
            description: "Compute the edits that rename a symbol. The edits are advice — \
                          apply them with str_replace_editor."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"},"new_name":{"type":"string"}},"required":["path","line","character","new_name"]}"#.into(),
        },
    ]
}

fn format_location(loc: &LspLocation) -> String {
    format!(
        "{}:{}:{}",
        loc.uri,
        loc.range.start.line + 1,
        loc.range.start.character + 1
    )
}

fn format_workspace_edit(edit: &LspWorkspaceEdit) -> String {
    if edit.changes.is_empty() {
        return "no edits proposed".to_string();
    }
    let mut out = String::with_capacity(edit.changes.len() * 48);
    out.push_str("proposed edits (apply with str_replace_editor):\n");
    for change in edit.changes.iter() {
        out.push_str(&format!(
            "{}:{}:{} -> {:?}\n",
            change.uri,
            change.range.start.line + 1,
            change.range.start.character + 1,
            change.new_text
        ));
    }
    out
}

/// `file://` URI for an absolute path. Kept local so the registry does not
/// need the optional `infra-lsp` dependency just to build a URI.
fn file_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len() + 8);
    out.push_str("file://");
    for ch in raw.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{LspPosition, LspRange, LspTextEdit, McpToolDef};

    /// Canned MCP server: records the calls it receives.
    struct FakeMcp {
        calls: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    impl McpPort for FakeMcp {
        fn list_tools(&mut self) -> Result<Box<[McpToolDef]>, BoxError> {
            Ok(Box::new([McpToolDef {
                name: "search".into(),
                description: "Search things".into(),
                input_schema: r#"{"type":"object"}"#.into(),
            }]))
        }
        fn call(&mut self, name: &str, args_json: String) -> Result<String, BoxError> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), args_json));
            Ok("mcp result".into())
        }
        fn ping(&mut self) -> Result<bool, BoxError> {
            Ok(true)
        }
    }

    /// An MCP server that is up but whose discovery fails (FR-MCP-05).
    struct BrokenMcp;
    impl McpPort for BrokenMcp {
        fn list_tools(&mut self) -> Result<Box<[McpToolDef]>, BoxError> {
            Err("server exploded".into())
        }
        fn call(&mut self, _name: &str, _args: String) -> Result<String, BoxError> {
            Err("server exploded".into())
        }
        fn ping(&mut self) -> Result<bool, BoxError> {
            Ok(false)
        }
    }

    #[derive(Clone, Default)]
    struct OpenedDocs(std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>);

    struct FakeLsp {
        opened: OpenedDocs,
    }

    impl LspPort for FakeLsp {
        fn goto_definition(
            &mut self,
            uri: &str,
            _line: u32,
            _character: u32,
        ) -> Result<LspLocation, BoxError> {
            Ok(LspLocation {
                uri: uri.to_string(),
                range: LspRange {
                    start: LspPosition {
                        line: 41,
                        character: 3,
                    },
                    end: LspPosition {
                        line: 41,
                        character: 9,
                    },
                },
            })
        }
        fn find_references(
            &mut self,
            _uri: &str,
            _line: u32,
            _character: u32,
        ) -> Result<Box<[LspLocation]>, BoxError> {
            Ok(Box::new([]))
        }
        fn hover(&mut self, _uri: &str, _line: u32, _character: u32) -> Result<String, BoxError> {
            Ok("fn foo()".into())
        }
        fn rename_symbol(
            &mut self,
            uri: &str,
            _line: u32,
            _character: u32,
            new_name: &str,
        ) -> Result<LspWorkspaceEdit, BoxError> {
            Ok(LspWorkspaceEdit {
                changes: Box::new([LspTextEdit {
                    uri: uri.to_string(),
                    range: LspRange {
                        start: LspPosition {
                            line: 0,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 0,
                            character: 3,
                        },
                    },
                    new_text: new_name.to_string(),
                }]),
            })
        }
        fn open_document(&mut self, uri: &str, text: &str) -> Result<(), BoxError> {
            self.opened
                .0
                .lock()
                .unwrap()
                .push((uri.to_string(), text.to_string()));
            Ok(())
        }
    }

    fn registry_with_native(root: PathBuf) -> ToolRegistry {
        ToolRegistry::new(root.clone())
            .with_native(Box::new(ReadTool::new(root.clone())))
            .with_native(Box::new(WriteTool::new(root)))
    }

    #[test]
    fn merges_native_and_mcp_specs() {
        let dir = tempfile::tempdir().unwrap();
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = registry_with_native(dir.path().to_path_buf()).with_mcp(
            "everything",
            Box::new(FakeMcp {
                calls: calls.clone(),
            }),
        );
        let names: Vec<String> = registry.list().iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&TOOL_READ.to_string()));
        assert!(names.contains(&"mcp__everything__search".to_string()));
    }

    #[test]
    fn dispatches_native_and_mcp_calls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "body").unwrap();
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = registry_with_native(dir.path().to_path_buf()).with_mcp(
            "everything",
            Box::new(FakeMcp {
                calls: calls.clone(),
            }),
        );

        let native = registry.call(TOOL_READ, r#"{"path":"a.txt"}"#).unwrap();
        assert_eq!(native.content, "body");

        let mcp = registry
            .call("mcp__everything__search", r#"{"q":"x"}"#)
            .unwrap();
        assert_eq!(mcp.content, "mcp result");
        // The server receives its own unprefixed tool name.
        assert_eq!(calls.lock().unwrap()[0].0, "search");
    }

    #[test]
    fn accepts_prd_spelling_as_an_alias() {
        let dir = tempfile::tempdir().unwrap();
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = registry_with_native(dir.path().to_path_buf()).with_mcp(
            "everything",
            Box::new(FakeMcp {
                calls: calls.clone(),
            }),
        );
        let res = registry
            .call("mcp::everything::search", r#"{"q":"x"}"#)
            .unwrap();
        assert_eq!(res.content, "mcp result");
    }

    #[test]
    fn broken_mcp_server_is_skipped_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            registry_with_native(dir.path().to_path_buf()).with_mcp("broken", Box::new(BrokenMcp));
        // The agent still has its native tools…
        assert!(registry.list().iter().any(|s| s.name == TOOL_READ));
        // …and the failure is reported, not swallowed silently (FR-MCP-05).
        assert_eq!(registry.warnings().len(), 1);
        assert!(registry.warnings()[0].contains("broken"));
    }

    #[test]
    fn unknown_tool_is_a_tool_error_not_a_run_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = registry_with_native(dir.path().to_path_buf());
        let res = registry.call("nonexistent", "{}").unwrap();
        assert!(res.error.unwrap().contains("unknown tool"));
    }

    #[test]
    fn is_native_distinguishes_backends() {
        let dir = tempfile::tempdir().unwrap();
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = registry_with_native(dir.path().to_path_buf())
            .with_mcp("srv", Box::new(FakeMcp { calls }))
            .with_lsp(Box::new(FakeLsp {
                opened: OpenedDocs::default(),
            }));
        assert!(registry.is_native("read"));
        assert!(registry.is_native(TOOL_WRITE));
        assert!(!registry.is_native("mcp__srv__search"));
        assert!(!registry.is_native(LSP_HOVER));
    }

    #[test]
    fn lsp_tools_are_listed_and_dispatched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}").unwrap();
        let mut registry =
            registry_with_native(dir.path().to_path_buf()).with_lsp(Box::new(FakeLsp {
                opened: OpenedDocs::default(),
            }));

        let names: Vec<String> = registry.list().iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&LSP_HOVER.to_string()));

        let hover = registry
            .call(LSP_HOVER, r#"{"path":"a.rs","line":0,"character":3}"#)
            .unwrap();
        assert_eq!(hover.content, "fn foo()");

        let def = registry
            .call(
                "lsp::goto_definition",
                r#"{"path":"a.rs","line":0,"character":3}"#,
            )
            .unwrap();
        // Positions are reported 1-based for humans/LLMs.
        assert!(def.content.ends_with(":42:4"), "{def:?}");
    }

    #[test]
    fn lsp_tools_absent_when_no_server() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = registry_with_native(dir.path().to_path_buf());
        assert!(!registry.list().iter().any(|s| s.name == LSP_HOVER));
        let res = registry.call(LSP_HOVER, r#"{"path":"a.rs"}"#).unwrap();
        assert!(res.error.unwrap().contains("no language server"));
    }

    #[test]
    fn rename_returns_advice_not_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "foo").unwrap();
        let mut registry =
            registry_with_native(dir.path().to_path_buf()).with_lsp(Box::new(FakeLsp {
                opened: OpenedDocs::default(),
            }));
        let res = registry
            .call(
                LSP_RENAME_SYMBOL,
                r#"{"path":"a.rs","line":0,"character":0,"new_name":"bar"}"#,
            )
            .unwrap();
        assert!(res.content.contains("apply with str_replace_editor"));
        // The file itself is untouched — FS tools remain the only write path.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "foo"
        );
    }

    #[test]
    fn writes_are_pushed_to_the_language_server() {
        let dir = tempfile::tempdir().unwrap();
        let opened = OpenedDocs::default();
        let mut registry = ToolRegistry::new(dir.path().to_path_buf())
            .with_native(Box::new(WriteTool::new(dir.path().to_path_buf())))
            .with_native(Box::new(StrReplaceTool::new(dir.path().to_path_buf())))
            .with_lsp(Box::new(FakeLsp {
                opened: opened.clone(),
            }));

        registry
            .call(TOOL_WRITE, r#"{"path":"a.rs","content":"fn main() {}"}"#)
            .unwrap();
        // FR-LSP-04: the server is told about the file we just wrote.
        let docs = opened.0.lock().unwrap().clone();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].0.ends_with("a.rs"));
        assert_eq!(docs[0].1, "fn main() {}");
        drop(docs);

        registry
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"str_replace","path":"a.rs","old_str":"main","new_str":"start"}"#,
            )
            .unwrap();
        let docs = opened.0.lock().unwrap();
        assert_eq!(docs.len(), 2, "str_replace must also sync");
        assert_eq!(docs[1].1, "fn start() {}");
    }

    #[test]
    fn failed_edits_do_not_sync_the_language_server() {
        let dir = tempfile::tempdir().unwrap();
        let opened = OpenedDocs::default();
        let mut registry = ToolRegistry::new(dir.path().to_path_buf())
            .with_native(Box::new(StrReplaceTool::new(dir.path().to_path_buf())))
            .with_lsp(Box::new(FakeLsp {
                opened: opened.clone(),
            }));
        let res = registry
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"str_replace","path":"missing.rs","old_str":"a","new_str":"b"}"#,
            )
            .unwrap();
        assert!(res.error.is_some());
        assert!(opened.0.lock().unwrap().is_empty());
    }

    #[test]
    fn from_config_builds_the_native_tool_set() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = infra_config::Config {
            working_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let registry = ToolRegistry::from_config(&cfg).expect("registry");
        let names: Vec<String> = registry.list().iter().map(|s| s.name.clone()).collect();
        for expected in [
            TOOL_READ,
            TOOL_WRITE,
            TOOL_STR_REPLACE,
            TOOL_APPLY_PATCH,
            TOOL_LIST_DIR,
            TOOL_SHELL,
            TOOL_SKILL,
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
    }

    #[test]
    fn from_config_rejects_an_invalid_allowlist_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = infra_config::Config {
            working_dir: dir.path().to_path_buf(),
            shell_allowed: Box::new(["(unclosed".to_string()]),
            ..Default::default()
        };
        assert!(matches!(
            ToolRegistry::from_config(&cfg),
            Err(ShellToolError::BadPattern { .. })
        ));
    }
}
