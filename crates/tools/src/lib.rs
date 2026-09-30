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

pub mod edit;
pub mod edit_symbol;
pub mod guard;
pub mod index_tools;
pub mod lsp_tools;
pub mod native;
pub mod patch;
pub mod render;
pub mod rtk;
pub mod search_tools;
pub mod skills;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use domain::{
    canonical_tool_name, BoxError, LspPort, McpPort, Tool, ToolRegistryPort, ToolResult, ToolSpec,
};

pub use edit_symbol::{EditSymbolTool, TOOL_EDIT_SYMBOL};
pub use guard::{allowlist_is_unrestricted, builtin_deny_rule_count, GuardedShell, ShellToolError};
pub use index_tools::{
    IndexSlot, OutlineTool, RelatedTool, SymbolsTool, TOOL_OUTLINE, TOOL_RELATED, TOOL_SYMBOLS,
};
pub use native::{
    ApplyPatchTool, ReadTool, ShellTool, SkillTool, StrReplaceTool, WriteTool, TOOL_APPLY_PATCH,
    TOOL_LIST_DIR, TOOL_READ, TOOL_SHELL, TOOL_SKILL, TOOL_STR_REPLACE, TOOL_WRITE,
};
pub use patch::{apply_patch, parse_unified_diff, PatchError};
pub use rtk::Rtk;
pub use search_tools::{
    default_search, list_tree, GlobTool, GrepTool, ListDirTool, Search, SharedCancel, TOOL_GLOB,
    TOOL_GREP,
};
pub use skills::{SkillEntry, SkillIndex};

/// Find rtk, installing it first if that is allowed and it is missing.
///
/// Every failure here is a `None` and a log line: rtk makes output smaller,
/// and a machine that cannot have it must run exactly as it did before.
fn resolve_rtk(cfg: &infra_config::RtkConfig, notes: &mut Vec<String>) -> Option<rtk::Rtk> {
    if !cfg.enabled {
        return None;
    }
    if let Some(found) = rtk::Rtk::detect(cfg.path.as_deref()) {
        // Nothing is said on the happy path. A line on every launch about an
        // optimisation that always works is noise; `zcode config` is where you
        // look to confirm it.
        log::info!(
            "rtk {} active — shell output is token-optimised ({})",
            found.version(),
            found.path().display()
        );
        return Some(found);
    }
    if cfg.path.is_some() {
        notes.push("rtk.path does not point at a working rtk; continuing without it".to_string());
        return None;
    }
    if !cfg.auto_install {
        log::debug!(
            "rtk not found and rtk.auto_install is off; {}",
            rtk::MANUAL_INSTALL_HINT
        );
        return None;
    }
    // Said *before* the package manager runs, not after. `brew install` can
    // take a minute, and a first run that stalls with no explanation reads as
    // a hang rather than as work.
    if rtk::install_will_be_attempted() {
        log::warn!("rtk is not installed — installing it now to cut shell output; this runs once");
    }
    match rtk::install() {
        Ok(installed) => {
            notes.push(format!(
                "installed rtk {} — shell output is now token-optimised",
                installed.version()
            ));
            Some(installed)
        }
        Err(e) => {
            notes.push(format!(
                "could not install rtk automatically ({e}); continuing without it"
            ));
            None
        }
    }
}

pub const LSP_GOTO_DEFINITION: &str = "lsp__goto_definition";
pub const LSP_FIND_REFERENCES: &str = "lsp__find_references";
pub const LSP_HOVER: &str = "lsp__hover";
pub use lsp_tools::LSP_DIAGNOSTICS;
pub const LSP_RENAME_SYMBOL: &str = "lsp__rename_symbol";

