//! `outline`, `symbols`, `related` — the code index as tools (FR-INDEX-05..07),
//! with a labelled regex fallback when there is no index or no grammar
//! (FR-INDEX-09).
//!
//! The index is reached through an [`IndexSlot`]: the registry is built
//! before the index is spawned (the index walks with the registry's own
//! search service), so tools hold the slot and read it at call time.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use domain::{
    BoxError, CodeIndexPort, GrepQuery, IndexState, Subject, SymbolDef, SymbolKind, Tool,
    ToolResult, ToolSpec,
};
use serde_json::Value;

use crate::native::{display_path, parse_args, resolve, thousands, tool_error};
use crate::search_tools::Search;

pub const TOOL_OUTLINE: &str = "outline";
pub const TOOL_SYMBOLS: &str = "symbols";
pub const TOOL_RELATED: &str = "related";

/// Where the code index is, once it has been started.
pub type IndexSlot = Arc<OnceLock<Arc<dyn CodeIndexPort>>>;

/// Column the right-aligned `Lstart-end` starts at.
const SPAN_COL: usize = 60;
/// Lines of outline before the footer.
const MAX_OUTLINE_LINES: usize = 300;
/// Definitions per file in a directory outline.
const DIR_DEFS_PER_FILE: usize = 15;
/// Entries per `related` section.
const RELATED_CAP: usize = 20;

fn index(slot: &IndexSlot) -> Option<&Arc<dyn CodeIndexPort>> {
    slot.get()
}

fn building_note(ix: &dyn CodeIndexPort) -> Option<String> {
    match ix.state() {
        IndexState::Building { done, total } => Some(format!(
            "(index building: {done}/{total} files — results may be incomplete)"
        )),
        _ => None,
    }
}

/// A signature as an outline shows it: no visibility, no opening brace.
pub(crate) fn short_signature(sig: &str) -> String {
    let mut s = sig.trim();
    for prefix in [
        "export default ",
        "export ",
        "pub(crate) ",
        "pub(super) ",
        "pub ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim_start();
        }
    }
    // Cut at the body: the first `{` outside parentheses, so a one-line
    // `fn f() { … }` loses its body but `f(x: { a: number })` keeps its type.
    let mut depth = 0i32;
    let mut end = s.len();
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            '{' if depth <= 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    s[..end]
        .trim_end()
        .trim_end_matches([':', ';'])
        .trim_end()
        .to_string()
}

/// `left` padded so the span lands at [`SPAN_COL`], clipped with `…` when
/// it would not fit.
fn aligned(left: &str, span: &str) -> String {
    let width = SPAN_COL - 2;
    let chars = left.chars().count();
    if chars <= width {
        format!("{left}{}  {span}", " ".repeat(width - chars))
    } else {
        let kept: String = left.chars().take(width - 1).collect();
        format!("{kept}…  {span}")
    }
}

fn span_label(d: &SymbolDef) -> String {
    if d.span.start_line == d.span.end_line {
        format!("L{}", d.span.start_line)
    } else {
        format!("L{}-{}", d.span.start_line, d.span.end_line)
    }
}

fn def_line(d: &SymbolDef, indent: usize) -> String {
    let left = format!("{}{}", "  ".repeat(indent), short_signature(&d.signature));
    aligned(&left, &span_label(d))
}

fn str_param<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Regex fallback (FR-INDEX-09)
// ---------------------------------------------------------------------------

/// Definition-looking lines, by extension. Deliberately loose: the result
/// is labelled approximate, and a missed definition costs more than a
/// spurious one.
fn fallback_pattern(ext: &str) -> &'static str {
    match ext {
        "rs" => {
            r"^\s*(pub(\(.*?\))?\s+)?(async\s+)?(unsafe\s+)?(fn|struct|enum|trait|type|mod|const|static|impl|macro_rules!)\b"
        }
        "go" => r"^(func|type|const|var)\b",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => {
            r"^\s*(export\s+)?(default\s+)?(abstract\s+)?(async\s+)?(function\*?|class|interface|type|enum|const|let|namespace)\b"
        }
        "py" | "pyi" => r"^\s*(async\s+)?(def|class)\b",
        "java" | "kt" | "cs" | "swift" | "scala" => {
            r"^\s*((public|private|protected|internal|static|final|abstract|open|override|data|sealed)\s+)*(class|interface|enum|struct|record|object|fun|func|void|def)\b"
        }
        "rb" => r"^\s*(def|class|module)\b",
        "php" => {
            r"^\s*((public|private|protected|static|abstract|final)\s+)*(function|class|interface|trait)\b"
        }
        "c" | "h" | "cc" | "cpp" | "hpp" => {
            r"^(struct|enum|union|typedef|class|namespace)\b|^[A-Za-z_][\w\s\*&:<>]*\([^;]*\)\s*\{?\s*$"
        }
        _ => {
            r"^\s*(export\s+)?(pub\s+)?(def|fn|func|function|class|struct|interface|trait|enum|type|module)\b"
        }
    }
}