const MCP_PREFIX: &str = "mcp__";
/// Longest an edit's result waits for the server's verdict on it
/// (FR-LSP-08): a slow server delays the edit by at most this.
const EDIT_DIAGNOSTICS_CAP: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// The engine's cancel flag, shared with tools that run long (a search
    /// walk) so Ctrl-C interrupts them too.
    cancel: SharedCancel,
    /// The discovery service every search/listing tool shares (CE-DQ2).
    search: Option<Search>,
    /// Every file a native tool wrote during the current call, drained after
    /// it to keep the language server (and index) in sync (FR-EDIT-09).
    write_log: edit::WriteLog,
    /// URIs the language server has been told about.
    opened: std::collections::HashSet<String>,
    /// The code index, once started: told about every write so its spans
    /// stay exact (FR-INDEX-04), and read by the index tools at call time.
    index_slot: IndexSlot,
    /// Report errors an edit introduced, when a running server covers the
    /// file (`lsp.diagnostics_on_edit`, FR-LSP-08).
    diagnostics_on_edit: bool,
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
            cancel: SharedCancel::default(),
            search: None,
            write_log: edit::WriteLog::default(),
            opened: std::collections::HashSet::new(),
            index_slot: IndexSlot::default(),
            diagnostics_on_edit: false,
        }
    }

    /// Append the errors each edit introduced (FR-LSP-08).
    pub fn with_diagnostics_on_edit(mut self, on: bool) -> Self {
        self.diagnostics_on_edit = on;
        self
    }

    /// Attach the code index.
    pub fn with_code_index(self, index: Arc<dyn domain::CodeIndexPort>) -> Self {
        if self.index_slot.set(index).is_err() {
            log::warn!("code index attached twice; keeping the first");
        }
        self
    }

    /// The attached code index, if any.
    pub fn code_index(&self) -> Option<&Arc<dyn domain::CodeIndexPort>> {
        self.index_slot.get()
    }

    /// The slot the index tools read; filled by [`Self::with_code_index`].
    pub fn index_slot(&self) -> IndexSlot {
        self.index_slot.clone()
    }

    /// The log write tools record into; pass it to each one registered.
    pub fn write_log(&self) -> edit::WriteLog {
        self.write_log.clone()
    }

    /// Sync one written file everywhere that keeps a view of the tree, and
    /// return the errors the write introduced when a running language
    /// server covers the file and `diagnostics_on_edit` is on (FR-LSP-08).
    fn after_write(&mut self, path: &Path, text: &str) -> Vec<domain::LspDiagnostic> {
        if let Some(index) = self.index_slot.get() {
            index.notify_changed(&path.to_string_lossy());
        }
        let watch = self.diagnostics_on_edit;
        let Some(lsp) = self.lsp.as_mut() else {
            return Vec::new();
        };
        let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let uri = file_uri(&absolute);
        // Never start a server for this: only one already running is asked,
        // and its cached report is the baseline — no wait for it.
        let watch = watch && lsp.port.serves(&uri);
        let before = if watch {
            lsp.port.stored_diagnostics(&uri)
        } else {
            Box::new([])
        };
        if lsp.port.open_document(&uri, text).is_ok() {
            self.opened.insert(uri.clone());
        } else {
            return Vec::new();
        }
        if !watch {
            return Vec::new();
        }
        match lsp.port.diagnostics_within(&uri, EDIT_DIAGNOSTICS_CAP) {
            Ok(after) => lsp_tools::new_errors(&before, &after)
                .into_iter()
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Sync every file the last call wrote; the errors they introduced.
    fn sync_writes(&mut self) -> Vec<domain::LspDiagnostic> {
        let written = match self.write_log.lock() {
            Ok(mut log) => std::mem::take(&mut *log),
            Err(_) => Vec::new(),
        };
        let mut errors = Vec::new();
        for (path, text) in written {
            errors.extend(self.after_write(&path, &text));
        }
        errors
    }

    /// The cancel flag tools read at call time.
    pub fn shared_cancel(&self) -> SharedCancel {
        self.cancel.clone()
    }

    /// Register the discovery tools — `list_dir`, `glob`, `grep` — on one
    /// shared search service.
    pub fn with_search(mut self, search: Search, max_file_bytes: u64, timeout_ms: u64) -> Self {
        let root = self.root.clone();
        let cancel = self.cancel.clone();
        self.search = Some(search.clone());
        self.with_native(Box::new(ListDirTool::new(root.clone(), search.clone())))
            .with_native(Box::new(GlobTool::new(root.clone(), search.clone())))
            .with_native(Box::new(
                GrepTool::new(root, search, cancel).with_limits(max_file_bytes, timeout_ms),
            ))
    }

    /// The shared search service, once `with_search` has run.
    pub fn search(&self) -> Option<&Search> {
        self.search.as_ref()
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
                    .collect::<Vec<_>>();
                // FR-CACHE-02: tool schemas render first in every request, so
                // their order is part of the cached prefix. A server is free to
                // list its tools in any order on each start; sort them.
                let mut tools = tools;
                tools.sort_by(|a, b| a.0.cmp(&b.0));
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
        // Collected rather than logged directly so they reach the TUI through
        // the same channel as an MCP server that would not start.
        let mut rtk_notes: Vec<String> = Vec::new();
        let shell = GuardedShell::with_denylist(
            infra_shell::StdShell::new(),
            &cfg.shell_allowed,
            &cfg.shell_denied,
        )?
        .with_rtk(resolve_rtk(&cfg.rtk, &mut rtk_notes));

        let search: Search = Arc::new(
            infra_search::RipgrepSearch::new(
                &root,
                &infra_search::FilterConfig {
                    exclude: cfg.context.exclude.clone(),
                    include: cfg.context.include.clone(),
                },
            )
            .map_err(|e| ShellToolError::BadPattern {
                pattern: "context.exclude / context.include".into(),
                reason: e.to_string(),
            })?,
        );

        // Registration order is wire order, and wire order is part of the
        // cached prompt prefix (FR-CACHE-02) — keep it fixed (technical plan
        // §8): discover/inspect tools first, then the ones that change things.
        let registry =
            Self::new(root.clone()).with_diagnostics_on_edit(cfg.lsp_tuning.diagnostics_on_edit);
        let log = registry.write_log();
        let slot = registry.index_slot();
        #[allow(unused_mut)]
        let mut registry = registry
            .with_native(Box::new(
                ReadTool::new(root.clone())
                    .with_default_limit(cfg.read.default_limit)
                    .with_index(slot.clone()),
            ))
            .with_search(
                search.clone(),
                cfg.search.max_file_bytes,
                cfg.search.timeout_ms,
            )
            // FR-INDEX-05..07: registered whether or not an index will run —
            // without one they answer from a labelled regex fallback.
            .with_native(Box::new(OutlineTool::new(root.clone(), slot.clone())))
            .with_native(Box::new(SymbolsTool::new(
                root.clone(),
                slot.clone(),
                search.clone(),
            )))
            .with_native(Box::new(RelatedTool::new(
                root.clone(),
                slot,
                search.clone(),
            )))
            .with_native(Box::new(
                WriteTool::new(root.clone()).with_write_log(log.clone()),
            ))
            .with_native(Box::new(
                StrReplaceTool::new(root.clone())
                    .with_search(search)
                    .with_write_log(log.clone()),
            ))
            .with_native(Box::new(
                ApplyPatchTool::new(root.clone()).with_write_log(log.clone()),
            ))
            .with_native(Box::new(ShellTool::new(
                root.clone(),
                shell,
                cfg.timeout_ms,
            )));

        // FR-EDIT-01: only offered when an index will run — without one it
        // could only ever fail. Registered after the shell tool so turning
        // the index off drops a tool from the end rather than the middle of
        // the cached tool list.
        if cfg.index.enabled {
            let slot = registry.index_slot();
            registry = registry.with_native(Box::new(
                EditSymbolTool::new(root.clone(), slot)
                    .with_write_log(log)
                    .with_syntax_check(cfg.edit.syntax_check),
            ));
        }

        for note in rtk_notes {
            registry.warn(note);
        }

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

        // FR-LSP-10, CE-DQ20: one server per language, each started the
        // first time a request names a file of its language, capped at
        // `lsp.max_servers` and stopped after `lsp.idle_shutdown_s`. Nothing
        // starts here, so a session that never asks the LSP pays nothing.
        #[cfg(feature = "lsp")]
        {
            let specs: Vec<infra_lsp::ServerSpec> = cfg
                .effective_lsp_servers()
                .into_iter()
                .map(|s| infra_lsp::ServerSpec {
                    languages: vec![infra_config::canonical_language(&s.language)],
                    command: s.command,
                    args: s.args,
                    env: s.env,
                })
                .collect();
            if !specs.is_empty() {
                registry = registry.with_lsp(Box::new(infra_lsp::LspPool::new(
                    &root,
                    specs,
                    cfg.lsp_tuning.max_servers as usize,
                    std::time::Duration::from_secs(cfg.lsp_tuning.idle_shutdown_s),
                    cfg.timeout_ms,
                )));
            }
        }

        Ok(registry)
    }

    fn native_index(&self, canonical: &str) -> Option<usize> {
        self.native.iter().position(|e| e.name == canonical)
    }

    /// `file://` URI for a model-supplied path.
    fn uri_for_path(root: &Path, path: &str) -> String {
        let resolved = native::resolve(root, path);
        file_uri(&resolved.canonicalize().unwrap_or(resolved))
    }

    /// Tell the server about a file the first time the agent touches it, so
    /// references and diagnostics cover it (FR-LSP-04/09).
    fn ensure_open(&mut self, uri: &str) {
        if self.opened.contains(uri) {
            return;
        }
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        if let Ok(text) = std::fs::read_to_string(lsp_tools::uri_path(uri)) {
            if lsp.port.open_document(uri, &text).is_ok() {
                self.opened.insert(uri.to_string());
            }
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
        let root = self.root.clone();

        if canonical == LSP_DIAGNOSTICS {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let severity = match args
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("error")
            {
                "error" => 1,
                "warning" => 2,
                "all" => 4,
                other => {
                    return Ok(native::tool_error(format!(
                        "unknown severity `{other}`; expected error, warning or all"
                    )))
                }
            };
            let uri = path.as_deref().map(|p| Self::uri_for_path(&root, p));
            if let Some(u) = &uri {
                self.ensure_open(u);
            }
            let scope = path
                .clone()
                .unwrap_or_else(|| "the files opened this session".into());
            let lsp = self.lsp.as_mut().ok_or("no language server")?;
            return Ok(match lsp.port.diagnostics(uri.as_deref()) {
                Ok(found) => ToolResult::ok(&lsp_tools::render_diagnostics(
                    &root, &found, severity, &scope,
                ))
                .with_subject(domain::Subject::Diagnostics { path }),
                Err(e) => native::tool_error(e.to_string()),
            });
        }

        let target = match lsp_tools::parse_target(&args) {
            Ok(t) => t,
            Err(e) => return Ok(native::tool_error(e)),
        };
        if let lsp_tools::Target::Position { path, .. } = &target {
            let uri = Self::uri_for_path(&root, path);
            self.ensure_open(&uri);
        }
        // FR-LSP-11: a server still indexing would sit on the request until
        // the timeout — and so would the `workspace/symbol` lookup that
        // resolves a named symbol. The index answers instead, labelled;
        // without one the model is told to come back rather than left
        // waiting. Checked before anything is sent.
        if matches!(
            canonical,
            LSP_GOTO_DEFINITION | LSP_FIND_REFERENCES | LSP_HOVER
        ) {
            let (named_uri, name) = match &target {
                lsp_tools::Target::Symbol { symbol, path } => (
                    path.as_deref().map(|p| Self::uri_for_path(&root, p)),
                    Some(symbol.clone()),
                ),
                lsp_tools::Target::Position { path, line, column } => {
                    let uri = Self::uri_for_path(&root, path);
                    let file = lsp_tools::uri_path(&uri);
                    let text = std::fs::read_to_string(&file).unwrap_or_default();
                    let line_text = text.lines().nth((line - 1) as usize).unwrap_or("");
                    let col16 = lsp_tools::char_col_to_utf16(line_text, *column);
                    let word = lsp_tools::word_at(&file, line - 1, col16);
                    (Some(uri), word)
                }
            };
            let readiness =
                self.lsp
                    .as_ref()
                    .map_or(domain::LspReadiness::Ready, |l| match &named_uri {
                        Some(u) => l.port.readiness_for(u),
                        None => l.port.readiness(),
                    });
            if let domain::LspReadiness::Indexing(pct) = readiness {
                let progress = pct.map_or(String::new(), |p| format!(": {p}%"));
                let answer = match (self.index_slot.get(), &name) {
                    (Some(ix), Some(n)) => {
                        lsp_tools::index_answer(ix.as_ref(), &root, canonical, n)
                    }
                    _ => None,
                };
                return Ok(match answer {
                    Some(text) => ToolResult::ok(&format!(
                        "(from code index — language server still indexing{progress})\n{text}"
                    )),
                    None => native::tool_error(format!(
                        "language server is still indexing{progress}; try grep or symbols, or \
                         retry shortly"
                    )),
                });
            }
        }

        let uri_for = |p: &str| Self::uri_for_path(&root, p);
        let resolved = {
            let index = self.index_slot.get().cloned();
            let lsp = self.lsp.as_mut().ok_or("no language server")?;
            lsp_tools::resolve(
                lsp.port.as_mut(),
                &root,
                &target,
                &uri_for,
                index.as_deref(),
            )
        };
        let (uri, line, column) = match resolved {
            Ok(r) => r,
            Err(e) => return Ok(native::tool_error(e)),
        };
        self.ensure_open(&uri);

        let lsp = self.lsp.as_mut().ok_or("no language server")?;
        let result = match canonical {
            LSP_GOTO_DEFINITION => match lsp.port.goto_definition(&uri, line, column) {
                Ok(loc) => ToolResult::ok(&lsp_tools::render_definition(&root, &loc)),
                Err(e) => native::tool_error(e.to_string()),
            },
            LSP_FIND_REFERENCES => match lsp.port.find_references(&uri, line, column) {
                Ok(locs) => {
                    ToolResult::ok(&lsp_tools::render_locations(&root, &locs, "references"))
                }
                Err(e) => native::tool_error(e.to_string()),
            },
            LSP_HOVER => match lsp.port.hover(&uri, line, column) {
                Ok(text) => ToolResult::ok(&lsp_tools::clean_hover(&text)),
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
                let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
                let edit = match lsp.port.rename_symbol(&uri, line, column, new_name) {
                    Ok(edit) => edit,
                    Err(e) => return Ok(native::tool_error(e.to_string())),
                };
                if !apply || edit.changes.is_empty() {
                    ToolResult::ok(&lsp_tools::render_rename(&root, &edit))
                } else {
                    // FR-LSP-12: staged, then swapped in; synced like any write.
                    match lsp_tools::apply_workspace_edit(&edit) {
                        Ok(applied) => {
                            let mut errors = Vec::new();
                            for (path, text, _) in &applied.files {
                                errors.extend(self.after_write(path, text));
                            }
                            let mut out =
                                lsp_tools::render_applied_rename(&root, new_name, &applied);
                            if !errors.is_empty() {
                                let refs: Vec<&domain::LspDiagnostic> = errors.iter().collect();
                                out.push('\n');
                                out.push_str(&lsp_tools::render_new_errors(&root, &refs));
                            }
                            let paths = applied
                                .files
                                .iter()
                                .map(|(p, _, _)| lsp_tools::shown(&root, p))
                                .collect();
                            ToolResult::ok(&out).with_subject(domain::Subject::FileWrites { paths })
                        }
                        Err(e) => native::tool_error(e),
                    }
                }
            }
            other => native::tool_error(format!("unknown lsp tool `{other}`")),
        };
        Ok(result)
    }
}

impl ToolRegistryPort for ToolRegistry {
    fn list(&self) -> Box<[ToolSpec]> {
        let lsp_count = if self.lsp.is_some() { 5 } else { 0 };
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

    /// FR-CTX-06: replace the file content an old edit call carried with a
    /// line count, keeping the arguments valid JSON (some providers
    /// re-validate `tool_use` input) and every short field — the path above
    /// all — as it was.
    fn elide_args(&self, name: &str, args_json: &str) -> Option<String> {
        const CONTENT_KEYS: &[&str] = &[
            "content",
            "file_text",
            "patch",
            "diff",
            "new_str",
            "old_str",
            "body",
        ];
        let canonical = canonical_tool_name(name);
        if !domain::context::EDIT_TOOLS.contains(&canonical.as_str()) {
            return None;
        }
        let mut value: serde_json::Value = serde_json::from_str(args_json).ok()?;
        let object = value.as_object_mut()?;
        let mut changed = false;
        for key in CONTENT_KEYS {
            if let Some(serde_json::Value::String(text)) = object.get(*key) {
                if text.len() > 200 {
                    let lines = text.lines().count();
                    object.insert(
                        (*key).to_string(),
                        serde_json::Value::String(format!(
                            "[{lines} lines elided by compaction — already applied]"
                        )),
                    );
                    changed = true;
                }
            }
        }
        changed.then(|| value.to_string())
    }

    /// FR-SEARCH-09: a shell command whose program is a search tool —
    /// after `cd … &&` and `env VAR=…` prefixes — is `shell_search`.
    fn classify_call(&self, name: &str, args_json: &str) -> Option<&'static str> {
        if canonical_tool_name(name) != TOOL_SHELL {
            return None;
        }
        let args: serde_json::Value = serde_json::from_str(args_json).ok()?;
        let command = args.get("command")?.as_str()?;
        shell_program(command)
            .is_some_and(|p| {
                matches!(
                    p,
                    "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "find" | "fd"
                )
            })
            .then_some("shell_search")
    }

    fn set_cancel(&mut self, cancel: domain::CancelFlag) {
        if let Ok(mut slot) = self.cancel.lock() {
            *slot = Some(cancel);
        }
    }

    fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let canonical = canonical_tool_name(name);

        if let Some(index) = self.native_index(&canonical) {
            let mut result = self.native[index].tool.call(&canonical, args_json)?;
            // FR-EDIT-09 / FR-LSP-09: every file any write path touched —
            // each file of an `apply_patch` included — is synced.
            let errors = self.sync_writes();
            if !errors.is_empty() && result.error.is_none() {
                let refs: Vec<&domain::LspDiagnostic> = errors.iter().collect();
                result.content.push('\n');
                result
                    .content
                    .push_str(&lsp_tools::render_new_errors(&self.root, &refs));
            }
            // FR-LSP-04: a file the agent reads is opened on the server, so
            // references and diagnostics cover it.
            if canonical == TOOL_READ && self.lsp.is_some() {
                if let Some(domain::Subject::FileRange { path, .. }) = &result.subject {
                    let uri = Self::uri_for_path(&self.root.clone(), path);
                    self.ensure_open(&uri);
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
    // FR-LSP-05: name the symbol, or give a 1-based position.
    let target = r#""path":{"type":"string"},"symbol":{"type":"string","description":"e.g. \"AgentLoop::execute\" — instead of line/column"},"line":{"type":"integer","description":"1-based"},"column":{"type":"integer","description":"1-based"}"#;
    let position_schema = format!(r#"{{"type":"object","properties":{{{target}}}}}"#);
    vec![
        ToolSpec {
            name: LSP_GOTO_DEFINITION.into(),
            description: "Where a symbol is defined. Address it by `symbol` name, or by path + \
                          line + column (1-based)."
                .into(),
            params_json: position_schema.clone(),
        },
        ToolSpec {
            name: LSP_FIND_REFERENCES.into(),
            description: "Every reference to a symbol, grouped by file.".into(),
            params_json: position_schema.clone(),
        },
        ToolSpec {
            name: LSP_HOVER.into(),
            description: "Type and documentation of a symbol.".into(),
            params_json: position_schema,
        },
        ToolSpec {
            name: LSP_RENAME_SYMBOL.into(),
            description: "Rename a symbol across files. By default lists the edits it would \
                          make; with apply: true it makes them, all files or none."
                .into(),
            params_json: format!(
                r#"{{"type":"object","properties":{{{target},"new_name":{{"type":"string"}},"apply":{{"type":"boolean","description":"Make the edits instead of listing them"}}}},"required":["new_name"]}}"#
            ),
        },
        ToolSpec {
            name: LSP_DIAGNOSTICS.into(),
            description: "Compiler errors (and warnings with severity) from the language server \
                          — cheaper than a full build after an edit."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"One file; default: every file touched this session"},"severity":{"type":"string","enum":["error","warning","all"]}}}"#.into(),
        },
    ]
}

/// The program a shell command runs, past `cd … &&` and `env VAR=…`
/// prefixes.
fn shell_program(command: &str) -> Option<&str> {
    let mut rest = command.trim();
    while let Some(idx) = rest.find("&&") {
        if rest[..idx].trim_start().starts_with("cd ") {
            rest = rest[idx + 2..].trim_start();
        } else {
            break;
        }
    }
    rest.split_whitespace()
        .find(|w| *w != "env" && !w.contains('='))
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
    use domain::{LspLocation, LspPosition, LspRange, LspTextEdit, LspWorkspaceEdit, McpToolDef};

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

    /// A server that lists its tools in whatever order it likes.
    struct ShuffledMcp(Vec<&'static str>);
    impl McpPort for ShuffledMcp {
        fn list_tools(&mut self) -> Result<Box<[McpToolDef]>, BoxError> {
            Ok(self
                .0
                .iter()
                .map(|n| McpToolDef {
                    name: (*n).into(),
                    description: String::new(),
                    input_schema: "{}".into(),
                })
                .collect())
        }
        fn call(&mut self, _name: &str, _args: String) -> Result<String, BoxError> {
            Ok(String::new())
        }
        fn ping(&mut self) -> Result<bool, BoxError> {
            Ok(true)
        }
    }

    /// FR-CACHE-02: tool specs head every request; a server listing its tools
    /// in a different order on each start must not change those bytes.
    #[test]
    fn mcp_tools_are_listed_in_sorted_order() {
        let names = |order: Vec<&'static str>| -> Vec<String> {
            ToolRegistry::new(std::path::PathBuf::from("."))
                .with_mcp("srv", Box::new(ShuffledMcp(order)))
                .list()
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        let a = names(vec!["zeta", "alpha", "mid"]);
        let b = names(vec!["mid", "zeta", "alpha"]);
        assert_eq!(a, b);
        assert_eq!(a, ["mcp__srv__alpha", "mcp__srv__mid", "mcp__srv__zeta"]);
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
        assert_eq!(native.content, "1│body");

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

        // FR-LSP-05: positions are 1-based, like every other tool.
        let hover = registry
            .call(LSP_HOVER, r#"{"path":"a.rs","line":1,"column":4}"#)
            .unwrap();
        assert_eq!(hover.content, "fn foo()", "{hover:?}");

        let def = registry
            .call(
                "lsp::goto_definition",
                r#"{"path":"a.rs","line":1,"column":4}"#,
            )
            .unwrap();
        // Reported 1-based, as a path rather than a file:// URI.
        assert!(def.content.contains(":42:4"), "{def:?}");
        assert!(!def.content.contains("file://"), "{def:?}");
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
                r#"{"path":"a.rs","line":1,"column":1,"new_name":"bar"}"#,
            )
            .unwrap();
        assert!(
            res.content.contains("apply with str_replace_editor"),
            "{res:?}"
        );
        // The file itself is untouched — FS tools remain the only write path.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "foo"
        );
    }

    /// Records what the registry tells the index.
    #[derive(Default)]
    struct FakeIndex(std::sync::Mutex<Vec<String>>);

    impl domain::CodeIndexPort for FakeIndex {
        fn state(&self) -> domain::IndexState {
            domain::IndexState::Ready
        }
        fn outline(&self, _: &str) -> Result<Option<Vec<domain::SymbolDef>>, BoxError> {
            Ok(None)
        }
        fn symbols(
            &self,
            _: &str,
            _: Option<domain::SymbolKind>,
            _: Option<&str>,
            _: usize,
        ) -> Result<Vec<domain::SymbolDef>, BoxError> {
            Ok(Vec::new())
        }
        fn locate(&self, _: Option<&str>, _: &str) -> Result<Vec<domain::SymbolDef>, BoxError> {
            Ok(Vec::new())
        }
        fn related(&self, _: &str) -> Result<domain::Related, BoxError> {
            Ok(domain::Related::default())
        }
        fn parse_text(&self, _: &str, _: &str) -> Result<Option<domain::ParsedFile>, BoxError> {
            Ok(None)
        }
        fn repo_map(&self, _: &str, _: u32) -> Result<String, BoxError> {
            Ok(String::new())
        }
        fn notify_changed(&self, path: &str) {
            self.0.lock().unwrap().push(path.to_string());
        }
    }

    /// A scripted server for the pool-era behaviours.
    #[derive(Default)]
    struct ScriptLsp {
        edit: Option<LspWorkspaceEdit>,
        indexing: bool,
        serves: bool,
        before: Vec<domain::LspDiagnostic>,
        after: Vec<domain::LspDiagnostic>,
        waited: std::sync::Arc<std::sync::Mutex<u32>>,
    }

    impl LspPort for ScriptLsp {
        fn goto_definition(&mut self, _: &str, _: u32, _: u32) -> Result<LspLocation, BoxError> {
            Err("the server was asked".into())
        }
        fn find_references(
            &mut self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<Box<[LspLocation]>, BoxError> {
            Err("the server was asked".into())
        }
        fn hover(&mut self, _: &str, _: u32, _: u32) -> Result<String, BoxError> {
            Err("the server was asked".into())
        }
        fn rename_symbol(
            &mut self,
            _: &str,
            _: u32,
            _: u32,
            _: &str,
        ) -> Result<LspWorkspaceEdit, BoxError> {
            self.edit.clone().ok_or_else(|| "no edit".into())
        }
        fn open_document(&mut self, _: &str, _: &str) -> Result<(), BoxError> {
            Ok(())
        }
        fn stored_diagnostics(&self, _: &str) -> Box<[domain::LspDiagnostic]> {
            self.before.clone().into_boxed_slice()
        }
        fn diagnostics_within(
            &mut self,
            _: &str,
            _: std::time::Duration,
        ) -> Result<Box<[domain::LspDiagnostic]>, BoxError> {
            *self.waited.lock().unwrap() += 1;
            Ok(self.after.clone().into_boxed_slice())
        }
        fn readiness(&self) -> domain::LspReadiness {
            self.readiness_for("")
        }
        fn readiness_for(&self, _: &str) -> domain::LspReadiness {
            if self.indexing {
                domain::LspReadiness::Indexing(Some(43))
            } else {
                domain::LspReadiness::Ready
            }
        }
        fn serves(&self, _: &str) -> bool {
            self.serves
        }
    }

    fn range(line: u32, from: u32, to: u32) -> LspRange {
        LspRange {
            start: LspPosition {
                line,
                character: from,
            },
            end: LspPosition {
                line,
                character: to,
            },
        }
    }

    fn error_at(uri: &str, line: u32, message: &str) -> domain::LspDiagnostic {
        domain::LspDiagnostic {
            uri: uri.to_string(),
            range: range(line, 0, 1),
            severity: 1,
            code: Some("E0425".into()),
            message: message.into(),
        }
    }

    #[test]
    fn rename_apply_rewrites_every_file_with_utf16_columns() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        // `𝒳` is two UTF-16 units (four bytes): `foo` starts at unit 14.
        std::fs::write(&a, "let s = \"𝒳\"; foo();\nfoo();\n").unwrap();
        std::fs::write(&b, "fn foo() {}\n").unwrap();
        let ua = file_uri(&a.canonicalize().unwrap());
        let ub = file_uri(&b.canonicalize().unwrap());
        let edit = LspWorkspaceEdit {
            changes: Box::new([
                LspTextEdit {
                    uri: ua.clone(),
                    range: range(0, 14, 17),
                    new_text: "bar".into(),
                },
                LspTextEdit {
                    uri: ua.clone(),
                    range: range(1, 0, 3),
                    new_text: "bar".into(),
                },
                LspTextEdit {
                    uri: ub.clone(),
                    range: range(0, 3, 6),
                    new_text: "bar".into(),
                },
            ]),
        };
        let mut registry =
            ToolRegistry::new(dir.path().to_path_buf()).with_lsp(Box::new(ScriptLsp {
                edit: Some(edit),
                ..Default::default()
            }));
        let res = registry
            .call(
                LSP_RENAME_SYMBOL,
                r#"{"path":"b.rs","line":1,"column":4,"new_name":"bar","apply":true}"#,
            )
            .unwrap();
        assert!(res.error.is_none(), "{res:?}");
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "let s = \"𝒳\"; bar();\nbar();\n"
        );
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "fn bar() {}\n");
        assert!(
            res.content
                .starts_with("renamed to bar: 3 occurrences in 2 files"),
            "{}",
            res.content
        );
        assert!(matches!(
            res.subject,
            Some(domain::Subject::FileWrites { .. })
        ));
    }

    #[test]
    fn a_rename_that_cannot_be_staged_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        std::fs::write(&a, "foo();\n").unwrap();
        let ua = file_uri(&a.canonicalize().unwrap());
        let edit = LspWorkspaceEdit {
            changes: Box::new([
                LspTextEdit {
                    uri: ua,
                    range: range(0, 0, 3),
                    new_text: "bar".into(),
                },
                LspTextEdit {
                    uri: file_uri(&dir.path().join("gone.rs")),
                    range: range(0, 0, 3),
                    new_text: "bar".into(),
                },
            ]),
        };
        let mut registry =
            ToolRegistry::new(dir.path().to_path_buf()).with_lsp(Box::new(ScriptLsp {
                edit: Some(edit),
                ..Default::default()
            }));
        let res = registry
            .call(
                LSP_RENAME_SYMBOL,
                r#"{"path":"a.rs","line":1,"column":1,"new_name":"bar","apply":true}"#,
            )
            .unwrap();
        assert!(res
            .error
            .unwrap()
            .starts_with("rename not applied, nothing changed"));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "foo();\n");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".zcode-tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn an_edit_reports_only_the_errors_it_introduced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        let uri = file_uri(&dir.path().join("a.rs").canonicalize().unwrap());
        let waited = std::sync::Arc::new(std::sync::Mutex::new(0));
        let registry = ToolRegistry::new(dir.path().to_path_buf()).with_diagnostics_on_edit(true);
        let log = registry.write_log();
        let mut registry = registry
            .with_native(Box::new(
                WriteTool::new(dir.path().to_path_buf()).with_write_log(log),
            ))
            .with_lsp(Box::new(ScriptLsp {
                serves: true,
                before: vec![error_at(&uri, 5, "old problem")],
                // The old one moved two lines down; one is new.
                after: vec![
                    error_at(&uri, 7, "old problem"),
                    error_at(&uri, 0, "cannot find value `x`"),
                ],
                waited: waited.clone(),
                ..Default::default()
            }));
        let res = registry
            .call(TOOL_WRITE, r#"{"path":"a.rs","content":"fn a() { x }\n"}"#)
            .unwrap();
        assert!(
            res.content
                .ends_with("new errors:\n  a.rs:1:1  error[E0425]  cannot find value `x`"),
            "{}",
            res.content
        );
        assert!(!res.content.contains("old problem"));
        assert_eq!(*waited.lock().unwrap(), 1);
    }

    #[test]
    fn no_running_server_means_no_wait_after_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let waited = std::sync::Arc::new(std::sync::Mutex::new(0));
        let registry = ToolRegistry::new(dir.path().to_path_buf()).with_diagnostics_on_edit(true);
        let log = registry.write_log();
        let mut registry = registry
            .with_native(Box::new(
                WriteTool::new(dir.path().to_path_buf()).with_write_log(log),
            ))
            .with_lsp(Box::new(ScriptLsp {
                serves: false,
                waited: waited.clone(),
                ..Default::default()
            }));
        let res = registry
            .call(TOOL_WRITE, r#"{"path":"a.rs","content":"fn a() {}\n"}"#)
            .unwrap();
        assert!(!res.content.contains("new errors"));
        assert_eq!(*waited.lock().unwrap(), 0);
    }

    #[test]
    fn an_indexing_server_is_answered_for_by_the_index_or_not_waited_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "/// Adds.\npub fn add(a: u32) -> u32 { a }\nfn main() { add(1); }\n",
        )
        .unwrap();
        let mut registry =
            ToolRegistry::new(dir.path().to_path_buf()).with_lsp(Box::new(ScriptLsp {
                indexing: true,
                ..Default::default()
            }));
        // No index: told to come back, not left waiting.
        let res = registry
            .call(LSP_HOVER, r#"{"symbol":"add","path":"a.rs"}"#)
            .unwrap();
        assert_eq!(
            res.error.as_deref(),
            Some("language server is still indexing: 43%; try grep or symbols, or retry shortly")
        );
        // With one: answered, and labelled.
        let search: Arc<dyn domain::SearchPort> = Arc::new(
            infra_search::RipgrepSearch::new(dir.path(), &infra_search::FilterConfig::default())
                .unwrap(),
        );
        let ix = infra_index::CodeIndex::open(infra_index::IndexOptions::new(dir.path()), search);
        ix.build();
        let mut registry = registry.with_code_index(ix);
        let res = registry
            .call(
                LSP_GOTO_DEFINITION,
                r#"{"path":"a.rs","line":3,"column":14}"#,
            )
            .unwrap();
        assert_eq!(
            res.content,
            "(from code index — language server still indexing: 43%)\na.rs:2:8  pub fn add(a: u32) -> u32 { a }"
        );
        let res = registry.call(LSP_HOVER, r#"{"symbol":"add"}"#).unwrap();
        assert!(
            res.content
                .ends_with("a.rs:2\n/// Adds.\npub fn add(a: u32) -> u32 { a }"),
            "{res:?}"
        );
        let res = registry
            .call(LSP_FIND_REFERENCES, r#"{"symbol":"add"}"#)
            .unwrap();
        assert!(
            res.content
                .ends_with("`add` occurs (by name) at:\n  a.rs:3"),
            "{}",
            res.content
        );
    }

    #[test]
    fn writes_are_pushed_to_the_code_index() {
        let dir = tempfile::tempdir().unwrap();
        let index = Arc::new(FakeIndex::default());
        let registry = ToolRegistry::new(dir.path().to_path_buf());
        let log = registry.write_log();
        let mut registry = registry
            .with_native(Box::new(
                WriteTool::new(dir.path().to_path_buf()).with_write_log(log),
            ))
            .with_code_index(index.clone());
        registry
            .call(TOOL_WRITE, r#"{"path":"src/a.rs","content":"fn a() {}"}"#)
            .unwrap();
        let seen = index.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].ends_with("src/a.rs"), "{seen:?}");
    }

    #[test]
    fn writes_are_pushed_to_the_language_server() {
        let dir = tempfile::tempdir().unwrap();
        let opened = OpenedDocs::default();
        let registry = ToolRegistry::new(dir.path().to_path_buf());
        let log = registry.write_log();
        let root = dir.path().to_path_buf();
        let mut registry = registry
            .with_native(Box::new(
                WriteTool::new(root.clone()).with_write_log(log.clone()),
            ))
            .with_native(Box::new(
                StrReplaceTool::new(root.clone()).with_write_log(log.clone()),
            ))
            .with_native(Box::new(ApplyPatchTool::new(root).with_write_log(log)))
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
        let docs = opened.0.lock().unwrap().clone();
        assert_eq!(docs.len(), 2, "str_replace must also sync");
        assert_eq!(docs[1].1, "fn start() {}");

        // FR-LSP-09: v0.6 never synced apply_patch; every patched file is.
        let patch = "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-fn start() {}\n+fn go() {}\n\
                     --- /dev/null\n+++ b/b.rs\n@@ -0,0 +1 @@\n+fn b() {}\n";
        let res = registry
            .call(
                TOOL_APPLY_PATCH,
                &serde_json::json!({ "patch": patch }).to_string(),
            )
            .unwrap();
        assert!(res.error.is_none(), "{res:?}");
        let docs = opened.0.lock().unwrap();
        assert_eq!(docs.len(), 4, "both patched files synced: {docs:?}");
        assert!(docs[2].0.ends_with("a.rs"), "{docs:?}");
        assert_eq!(docs[2].1.trim_end(), "fn go() {}", "{docs:?}");
        assert!(docs[3].0.ends_with("b.rs"));
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

    /// FR-CACHE-02: registration order is wire order, and tool specs head
    /// every request — a reshuffle would silently defeat the prompt cache.
    #[test]
    fn native_tool_order_is_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = infra_config::Config {
            working_dir: dir.path().to_path_buf(),
            lsp_defaults: false,
            ..Default::default()
        };
        let registry = ToolRegistry::from_config(&cfg).expect("registry");
        let names: Vec<String> = registry.list().iter().map(|s| s.name.clone()).collect();
        let natives: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|n| !n.starts_with("lsp__") && !n.starts_with("mcp__") && *n != TOOL_SKILL)
            .collect();
        assert_eq!(
            natives,
            [
                TOOL_READ,
                TOOL_LIST_DIR,
                TOOL_GLOB,
                TOOL_GREP,
                TOOL_OUTLINE,
                TOOL_SYMBOLS,
                TOOL_RELATED,
                TOOL_WRITE,
                TOOL_STR_REPLACE,
                TOOL_APPLY_PATCH,
                TOOL_SHELL,
                TOOL_EDIT_SYMBOL
            ]
        );
    }

    #[test]
    fn a_bad_discovery_glob_in_config_is_a_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = infra_config::Config {
            working_dir: dir.path().to_path_buf(),
            lsp_defaults: false,
            ..Default::default()
        };
        cfg.context.exclude = vec!["src/[".into()];
        let err = ToolRegistry::from_config(&cfg)
            .err()
            .expect("error")
            .to_string();
        assert!(err.contains("context.exclude"), "{err}");
    }

    #[test]
    fn from_config_builds_the_native_tool_set() {
        let dir = tempfile::tempdir().unwrap();
        // `TOOL_SKILL` is registered only when a skill is discoverable, and
        // `Config::skills_dirs` always includes the machine-wide
        // `~/.config/zcode/skills` root alongside the project one — so
        // without a skill planted here, this test passed only on a machine
        // that happens to have one installed there, and failed on every
        // clean CI runner (no such directory exists) despite passing locally.
        let skills_dir = dir.path().join(".zcode").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("example.md"), "an example skill").unwrap();

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

    #[test]
    fn elide_args_keeps_valid_json_and_the_path() {
        let registry = ToolRegistry::new(PathBuf::from("."));
        let big = "fn x() {}\n".repeat(100);
        let args = serde_json::json!({ "path": "src/a.rs", "content": big }).to_string();
        let shrunk = registry.elide_args("write", &args).expect("elided");
        let v: serde_json::Value = serde_json::from_str(&shrunk).unwrap();
        assert_eq!(v["path"], "src/a.rs");
        assert_eq!(
            v["content"],
            "[100 lines elided by compaction — already applied]"
        );
        assert!(
            registry.elide_args("read", &args).is_none(),
            "only edit tools"
        );
        let small = serde_json::json!({ "path": "a", "content": "x" }).to_string();
        assert!(
            registry.elide_args("write", &small).is_none(),
            "nothing worth eliding"
        );
    }

    // ---- FR-LSP-05..07/09: addressing, diagnostics, sync on read -----------

    /// A language server that knows two symbols and records what it is asked.
    #[derive(Clone, Default)]
    struct SymLsp {
        asked: Arc<std::sync::Mutex<Vec<(String, u32, u32)>>>,
        opened: Arc<std::sync::Mutex<Vec<String>>>,
        root: PathBuf,
    }

    impl LspPort for SymLsp {
        fn goto_definition(&mut self, uri: &str, l: u32, c: u32) -> Result<LspLocation, BoxError> {
            self.asked.lock().unwrap().push((uri.into(), l, c));
            Err("unused".into())
        }
        fn find_references(
            &mut self,
            uri: &str,
            l: u32,
            c: u32,
        ) -> Result<Box<[LspLocation]>, BoxError> {
            self.asked.lock().unwrap().push((uri.into(), l, c));
            Ok(Box::new([]))
        }
        fn hover(&mut self, uri: &str, l: u32, c: u32) -> Result<String, BoxError> {
            self.asked.lock().unwrap().push((uri.into(), l, c));
            Ok("```rust\nfn run()\n```".into())
        }
        fn rename_symbol(
            &mut self,
            _u: &str,
            _l: u32,
            _c: u32,
            _n: &str,
        ) -> Result<LspWorkspaceEdit, BoxError> {
            Ok(LspWorkspaceEdit {
                changes: Box::new([]),
            })
        }
        fn open_document(&mut self, uri: &str, _text: &str) -> Result<(), BoxError> {
            self.opened.lock().unwrap().push(uri.into());
            Ok(())
        }
        fn workspace_symbols(
            &mut self,
            query: &str,
        ) -> Result<Box<[domain::LspSymbolInfo]>, BoxError> {
            let at = |file: &str, line: u32, container: &str| domain::LspSymbolInfo {
                name: query.into(),
                kind: 6,
                container: Some(container.into()),
                location: LspLocation {
                    uri: file_uri(&self.root.join(file).canonicalize().unwrap()),
                    range: LspRange {
                        start: LspPosition { line, character: 0 },
                        end: LspPosition {
                            line: line + 2,
                            character: 1,
                        },
                    },
                },
            };
            Ok(match query {
                "run" => Box::new([at("a.rs", 1, "Engine"), at("b.rs", 0, "Other")]),
                "start" => Box::new([at("a.rs", 4, "Engine")]),
                _ => Box::new([]),
            })
        }
        fn diagnostics(
            &mut self,
            uri: Option<&str>,
        ) -> Result<Box<[domain::LspDiagnostic]>, BoxError> {
            let uri = uri.unwrap_or("").to_string();
            Ok(Box::new([domain::LspDiagnostic {
                uri,
                range: LspRange {
                    start: LspPosition {
                        line: 1,
                        character: 4,
                    },
                    end: LspPosition {
                        line: 1,
                        character: 7,
                    },
                },
                severity: 1,
                code: Some("E0425".into()),
                message: "cannot find value `x`".into(),
            }]))
        }
    }

    fn sym_registry() -> (tempfile::TempDir, ToolRegistry, SymLsp) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "impl Engine {\n    pub fn run(&self) {}\n}\n\n    fn start() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn run() {}\n").unwrap();
        let lsp = SymLsp {
            root: dir.path().to_path_buf(),
            ..SymLsp::default()
        };
        let registry =
            registry_with_native(dir.path().to_path_buf()).with_lsp(Box::new(lsp.clone()));
        (dir, registry, lsp)
    }

    #[test]
    fn a_symbol_resolves_to_its_name_not_the_declaration_start() {
        let (_dir, mut registry, lsp) = sym_registry();
        let res = registry.call(LSP_HOVER, r#"{"symbol":"start"}"#).unwrap();
        assert_eq!(res.content, "fn run()", "fences stripped");
        let asked = lsp.asked.lock().unwrap();
        let (uri, line, col) = asked.last().unwrap().clone();
        assert!(uri.ends_with("a.rs"));
        // `    fn start() {}` — the name starts at 0-based column 7.
        assert_eq!((line, col), (4, 7));
    }

    #[test]
    fn an_ambiguous_symbol_lists_candidates_and_a_qualifier_disambiguates() {
        let (_dir, mut registry, lsp) = sym_registry();
        let res = registry
            .call(LSP_FIND_REFERENCES, r#"{"symbol":"run"}"#)
            .unwrap();
        let err = res.error.unwrap();
        assert!(err.contains("`run` is ambiguous (2 matches)"), "{err}");
        assert!(
            err.contains("Engine::run — a.rs:2") && err.contains("Other::run — b.rs:1"),
            "{err}"
        );
        let ok = registry
            .call(LSP_FIND_REFERENCES, r#"{"symbol":"Engine::run"}"#)
            .unwrap();
        assert!(ok.error.is_none(), "{ok:?}");
        let (_, line, col) = lsp.asked.lock().unwrap().last().unwrap().clone();
        assert_eq!((line, col), (1, 11), "`    pub fn run` — name at column 11");
        let missing = registry.call(LSP_HOVER, r#"{"symbol":"nothing"}"#).unwrap();
        assert!(missing
            .error
            .unwrap()
            .starts_with("no symbol named `nothing`"));
    }

    #[test]
    fn the_diagnostics_tool_renders_one_based_and_is_read_only() {
        let (_dir, mut registry, lsp) = sym_registry();
        let res = registry
            .call(LSP_DIAGNOSTICS, r#"{"path":"a.rs"}"#)
            .unwrap();
        assert_eq!(
            res.content,
            "a.rs:2:5  error[E0425]  cannot find value `x`\n[1 errors]"
        );
        assert_eq!(
            res.subject,
            Some(domain::Subject::Diagnostics {
                path: Some("a.rs".into())
            })
        );
        assert!(
            lsp.opened.lock().unwrap()[0].ends_with("a.rs"),
            "the file was opened first"
        );
        for mode in domain::AgentMode::all() {
            assert!(!domain::modes::denies(*mode, LSP_DIAGNOSTICS));
        }
        assert!(registry.list().iter().any(|s| s.name == LSP_DIAGNOSTICS));
    }

    #[test]
    fn reading_a_file_opens_it_on_the_language_server_once() {
        let (_dir, mut registry, lsp) = sym_registry();
        registry.call(TOOL_READ, r#"{"path":"a.rs"}"#).unwrap();
        registry
            .call(TOOL_READ, r#"{"path":"a.rs","offset":2}"#)
            .unwrap();
        let opened = lsp.opened.lock().unwrap();
        assert_eq!(opened.len(), 1, "{opened:?}");
        assert!(opened[0].ends_with("a.rs"));
    }

    /// CE-DQ14: one line-number convention across tools — the line `grep`
    /// reports is the line the LSP tools address.
    #[test]
    fn a_grep_line_number_addresses_the_same_line_in_lsp_tools() {
        let (_dir, registry, lsp) = sym_registry();
        let root = registry.root.clone();
        let mut registry = registry.with_search(default_search(&root), 2_000_000, 10_000);
        let hit = registry
            .call(TOOL_GREP, r#"{"pattern":"fn start","output":"content"}"#)
            .unwrap();
        let line: u32 = hit
            .content
            .lines()
            .find_map(|l| l.split('│').next()?.trim().parse().ok())
            .unwrap();
        registry
            .call(
                LSP_HOVER,
                &serde_json::json!({ "path": "a.rs", "line": line, "column": 8 }).to_string(),
            )
            .unwrap();
        let (_, l0, c0) = lsp.asked.lock().unwrap().last().unwrap().clone();
        assert_eq!((l0 + 1, c0 + 1), (line, 8));
    }

    #[test]
    fn shell_searches_are_classified() {
        let registry = ToolRegistry::new(PathBuf::from("."));
        let class = |cmd: &str| {
            registry.classify_call(
                TOOL_SHELL,
                &serde_json::json!({ "command": cmd }).to_string(),
            )
        };
        assert_eq!(class("rg -n TODO src"), Some("shell_search"));
        assert_eq!(class("cd web && grep -r foo ."), Some("shell_search"));
        assert_eq!(class("env LC_ALL=C find . -name x"), Some("shell_search"));
        assert_eq!(class("cargo test"), None);
        assert_eq!(registry.classify_call(TOOL_READ, "{}"), None);
    }
}