fn fallback_outline(root: &Path, full: &Path, why: &str) -> ToolResult {
    let shown = display_path(root, full);
    let text = match std::fs::read_to_string(full) {
        Ok(t) => t,
        Err(e) => return tool_error(format!("cannot read {shown}: {e}")),
    };
    let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("");
    let Ok(re) = regex::Regex::new(fallback_pattern(ext)) else {
        return tool_error("internal: fallback pattern does not compile");
    };
    let total = text.lines().count();
    let mut out = format!("{shown}  ({} lines)\n", thousands(total));
    let mut n = 0;
    for (i, line) in text.lines().enumerate() {
        if re.is_match(line) {
            n += 1;
            if n <= MAX_OUTLINE_LINES {
                let left = format!("  {}", short_signature(line));
                out.push_str(&aligned(&left, &format!("L{}", i + 1)));
                out.push('\n');
            }
        }
    }
    if n > MAX_OUTLINE_LINES {
        out.push_str(&format!("  (+{} more)\n", n - MAX_OUTLINE_LINES));
    }
    out.push_str(&format!(
        "[{n} definition-like lines (approximate — {why})]"
    ));
    ToolResult::ok(&out).with_subject(Subject::Listing { path: shown })
}

// ---------------------------------------------------------------------------
// outline
// ---------------------------------------------------------------------------

pub struct OutlineTool {
    root: PathBuf,
    slot: IndexSlot,
}

impl OutlineTool {
    pub fn new(root: PathBuf, slot: IndexSlot) -> Self {
        Self { root, slot }
    }
}

/// The outline of one file, as `read`'s large-file guard shows it too.
pub(crate) fn render_file_outline(
    shown: &str,
    total_lines: usize,
    defs: &[SymbolDef],
    max_lines: usize,
) -> String {
    let mut out = format!("{shown}  ({} lines)\n", thousands(total_lines));
    for d in defs.iter().take(max_lines) {
        out.push_str(&def_line(d, usize::from(d.depth) + 1));
        out.push('\n');
    }
    if defs.len() > max_lines {
        out.push_str(&format!("  (+{} more)\n", defs.len() - max_lines));
    }
    out.push_str(&format!(
        "[{}]",
        crate::render::count(defs.len() as u64, "definition", "definitions")
    ));
    out
}

fn render_dir_outline(shown: &str, defs: &[SymbolDef]) -> String {
    if defs.is_empty() {
        return format!("{shown}: no definitions in files directly inside it (outline a file, or glob for files)");
    }
    let mut out = String::new();
    let mut lines = 0;
    let mut files = 0;
    let mut i = 0;
    while i < defs.len() {
        let path = &defs[i].path;
        let group: Vec<&SymbolDef> = defs[i..].iter().take_while(|d| d.path == *path).collect();
        i += group.len();
        if lines >= MAX_OUTLINE_LINES {
            files += 1;
            continue;
        }
        out.push_str(path);
        out.push('\n');
        for d in group.iter().take(DIR_DEFS_PER_FILE) {
            out.push_str(&def_line(d, 1));
            out.push('\n');
        }
        if group.len() > DIR_DEFS_PER_FILE {
            out.push_str(&format!("  (+{} more)\n", group.len() - DIR_DEFS_PER_FILE));
        }
        lines += 1 + group.len().min(DIR_DEFS_PER_FILE);
    }
    if files > 0 {
        out.push_str(&format!(
            "({files} more files not shown — outline a subdirectory)\n"
        ));
    }
    out.push_str(&format!(
        "[{} top-level in {shown}]",
        crate::render::count(defs.len() as u64, "definition", "definitions")
    ));
    out
}

impl Tool for OutlineTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_OUTLINE.into(),
            description: "List the definitions in a file (or the top-level ones in each file of a \
                          directory) with their signatures and line spans — no bodies. Far \
                          cheaper than read: outline first, then read just the lines you need."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"File or directory"}},"required":["path"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let Some(path) = str_param(&args, "path") else {
            return Ok(tool_error("missing required argument `path`"));
        };
        let full = resolve(&self.root, path);
        let shown = display_path(&self.root, &full);
        if !full.exists() {
            return Ok(tool_error(format!("{shown}: no such file or directory")));
        }
        let Some(ix) = index(&self.slot) else {
            if full.is_dir() {
                return Ok(tool_error(
                    "the code index is off, so a directory cannot be outlined — \
                     outline a file, or use list_dir",
                ));
            }
            return Ok(fallback_outline(&self.root, &full, "the code index is off"));
        };
        let subject = Subject::Listing {
            path: shown.clone(),
        };
        let defs = match ix.outline(&full.to_string_lossy()) {
            Ok(Some(defs)) => defs,
            Ok(None) => {
                return Ok(fallback_outline(
                    &self.root,
                    &full,
                    "no parser for this language",
                ))
            }
            Err(e) => return Ok(tool_error(e.to_string())),
        };
        let mut out = if full.is_dir() {
            render_dir_outline(&shown, &defs)
        } else {
            let total = std::fs::read_to_string(&full)
                .map(|t| t.lines().count())
                .unwrap_or(0);
            render_file_outline(&shown, total, &defs, MAX_OUTLINE_LINES)
        };
        if let Some(note) = building_note(ix.as_ref()) {
            out.push('\n');
            out.push_str(&note);
        }
        Ok(ToolResult::ok(&out).with_subject(subject))
    }
}

// ---------------------------------------------------------------------------
// symbols
// ---------------------------------------------------------------------------

pub struct SymbolsTool {
    root: PathBuf,
    slot: IndexSlot,
    search: Search,
}

impl SymbolsTool {
    pub fn new(root: PathBuf, slot: IndexSlot, search: Search) -> Self {
        Self { root, slot, search }
    }

    fn fallback(&self, query: &str, prefix: Option<&str>, limit: usize) -> ToolResult {
        let leaf = query.rsplit(['.', ':']).next().unwrap_or(query);
        let pattern = format!(
            r"\b(fn|func|def|class|struct|interface|type|trait|enum|mod|module|const|function|let|var)\s+(\([^)]*\)\s*)?\w*{}\w*",
            regex::escape(leaf)
        );
        let root = resolve(&self.root, prefix.unwrap_or("."));
        let mut q = GrepQuery::new(pattern, root);
        q.case = domain::CaseMode::Insensitive;
        q.max_lines_per_file = 5;
        let outcome = match self.search.grep(&q, &|| false) {
            Ok(o) => o,
            Err(e) => return tool_error(e.to_string()),
        };
        let mut out = String::new();
        let mut n = 0;
        for f in outcome.files.iter() {
            for l in f.lines.iter().filter(|l| !l.is_context) {
                if n < limit {
                    out.push_str(&format!(
                        "{}:{}  {}\n",
                        f.path,
                        l.line,
                        crate::render::clip_line(l.text.trim(), 120, 0)
                    ));
                }
                n += 1;
            }
        }
        if n == 0 {
            return ToolResult::ok(&format!(
                "no definitions matching `{query}` (approximate search — the code index is off)"
            ));
        }
        out.push_str(&format!(
            "[{n} definition-like lines (approximate — the code index is off)]"
        ));
        ToolResult::ok(&out)
    }
}

fn parse_kind(args: &Value) -> Result<Option<SymbolKind>, ToolResult> {
    match str_param(args, "kind") {
        None => Ok(None),
        Some(k) => SymbolKind::parse(k).map(Some).ok_or_else(|| {
            tool_error(format!(
                "unknown kind `{k}`; expected function, method, struct, enum, trait, \
                 interface, class, type, const, module or impl"
            ))
        }),
    }
}

impl Tool for SymbolsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_SYMBOLS.into(),
            description: "Find where functions, types and other definitions are, by name: exact \
                          matches first, then prefix, then fuzzy. `Type::method` or `Class.method` \
                          narrows to a container. Returns path:lines and signatures."
                .into(),
            params_json: r#"{"type":"object","properties":{"query":{"type":"string"},"kind":{"type":"string","enum":["function","method","struct","enum","trait","interface","class","type","const","module","impl"]},"path":{"type":"string","description":"Only under this path"},"limit":{"type":"integer","minimum":1}},"required":["query"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let Some(query) = str_param(&args, "query") else {
            return Ok(tool_error("missing required argument `query`"));
        };
        let kind = match parse_kind(&args) {
            Ok(k) => k,
            Err(e) => return Ok(e),
        };
        let prefix = str_param(&args, "path");
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(20, |n| n.clamp(1, 200) as usize);
        let Some(ix) = index(&self.slot) else {
            return Ok(self.fallback(query, prefix, limit));
        };
        let prefix_abs = prefix.map(|p| resolve(&self.root, p).to_string_lossy().into_owned());
        // One more than asked, to know whether there are more.
        let hits = match ix.symbols(query, kind, prefix_abs.as_deref(), limit + 1) {
            Ok(h) => h,
            Err(e) => return Ok(tool_error(e.to_string())),
        };
        let more = hits.len() > limit;
        let mut out = String::new();
        if hits.is_empty() {
            out.push_str(&format!("no definitions match `{query}`"));
            if kind.is_some() || prefix.is_some() {
                out.push_str(" with those filters");
            }
            out.push_str(" (try a shorter query, or grep for text)");
        }
        for d in hits.iter().take(limit) {
            out.push_str(&format!(
                "{:<6} {} — {}:{}  {}\n",
                d.kind.as_str(),
                d.qualified,
                d.path,
                span_label(d).trim_start_matches('L'),
                crate::render::clip_line(&short_signature(&d.signature), 100, 0)
            ));
        }
        if !hits.is_empty() {
            let shown = hits.len().min(limit);
            out.push_str(&format!(
                "[{}{}]",
                crate::render::count(shown as u64, "match", "matches"),
                if more {
                    "; more exist — narrow with kind or path"
                } else {
                    ""
                }
            ));
        }
        if let Some(note) = building_note(ix.as_ref()) {
            out.push('\n');
            out.push_str(&note);
        }
        Ok(ToolResult::ok(&out))
    }
}

// ---------------------------------------------------------------------------
// related
// ---------------------------------------------------------------------------

pub struct RelatedTool {
    root: PathBuf,
    slot: IndexSlot,
    search: Search,
}

impl RelatedTool {
    pub fn new(root: PathBuf, slot: IndexSlot, search: Search) -> Self {
        Self { root, slot, search }
    }

    /// Without an index: name occurrences for a symbol, import-looking
    /// lines naming the file's stem for a path.
    fn fallback(&self, path: Option<&str>, symbol: Option<&str>) -> ToolResult {
        let (pattern, what) = match (symbol, path) {
            (Some(s), _) => {
                let leaf = s.rsplit(['.', ':']).next().unwrap_or(s);
                (format!(r"\b{}\b", regex::escape(leaf)), format!("`{leaf}`"))
            }
            (None, Some(p)) => {
                let stem = Path::new(p)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(p)
                    .to_string();
                (
                    format!(
                        r"^\s*(import|from|use|require|include|#include)\b.*\b{}\b",
                        regex::escape(&stem)
                    ),
                    format!("imports of `{stem}`"),
                )
            }
            (None, None) => return tool_error("give `path` or `symbol`"),
        };
        let mut q = GrepQuery::new(pattern, self.root.clone());
        q.case = domain::CaseMode::Sensitive;
        q.max_lines_per_file = 3;
        let outcome = match self.search.grep(&q, &|| false) {
            Ok(o) => o,
            Err(e) => return tool_error(e.to_string()),
        };
        let mut out = format!("{what} ≈ (approximate — the code index is off)\n");
        let mut n = 0;
        for f in outcome.files.iter() {
            for l in f.lines.iter().filter(|l| !l.is_context) {
                if n < RELATED_CAP {
                    out.push_str(&format!("  {}:{}\n", f.path, l.line));
                }
                n += 1;
            }
        }
        if n == 0 {
            out.push_str("  (none found)\n");
        } else if n > RELATED_CAP {
            out.push_str(&format!("  (+{} more)\n", n - RELATED_CAP));
        }
        out.truncate(out.trim_end().len());
        ToolResult::ok(&out)
    }
}

fn section(out: &mut String, title: &str, items: &[String]) {
    out.push_str(title);
    out.push('\n');
    if items.is_empty() {
        out.push_str("  (none)\n");
        return;
    }
    for item in items.iter().take(RELATED_CAP) {
        out.push_str("  ");
        out.push_str(item);
        out.push('\n');
    }
    if items.len() > RELATED_CAP {
        out.push_str(&format!("  (+{} more)\n", items.len() - RELATED_CAP));
    }
}

impl Tool for RelatedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_RELATED.into(),
            description: "A file's neighbourhood (what it imports, what imports it) or a symbol's \
                          (where it is defined, where its name is used). Give `path` or `symbol`. \
                          Name-based and approximate; lsp__find_references is exact."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string"},"symbol":{"type":"string"}}}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let path = str_param(&args, "path");
        let symbol = str_param(&args, "symbol");
        if path.is_none() && symbol.is_none() {
            return Ok(tool_error("give `path` (a file) or `symbol` (a name)"));
        }
        let Some(ix) = index(&self.slot) else {
            return Ok(self.fallback(path, symbol));
        };
        let mut out = String::new();
        if let Some(sym) = symbol {
            let r = match ix.related(sym) {
                Ok(r) => r,
                Err(e) => return Ok(tool_error(e.to_string())),
            };
            section(&mut out, &format!("`{sym}` defined in"), &r.defined_in);
            section(&mut out, "referenced in ≈ (by name)", &r.referenced_in);
            out.push_str(&format!(
                "for exact references use lsp__find_references symbol=\"{sym}\""
            ));
        } else if let Some(p) = path {
            let full = resolve(&self.root, p);
            if !full.is_file() {
                return Ok(tool_error(format!(
                    "{}: not a file (for a name, pass `symbol`)",
                    display_path(&self.root, &full)
                )));
            }
            let r = match ix.related(&full.to_string_lossy()) {
                Ok(r) => r,
                Err(e) => return Ok(tool_error(e.to_string())),
            };
            section(&mut out, "imports", &r.imports);
            section(&mut out, "imported by", &r.imported_by);
            out.truncate(out.trim_end().len());
        }
        if let Some(note) = building_note(ix.as_ref()) {
            out.push('\n');
            out.push_str(&note);
        }
        Ok(ToolResult::ok(&out))
    }
}

/// Up to `max_lines` of outline for the definitions starting after line
/// `after` — what `read`'s large-file guard appends (FR-READ-02).
pub(crate) fn outline_after(
    ix: &dyn CodeIndexPort,
    full: &Path,
    after: u32,
    max_lines: usize,
) -> Option<String> {
    let defs = ix.outline(&full.to_string_lossy()).ok()??;
    let rest: Vec<&SymbolDef> = defs.iter().filter(|d| d.span.start_line > after).collect();
    if rest.is_empty() {
        return None;
    }
    let mut out = String::from("outline of the rest:\n");
    for d in rest.iter().take(max_lines) {
        out.push_str(&def_line(d, usize::from(d.depth) + 1));
        out.push('\n');
    }
    if rest.len() > max_lines {
        out.push_str(&format!(
            "  (+{} more — use outline)\n",
            rest.len() - max_lines
        ));
    }
    out.truncate(out.trim_end().len());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use domain::{LspLocation, LspPort, LspWorkspaceEdit, SearchPort};
    use infra_index::{CodeIndex, IndexOptions};

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn search(root: &Path) -> Search {
        Arc::new(
            infra_search::RipgrepSearch::new(root, &infra_search::FilterConfig::default()).unwrap(),
        )
    }

    fn indexed(root: &Path) -> IndexSlot {
        let search: Arc<dyn SearchPort> = search(root);
        let ix = CodeIndex::open(IndexOptions::new(root), search);
        ix.build();
        let slot = IndexSlot::default();
        let _ = slot.set(ix);
        slot
    }

    const ENGINE: &str = "/// The loop.\npub struct Engine {\n    turns: u32,\n}\n\n\
                          impl Engine {\n    pub fn new() -> Self {\n        Engine { turns: 0 }\n    }\n\n    \
                          /// Runs a very long named operation with many parameters in it.\n    \
                          pub fn execute_with_a_rather_long_name(&mut self, request: Request, budget: u32) -> Result<(), Error> {\n        \
                          Ok(())\n    }\n}\n\npub fn helper() {}\n";

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/engine.rs", ENGINE);
        write(
            dir.path(),
            "src/lib.rs",
            "mod engine;\nuse crate::engine::Engine;\npub fn start() { Engine::new(); }\n",
        );
        write(
            dir.path(),
            "notes/script.lua",
            "local function greet(name)\n  print(name)\nend\nfunction M.run() end\n",
        );
        dir
    }

    fn call(tool: &mut dyn Tool, args: &str) -> ToolResult {
        tool.call("", args).unwrap()
    }

    #[test]
    fn outline_shows_signatures_and_aligned_spans_without_bodies() {
        let dir = project();
        let mut tool = OutlineTool::new(dir.path().to_path_buf(), indexed(dir.path()));
        let res = call(&mut tool, r#"{"path":"src/engine.rs"}"#);
        assert!(res.error.is_none(), "{res:?}");
        let lines: Vec<&str> = res.content.lines().collect();
        assert_eq!(lines[0], "src/engine.rs  (17 lines)");
        assert_eq!(lines[1], aligned("  struct Engine", "L1-4"));
        assert_eq!(lines[2], aligned("  impl Engine", "L6-15"));
        assert_eq!(lines[3], aligned("    fn new() -> Self", "L7-9"));
        // Too long for the column: clipped with an ellipsis, span kept.
        assert!(lines[4].starts_with("    fn execute_with_a_rather_long_name(&mut self, request…"));
        assert!(lines[4].ends_with("  L11-14"), "{}", lines[4]);
        assert_eq!(lines[5], aligned("  fn helper()", "L17"));
        assert_eq!(lines[6], "[5 definitions]");
        assert!(!res.content.contains("Ok(())"), "no bodies");
        assert_eq!(
            res.subject,
            Some(Subject::Listing {
                path: "src/engine.rs".into()
            })
        );
    }

    #[test]
    fn outline_of_a_directory_lists_top_level_definitions_per_file() {
        let dir = project();
        let mut tool = OutlineTool::new(dir.path().to_path_buf(), indexed(dir.path()));
        let res = call(&mut tool, r#"{"path":"src"}"#);
        let text = &res.content;
        assert!(text.starts_with("src/engine.rs\n"), "{text}");
        assert!(text.contains("\nsrc/lib.rs\n"), "{text}");
        assert!(!text.contains("fn new"), "only top level: {text}");
        assert!(text.ends_with("[5 definitions top-level in src]"), "{text}");
        assert!(text.contains(&aligned("  fn start()", "L3")), "{text}");
    }

    #[test]
    fn outline_falls_back_to_a_labelled_scan_without_a_grammar_or_an_index() {
        let dir = project();
        let mut tool = OutlineTool::new(dir.path().to_path_buf(), indexed(dir.path()));
        let res = call(&mut tool, r#"{"path":"notes/script.lua"}"#);
        assert!(
            res.content
                .contains("approximate — no parser for this language"),
            "{res:?}"
        );

        let mut bare = OutlineTool::new(dir.path().to_path_buf(), IndexSlot::default());
        let res = call(&mut bare, r#"{"path":"src/engine.rs"}"#);
        assert!(
            res.content.contains("approximate — the code index is off"),
            "{res:?}"
        );
        assert!(res.content.contains("fn helper()"));
        assert!(res.content.contains("L17"));
    }

    #[test]
    fn symbols_rank_and_render_one_line_per_definition() {
        let dir = project();
        let slot = indexed(dir.path());
        let mut tool = SymbolsTool::new(dir.path().to_path_buf(), slot, search(dir.path()));
        let res = call(&mut tool, r#"{"query":"Engine::new"}"#);
        assert_eq!(
            res.content,
            "method Engine::new — src/engine.rs:7-9  fn new() -> Self\n[1 match]"
        );
        let res = call(&mut tool, r#"{"query":"e","limit":2}"#);
        assert!(res
            .content
            .ends_with("[2 matches; more exist — narrow with kind or path]"));
        let res = call(&mut tool, r#"{"query":"helper","kind":"struct"}"#);
        assert!(res
            .content
            .starts_with("no definitions match `helper` with those filters"));
        let res = call(&mut tool, r#"{"query":"x","kind":"bogus"}"#);
        assert!(res.error.unwrap().starts_with("unknown kind `bogus`"));
    }

    #[test]
    fn symbols_fall_back_to_grep_without_an_index() {
        let dir = project();
        let mut tool = SymbolsTool::new(
            dir.path().to_path_buf(),
            IndexSlot::default(),
            search(dir.path()),
        );
        let res = call(&mut tool, r#"{"query":"helper"}"#);
        assert!(
            res.content.contains("src/engine.rs:17  pub fn helper() {}"),
            "{res:?}"
        );
        assert!(res.content.contains("approximate — the code index is off"));
    }

    #[test]
    fn related_names_both_directions_and_points_at_the_exact_tool() {
        let dir = project();
        let slot = indexed(dir.path());
        let mut tool = RelatedTool::new(dir.path().to_path_buf(), slot, search(dir.path()));
        let res = call(&mut tool, r#"{"path":"src/engine.rs"}"#);
        assert_eq!(res.content, "imports\n  (none)\nimported by\n  src/lib.rs");
        let res = call(&mut tool, r#"{"symbol":"Engine"}"#);
        assert!(
            res.content
                .starts_with("`Engine` defined in\n  src/engine.rs:2\n"),
            "{res:?}"
        );
        assert!(res.content.contains("  src/lib.rs:2\n"), "{res:?}");
        assert!(res
            .content
            .ends_with("for exact references use lsp__find_references symbol=\"Engine\""));
        assert!(call(&mut tool, "{}").error.is_some());
    }

    /// Reports `Building` whatever it holds.
    struct Building(Arc<dyn CodeIndexPort>);

    impl CodeIndexPort for Building {
        fn state(&self) -> IndexState {
            IndexState::Building { done: 3, total: 10 }
        }
        fn outline(&self, p: &str) -> Result<Option<Vec<SymbolDef>>, BoxError> {
            self.0.outline(p)
        }
        fn symbols(
            &self,
            q: &str,
            k: Option<SymbolKind>,
            p: Option<&str>,
            l: usize,
        ) -> Result<Vec<SymbolDef>, BoxError> {
            self.0.symbols(q, k, p, l)
        }
        fn locate(&self, p: Option<&str>, q: &str) -> Result<Vec<SymbolDef>, BoxError> {
            self.0.locate(p, q)
        }
        fn related(&self, t: &str) -> Result<domain::Related, BoxError> {
            self.0.related(t)
        }
        fn parse_text(&self, p: &str, t: &str) -> Result<Option<domain::ParsedFile>, BoxError> {
            self.0.parse_text(p, t)
        }
        fn repo_map(&self, p: &str, b: u32) -> Result<String, BoxError> {
            self.0.repo_map(p, b)
        }
        fn notify_changed(&self, p: &str) {
            self.0.notify_changed(p)
        }
    }

    #[test]
    fn tools_label_a_building_index() {
        let dir = project();
        let inner = indexed(dir.path()).get().unwrap().clone();
        let slot = IndexSlot::default();
        let _ = slot.set(Arc::new(Building(inner)));
        let note = "(index building: 3/10 files — results may be incomplete)";
        let root = dir.path().to_path_buf();
        let mut outline = OutlineTool::new(root.clone(), slot.clone());
        assert!(call(&mut outline, r#"{"path":"src/engine.rs"}"#)
            .content
            .ends_with(note));
        let mut symbols = SymbolsTool::new(root.clone(), slot.clone(), search(&root));
        assert!(call(&mut symbols, r#"{"query":"helper"}"#)
            .content
            .ends_with(note));
        let mut related = RelatedTool::new(root.clone(), slot, search(&root));
        assert!(call(&mut related, r#"{"symbol":"helper"}"#)
            .content
            .ends_with(note));
    }

    #[test]
    fn the_read_guard_appends_an_outline_of_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mut src = String::new();
        for i in 0..80 {
            src.push_str(&format!("pub fn f{i}() {{\n    let x = {i};\n}}\n\n"));
        }
        write(dir.path(), "big.rs", &src);
        let slot = indexed(dir.path());
        let mut read = crate::ReadTool::new(dir.path().to_path_buf())
            .with_default_limit(100)
            .with_index(slot);
        let res = call(&mut read, r#"{"path":"big.rs"}"#);
        let text = &res.content;
        assert!(text.contains("[lines 1-100 of 320 — read the part you need with offset/limit]\noutline of the rest:\n"), "{text}");
        // f25 starts on line 101: the first definition not shown.
        assert!(text.contains(&aligned("  fn f25()", "L101-103")), "{text}");
        assert!(!text.contains("  fn f24()  "), "{text}");
        assert!(text.ends_with("  (+15 more — use outline)"), "{text}");
        // An explicit range is what was asked for: no outline.
        let res = call(&mut read, r#"{"path":"big.rs","offset":1,"limit":10}"#);
        assert!(!res.content.contains("outline of the rest"));
    }

    /// A server that knows nothing — so a resolved position can only have
    /// come from the index.
    struct EmptyLsp(Mutex<u32>);

    impl LspPort for EmptyLsp {
        fn goto_definition(&mut self, _: &str, _: u32, _: u32) -> Result<LspLocation, BoxError> {
            Err("unused".into())
        }
        fn find_references(
            &mut self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<Box<[LspLocation]>, BoxError> {
            Err("unused".into())
        }
        fn hover(&mut self, _: &str, _: u32, _: u32) -> Result<String, BoxError> {
            Err("unused".into())
        }
        fn rename_symbol(
            &mut self,
            _: &str,
            _: u32,
            _: u32,
            _: &str,
        ) -> Result<LspWorkspaceEdit, BoxError> {
            Err("unused".into())
        }
        fn open_document(&mut self, _: &str, _: &str) -> Result<(), BoxError> {
            Ok(())
        }
        fn workspace_symbols(&mut self, _: &str) -> Result<Box<[domain::LspSymbolInfo]>, BoxError> {
            *self.0.lock().unwrap() += 1;
            Ok(Box::new([]))
        }
    }

    #[test]
    fn lsp_symbol_resolution_prefers_the_index() {
        let dir = project();
        let slot = indexed(dir.path());
        let ix = slot.get().unwrap().clone();
        let root = dir.path().to_path_buf();
        let uri_for = |p: &str| format!("file://{}", resolve(&root, p).display());
        let mut lsp = EmptyLsp(Mutex::new(0));
        let target = crate::lsp_tools::Target::Symbol {
            symbol: "Engine::new".into(),
            path: None,
        };
        let (uri, line, col) =
            crate::lsp_tools::resolve(&mut lsp, &root, &target, &uri_for, Some(ix.as_ref()))
                .unwrap();
        assert!(uri.ends_with("src/engine.rs"), "{uri}");
        // `    pub fn new()`: line 7, name at char column 12 → wire (6, 11).
        assert_eq!((line, col), (6, 11));
        assert_eq!(*lsp.0.lock().unwrap(), 0, "the server was not asked");
        // `Engine` names the struct, not its impl block.
        let target = crate::lsp_tools::Target::Symbol {
            symbol: "Engine".into(),
            path: None,
        };
        let (_, line, _) =
            crate::lsp_tools::resolve(&mut lsp, &root, &target, &uri_for, Some(ix.as_ref()))
                .unwrap();
        assert_eq!(line, 1);
        // Unknown to the index: the server is asked.
        let target = crate::lsp_tools::Target::Symbol {
            symbol: "nowhere".into(),
            path: None,
        };
        assert!(
            crate::lsp_tools::resolve(&mut lsp, &root, &target, &uri_for, Some(ix.as_ref()))
                .is_err()
        );
        assert_eq!(*lsp.0.lock().unwrap(), 1);
    }

    /// FR-INDEX-05 acceptance: an outline costs a small fraction of reading
    /// the file, measured over this workspace's own larger sources and the
    /// eval fixtures.
    #[test]
    fn outline_is_under_15pct_of_full_read_tokens() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let workspace = workspace.canonicalize().unwrap();
        let slot = indexed(&workspace);
        let mut tool = OutlineTool::new(workspace.clone(), slot);
        let mut files: Vec<PathBuf> = Vec::new();
        for dir in [
            "crates/app/src",
            "crates/tools/src",
            "crates/infra/index/src",
            "evals/fixtures",
        ] {
            collect(&workspace.join(dir), &mut files);
        }
        let (mut outline_tokens, mut read_tokens) = (0u64, 0u64);
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            let args = serde_json::json!({ "path": f.to_string_lossy() }).to_string();
            let res = call(&mut tool, &args);
            assert!(res.error.is_none(), "{f:?}: {res:?}");
            outline_tokens += domain::tokens::estimate_tokens(&res.content);
            read_tokens += domain::tokens::estimate_tokens(&text);
        }
        let ratio = outline_tokens as f64 / read_tokens as f64;
        eprintln!(
            "{} files: outline {outline_tokens} / read {read_tokens} tokens = {ratio:.3}",
            files.len()
        );
        assert!(files.len() >= 20);
        assert!(ratio < 0.15, "{ratio}");
    }

    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect(&p, out);
            } else if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("rs" | "go" | "ts" | "tsx" | "py")
            ) {
                out.push(p);
            }
        }
    }
}
