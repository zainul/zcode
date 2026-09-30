//! `edit_symbol` — edit a definition by name (FR-EDIT-01..05, 09).
//!
//! The target is found by parsing the file's *current* text — never from
//! the index's stored spans (PRD R6), so an index that has not seen the
//! latest change cannot misplace an edit. The edit is re-parsed before it
//! is written and refused if it adds syntax errors; the result shows only
//! the seams.
#![deny(clippy::unwrap_used)]

use std::path::PathBuf;

use domain::{BoxError, ParsedFile, Subject, SymbolDef, SymbolKind, Tool, ToolResult, ToolSpec};
use infra_config::SyntaxCheck;
use infra_filesystem::StdFs;
use serde_json::Value;

use crate::edit::{record_write, seams, WriteLog};
use crate::index_tools::IndexSlot;
use crate::native::{display_path, parse_args, resolve, tool_error};

pub const TOOL_EDIT_SYMBOL: &str = "edit_symbol";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Replace,
    ReplaceBody,
    InsertBefore,
    InsertAfter,
    Delete,
}

impl Action {
    fn parse(s: &str) -> Option<Action> {
        Some(match s.trim() {
            "replace" => Action::Replace,
            "replace_body" => Action::ReplaceBody,
            "insert_before" => Action::InsertBefore,
            "insert_after" => Action::InsertAfter,
            "delete" => Action::Delete,
            _ => return None,
        })
    }

    fn past(self) -> &'static str {
        match self {
            Action::Replace => "replaced",
            Action::ReplaceBody => "replaced the body of",
            Action::InsertBefore => "inserted before",
            Action::InsertAfter => "inserted after",
            Action::Delete => "deleted",
        }
    }
}

pub struct EditSymbolTool {
    root: PathBuf,
    fs: StdFs,
    log: WriteLog,
    slot: IndexSlot,
    syntax: SyntaxCheck,
}

impl EditSymbolTool {
    pub fn new(root: PathBuf, slot: IndexSlot) -> Self {
        Self {
            root,
            fs: StdFs,
            log: WriteLog::default(),
            slot,
            syntax: SyntaxCheck::Reject,
        }
    }

    pub fn with_write_log(mut self, log: WriteLog) -> Self {
        self.log = log;
        self
    }

    /// What to do with an edit that adds syntax errors (`edit.syntax_check`).
    pub fn with_syntax_check(mut self, mode: SyntaxCheck) -> Self {
        self.syntax = mode;
        self
    }
}

// ---------------------------------------------------------------------------
// Indentation (FR-EDIT-05)
// ---------------------------------------------------------------------------

fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Width of one indentation level in space-indented text (2..=8, else 4).
fn space_unit(text: &str) -> usize {
    let g = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(leading_ws)
        .filter(|w| !w.is_empty() && !w.contains('\t'))
        .map(str::len)
        .fold(0, gcd);
    if (2..=8).contains(&g) {
        g
    } else {
        4
    }
}

/// One indentation level in the file's own style.
pub(crate) fn detect_unit(text: &str) -> String {
    let (mut tabs, mut spaces) = (0, 0);
    for l in text.lines() {
        if l.starts_with('\t') {
            tabs += 1;
        } else if l.starts_with(' ') && !l.trim().is_empty() {
            spaces += 1;
        }
    }
    if tabs > spaces {
        "\t".into()
    } else {
        " ".repeat(space_unit(text))
    }
}

/// Re-indent `content` to sit at `indent`, converting its own indentation
/// levels to `unit`. Works whether the model sent the block flush-left or
/// already indented: the common minimum is stripped first.
pub(crate) fn reindent_block(content: &str, indent: &str, unit: &str) -> String {
    let content = content.replace("\r\n", "\n");
    let lines: Vec<&str> = content
        .trim_end_matches(['\n', ' ', '\t'])
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .collect();
    let min = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_ws(l).len())
        .min()
        .unwrap_or(0);
    let content_unit = space_unit(&lines.join("\n"));
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let rest = line.get(min..).unwrap_or(line);
        let ws = leading_ws(rest);
        let body = &rest[ws.len()..];
        // Levels in the content's own units, then in the file's.
        let spaces: usize = ws
            .chars()
            .map(|c| if c == '\t' { content_unit } else { 1 })
            .sum();
        let (levels, extra) = (spaces / content_unit, spaces % content_unit);
        out.push(format!(
            "{indent}{}{}{body}",
            unit.repeat(levels),
            " ".repeat(extra)
        ));
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------
// Locating the target
// ---------------------------------------------------------------------------

fn norm(q: &str) -> String {
    q.replace("::", ".")
}

fn has_separator(q: &str) -> bool {
    q.contains("::") || q.contains('.')
}

fn matches_symbol(d: &SymbolDef, symbol: &str) -> bool {
    if has_separator(symbol) {
        let (full, want) = (norm(&d.qualified), norm(symbol));
        full == want || full.ends_with(&format!(".{want}"))
    } else {
        d.name == symbol
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn candidate_line(d: &SymbolDef) -> String {
    format!(
        "  {} {} — L{}-{}",
        d.kind.as_str(),
        d.qualified,
        d.span.start_line,
        d.span.end_line
    )
}

/// The one definition `symbol` names, or the error to show the model.
fn pick<'a>(
    parsed: &'a ParsedFile,
    symbol: &str,
    line: Option<u32>,
    shown: &str,
) -> Result<&'a SymbolDef, String> {
    let mut found: Vec<&SymbolDef> = parsed
        .defs
        .iter()
        .filter(|d| matches_symbol(d, symbol))
        .filter(|d| line.is_none_or(|l| d.span.start_line <= l && l <= d.span.end_line))
        .collect();
    // `impl Foo` shares its type's name; unless the impl is all there is,
    // the type is what is meant.
    if found.len() > 1 && found.iter().any(|d| d.kind != SymbolKind::Impl) {
        found.retain(|d| d.kind != SymbolKind::Impl);
    }
    // Nested definitions of one name all contain the line; the innermost
    // is meant.
    if line.is_some() && found.len() > 1 {
        if let Some(depth) = found.iter().map(|d| d.depth).max() {
            found.retain(|d| d.depth == depth);
        }
    }
    match found.as_slice() {
        [one] => Ok(one),
        [] => {
            let leaf = symbol.rsplit(['.', ':']).next().unwrap_or(symbol);
            let want = leaf.to_lowercase();
            let mut near: Vec<(usize, &SymbolDef)> = parsed
                .defs
                .iter()
                .filter(|d| d.kind != SymbolKind::Impl)
                .map(|d| (levenshtein(&d.name.to_lowercase(), &want), d))
                .filter(|(dist, d)| *dist <= 3 || d.name == leaf)
                .collect();
            near.sort_by_key(|(dist, d)| (*dist, d.span.start_line));
            let mut msg = format!("no definition named `{symbol}` in {shown}");
            if line.is_some() {
                msg.push_str(" containing that line");
            }
            if near.is_empty() {
                msg.push_str(" — outline the file to see what it defines");
            } else {
                msg.push_str("; did you mean:\n");
                let list: Vec<String> = near
                    .iter()
                    .take(10)
                    .map(|(_, d)| candidate_line(d))
                    .collect();
                msg.push_str(&list.join("\n"));
            }
            Err(msg)
        }
        many => {
            let list: Vec<String> = many.iter().take(10).map(|d| candidate_line(d)).collect();
            Err(format!(
                "`{symbol}` is ambiguous in {shown} ({} definitions) — qualify it or pass \
                 `line`:\n{}",
                many.len(),
                list.join("\n")
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Splicing
// ---------------------------------------------------------------------------

fn line_start(text: &str, at: usize) -> usize {
    text.get(..at)
        .and_then(|s| s.rfind('\n'))
        .map_or(0, |i| i + 1)
}

/// Just past the newline ending the line that contains `at` (or the end).
fn line_end(text: &str, at: usize) -> usize {
    text.get(at..)
        .and_then(|s| s.find('\n'))
        .map_or(text.len(), |i| at + i + 1)
}

fn is_blank_line(text: &str, start: usize) -> bool {
    let end = line_end(text, start);
    start < end && text.get(start..end).is_some_and(|l| l.trim().is_empty())
}

/// A byte range of the file to replace, and the text to put there (with
/// `\n` line ends; converted to the file's style at the end).
struct Splice {
    range: std::ops::Range<usize>,
    text: String,
}

fn brace_language(path: &str) -> bool {
    !(path.ends_with(".py") || path.ends_with(".pyi"))
}

fn plan(
    text: &str,
    path: &str,
    def: &SymbolDef,
    action: Action,
    content: &str,
) -> Result<Splice, String> {
    let start = def.span.start_byte as usize;
    let end = def.span.end_byte as usize;
    if end > text.len() || start > end {
        return Err("internal: the parse does not match the file".into());
    }
    let first_line = line_start(text, start);
    let indent = leading_ws(text.get(first_line..).unwrap_or("")).to_string();
    let unit = detect_unit(text);
    let block = |extra: &str| reindent_block(content, &format!("{indent}{extra}"), &unit);
    Ok(match action {
        Action::Replace => Splice {
            // Whole lines, so the replacement's own indentation is the one
            // that lands.
            range: first_line..end,
            text: block(""),
        },
        Action::Delete => {
            let mut range = first_line..line_end(text, end);
            if range.end < text.len() && is_blank_line(text, range.end) {
                range.end = line_end(text, range.end);
            } else if range.start > 0 {
                let prev = line_start(text, range.start - 1);
                if is_blank_line(text, prev) {
                    range.start = prev;
                }
            }
            Splice {
                range,
                text: String::new(),
            }
        }
        Action::InsertBefore => Splice {
            range: first_line..first_line,
            text: format!("{}\n\n", block("")),
        },
        Action::InsertAfter => {
            let at = line_end(text, end);
            let lead = if text.get(..at).is_some_and(|s| s.ends_with('\n')) {
                "\n"
            } else {
                "\n\n"
            };
            Splice {
                range: at..at,
                text: format!("{lead}{}\n", block("")),
            }
        }
        Action::ReplaceBody => {
            let Some(body) = def.body else {
                return Err(format!(
                    "{} has no body to replace (a declaration only?) — use action `replace`",
                    def.qualified
                ));
            };
            let (b_start, b_end) = (body.start_byte as usize, body.end_byte as usize);
            if brace_language(path) {
                let trimmed = content.trim();
                let inner = match trimmed.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
                    Some(inner) => inner,
                    None => trimmed,
                };
                let inner = inner.trim_matches('\n');
                let text = if inner.trim().is_empty() {
                    "{}".to_string()
                } else {
                    format!(
                        "{{\n{}\n{indent}}}",
                        reindent_block(inner, &format!("{indent}{unit}"), &unit)
                    )
                };
                Splice {
                    range: b_start..b_end,
                    text,
                }
            } else {
                // Python: the block is whole lines at its own indentation.
                let body_line = line_start(text, b_start);
                let body_indent = leading_ws(text.get(body_line..).unwrap_or("")).to_string();
                Splice {
                    range: body_line..b_end,
                    text: reindent_block(content, &body_indent, &unit),
                }
            }
        }
    })
}

fn line_of(text: &str, at: usize) -> u32 {
    text.get(..at).map_or(0, |s| s.matches('\n').count()) as u32 + 1
}

impl Tool for EditSymbolTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_EDIT_SYMBOL.into(),
            description: "Edit a definition by name without sending the rest of the file. action: \
                          replace (the whole definition, including its doc comments/attributes), \
                          replace_body (keep the signature), insert_before, insert_after, delete. \
                          Indentation is fixed up for you. The file is re-parsed and the edit is \
                          refused if it adds syntax errors."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string"},"symbol":{"type":"string","description":"e.g. AgentLoop::execute, UserService.create, handler"},"action":{"type":"string","enum":["replace","replace_body","insert_before","insert_after","delete"]},"content":{"type":"string","description":"The new code (not needed for delete)"},"line":{"type":"integer","minimum":1,"description":"A line inside the definition, to pick between same-named ones"}},"required":["path","symbol","action"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let get = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim);
        let (Some(path), Some(symbol), Some(action)) = (get("path"), get("symbol"), get("action"))
        else {
            return Ok(tool_error(
                "edit_symbol needs `path`, `symbol` and `action`",
            ));
        };
        let Some(action) = Action::parse(action) else {
            return Ok(tool_error(format!(
                "unknown action `{action}`; expected replace, replace_body, insert_before, \
                 insert_after or delete"
            )));
        };
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        if action != Action::Delete && content.trim().is_empty() {
            return Ok(tool_error("`content` is required for this action"));
        }
        let line = args
            .get("line")
            .and_then(Value::as_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        let Some(ix) = self.slot.get() else {
            return Ok(tool_error(
                "edit_symbol needs the code index, which is off — use str_replace_editor",
            ));
        };
        let full = resolve(&self.root, path);
        let shown = display_path(&self.root, &full);
        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(e) => return Ok(tool_error(format!("cannot read {shown}: {e}"))),
        };
        let before = match ix.parse_text(&shown, &text) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Ok(tool_error(format!(
                    "edit_symbol supports Rust, Go, TypeScript/JavaScript and Python, not \
                     {shown} — use str_replace_editor"
                )))
            }
            Err(e) => return Ok(tool_error(e.to_string())),
        };
        let def = match pick(&before, symbol, line, &shown) {
            Ok(d) => d.clone(),
            Err(e) => return Ok(tool_error(e)),
        };
        let crlf = text.contains("\r\n");
        let splice = match plan(&text, &shown, &def, action, content) {
            Ok(s) => s,
            Err(e) => return Ok(tool_error(e)),
        };
        let inserted = if crlf {
            splice.text.replace('\n', "\r\n")
        } else {
            splice.text
        };
        let (Some(head), Some(tail)) =
            (text.get(..splice.range.start), text.get(splice.range.end..))
        else {
            return Ok(tool_error(
                "internal: edit range is not on a character boundary",
            ));
        };
        let new_text = format!("{head}{inserted}{tail}");

        // FR-EDIT-04: the syntax gate.
        let mut warning = None;
        if self.syntax != SyntaxCheck::Off {
            if let Ok(Some(after)) = ix.parse_text(&shown, &new_text) {
                if after.error_nodes > before.error_nodes {
                    let at = after
                        .first_error
                        .map_or(String::new(), |(l, c)| format!(" at L{l}:{c}"));
                    let msg = format!("the edit introduces a syntax error{at}");
                    if self.syntax == SyntaxCheck::Reject {
                        return Ok(tool_error(format!(
                            "edit refused: {msg}. Nothing was written."
                        )));
                    }
                    warning = Some(format!("warning: {msg}"));
                }
            }
        }

        if let Err(e) = self.fs.write_atomic(&full, &new_text) {
            return Ok(tool_error(format!("cannot write {shown}: {e}")));
        }
        record_write(&self.log, &full, &new_text);

        // What changed, in the new file's lines.
        let first = line_of(&new_text, splice.range.start);
        let last = if inserted.is_empty() {
            first
        } else {
            line_of(&new_text, splice.range.start + inserted.trim_end().len())
        };
        let changed: Vec<(u32, u32)> = if last - first <= 2 {
            vec![(first, last)]
        } else {
            vec![(first, first), (last, last)]
        };
        let old = format!("L{}-{}", def.span.start_line, def.span.end_line);
        let mut out = match action {
            Action::Replace | Action::ReplaceBody => {
                let now = ix
                    .parse_text(&shown, &new_text)
                    .ok()
                    .flatten()
                    .and_then(|p| {
                        p.defs
                            .into_iter()
                            .filter(|d| d.qualified == def.qualified && d.kind == def.kind)
                            .min_by_key(|d| d.span.start_line.abs_diff(first))
                    })
                    .map_or(String::new(), |d| {
                        format!(" → L{}-{}", d.span.start_line, d.span.end_line)
                    });
                format!(
                    "{} {} {} in {shown}  {old}{now}\n",
                    action.past(),
                    def.kind.as_str(),
                    def.qualified
                )
            }
            _ => format!(
                "{} {} {} in {shown}  ({old})\n",
                action.past(),
                def.kind.as_str(),
                def.qualified
            ),
        };
        let seam_text = new_text.replace("\r\n", "\n");
        out.push_str(&seams(&seam_text, &changed, 1));
        if let Some(w) = warning {
            out.push_str(&w);
        }
        let out = out.trim_end().to_string();
        Ok(ToolResult::ok(&out).with_subject(Subject::FileWrite { path: shown }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    use domain::{SearchPort, ToolRegistryPort};
    use infra_index::{CodeIndex, IndexOptions};

    struct Fixture {
        dir: tempfile::TempDir,
        tool: EditSymbolTool,
        log: WriteLog,
    }

    fn fixture(files: &[(&str, &str)]) -> Fixture {
        fixture_with(files, SyntaxCheck::Reject)
    }

    fn fixture_with(files: &[(&str, &str)], mode: SyntaxCheck) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        for (p, t) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, t).unwrap();
        }
        let search: Arc<dyn SearchPort> = Arc::new(
            infra_search::RipgrepSearch::new(dir.path(), &infra_search::FilterConfig::default())
                .unwrap(),
        );
        let ix = CodeIndex::open(IndexOptions::new(dir.path()), search);
        ix.build();
        let slot = IndexSlot::default();
        let _ = slot.set(ix);
        let log = WriteLog::default();
        let tool = EditSymbolTool::new(dir.path().to_path_buf(), slot)
            .with_write_log(log.clone())
            .with_syntax_check(mode);
        Fixture { dir, tool, log }
    }

    impl Fixture {
        fn edit(&mut self, args: serde_json::Value) -> ToolResult {
            self.tool.call("", &args.to_string()).unwrap()
        }
        fn read(&self, p: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(p)).unwrap()
        }
        fn root(&self) -> &Path {
            self.dir.path()
        }
    }

    const RUST: &str = "use std::fmt;\n\n\
/// Runs things.\n\
#[derive(Debug)]\n\
pub struct Engine {\n    turns: u32,\n}\n\n\
impl Engine {\n    \
/// Make one.\n    \
#[inline]\n    \
pub fn new() -> Self {\n        Engine { turns: 0 }\n    }\n\n    \
pub fn step(&mut self) {\n        self.turns += 1;\n    }\n}\n\n\
pub fn helper() -> u32 {\n    1\n}\n";

    #[test]
    fn replace_takes_in_doc_comments_and_attributes() {
        let mut f = fixture(&[("src/lib.rs", RUST)]);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "Engine::new", "action": "replace",
            "content": "/// Make a fresh one.\npub fn new() -> Self {\n    Engine { turns: 1 }\n}"
        }));
        assert!(res.error.is_none(), "{res:?}");
        let text = f.read("src/lib.rs");
        assert!(text.contains("impl Engine {\n    /// Make a fresh one.\n    pub fn new() -> Self {\n        Engine { turns: 1 }\n    }\n\n    pub fn step"), "{text}");
        assert!(
            !text.contains("#[inline]"),
            "the attribute was part of the span"
        );
        assert!(
            res.content
                .starts_with("replaced method Engine::new in src/lib.rs  L10-14 → L10-13\n"),
            "{}",
            res.content
        );
        assert!(res.content.lines().count() < 12, "{}", res.content);
        assert_eq!(
            res.subject,
            Some(Subject::FileWrite {
                path: "src/lib.rs".into()
            })
        );
        // FR-EDIT-09: the write is logged for the index and the LSP.
        assert_eq!(f.log.lock().unwrap().len(), 1);
    }

    #[test]
    fn replace_body_keeps_the_signature_and_wraps_bare_statements() {
        let mut f = fixture(&[("src/lib.rs", RUST)]);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "step", "action": "replace_body",
            "content": "self.turns += 2;\nif self.turns > 10 {\n    self.turns = 0;\n}"
        }));
        assert!(res.error.is_none(), "{res:?}");
        let text = f.read("src/lib.rs");
        assert!(text.contains("    pub fn step(&mut self) {\n        self.turns += 2;\n        if self.turns > 10 {\n            self.turns = 0;\n        }\n    }\n}"), "{text}");
        // Braces supplied by the model are not doubled.
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "helper", "action": "replace_body",
            "content": "{\n    2\n}"
        }));
        assert!(res.error.is_none(), "{res:?}");
        assert!(f
            .read("src/lib.rs")
            .ends_with("pub fn helper() -> u32 {\n    2\n}\n"));
    }

    #[test]
    fn insert_before_and_after_leave_one_blank_line() {
        let mut f = fixture(&[("src/lib.rs", RUST)]);
        f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "helper", "action": "insert_after",
            "content": "pub fn after() {}"
        }));
        assert!(f
            .read("src/lib.rs")
            .ends_with("    1\n}\n\npub fn after() {}\n"));
        f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "Engine::step", "action": "insert_before",
            "content": "pub fn reset(&mut self) {\n    self.turns = 0;\n}"
        }));
        let text = f.read("src/lib.rs");
        assert!(text.contains("    }\n\n    pub fn reset(&mut self) {\n        self.turns = 0;\n    }\n\n    pub fn step"), "{text}");
    }

    #[test]
    fn delete_removes_the_trivia_and_one_blank_line() {
        let mut f = fixture(&[("src/lib.rs", RUST)]);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "Engine::new", "action": "delete"
        }));
        assert!(res.error.is_none(), "{res:?}");
        let text = f.read("src/lib.rs");
        assert!(text.contains("impl Engine {\n    pub fn step"), "{text}");
        assert!(!text.contains("Make one"));
    }

    #[test]
    fn go_methods_are_addressed_by_receiver() {
        let src = "package api\n\ntype Server struct {\n\taddr string\n}\n\n// Run serves.\nfunc (s *Server) Run() error {\n\treturn nil\n}\n\nfunc Run() {}\n";
        let mut f = fixture(&[("api/server.go", src)]);
        let res = f.edit(serde_json::json!({
            "path": "api/server.go", "symbol": "Server.Run", "action": "replace_body",
            "content": "if s.addr == \"\" {\n    return errEmpty\n}\nreturn nil"
        }));
        assert!(res.error.is_none(), "{res:?}");
        let text = f.read("api/server.go");
        assert!(text.contains("func (s *Server) Run() error {\n\tif s.addr == \"\" {\n\t\treturn errEmpty\n\t}\n\treturn nil\n}\n\nfunc Run() {}"), "{text}");
        // `Run` alone is ambiguous: the method and the function.
        let res = f.edit(serde_json::json!({
            "path": "api/server.go", "symbol": "Run", "action": "delete"
        }));
        let err = res.error.unwrap();
        assert!(err.contains("is ambiguous"), "{err}");
        assert!(err.contains("method Server.Run — L7-13"), "{err}");
        assert_eq!(f.read("api/server.go"), text, "nothing written");
        // …and `line` picks one.
        let res = f.edit(serde_json::json!({
            "path": "api/server.go", "symbol": "Run", "action": "delete", "line": 15
        }));
        assert!(res.error.is_none(), "{res:?}");
        assert!(f.read("api/server.go").ends_with("\treturn nil\n}\n"));
    }

    #[test]
    fn typescript_class_methods_and_jsdoc() {
        let src = "/** A service. */\nexport class UserService {\n  /** Create. */\n  create(u: User): User {\n    return u;\n  }\n}\n";
        let mut f = fixture(&[("src/users.ts", src)]);
        let res = f.edit(serde_json::json!({
            "path": "src/users.ts", "symbol": "UserService.create", "action": "replace",
            "content": "/** Create and save. */\ncreate(u: User): User {\n    save(u);\n    return u;\n}"
        }));
        assert!(res.error.is_none(), "{res:?}");
        assert_eq!(
            f.read("src/users.ts"),
            "/** A service. */\nexport class UserService {\n  /** Create and save. */\n  create(u: User): User {\n    save(u);\n    return u;\n  }\n}\n"
        );
    }

    #[test]
    fn python_blocks_and_decorators() {
        let src = "class Repo:\n    @staticmethod\n    def load(path):\n        return open(path)\n\n    def save(self):\n        pass\n";
        let mut f = fixture(&[("pkg/repo.py", src)]);
        let res = f.edit(serde_json::json!({
            "path": "pkg/repo.py", "symbol": "Repo.load", "action": "replace_body",
            "content": "with open(path) as fh:\n    return fh.read()"
        }));
        assert!(res.error.is_none(), "{res:?}");
        assert!(f.read("pkg/repo.py").contains("    def load(path):\n        with open(path) as fh:\n            return fh.read()\n\n    def save"));
        let res = f.edit(serde_json::json!({
            "path": "pkg/repo.py", "symbol": "load", "action": "delete"
        }));
        assert!(res.error.is_none(), "{res:?}");
        assert_eq!(
            f.read("pkg/repo.py"),
            "class Repo:\n    def save(self):\n        pass\n"
        );
    }

    #[test]
    fn a_missing_symbol_suggests_near_names() {
        let mut f = fixture(&[("src/lib.rs", RUST)]);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "helpr", "action": "delete"
        }));
        let err = res.error.unwrap();
        assert!(
            err.starts_with(
                "no definition named `helpr` in src/lib.rs; did you mean:\n  fn helper — L21-23"
            ),
            "{err}"
        );
    }

    #[test]
    fn the_syntax_gate_refuses_new_errors_and_writes_nothing() {
        let mut f = fixture(&[("src/lib.rs", RUST)]);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "helper", "action": "replace",
            "content": "pub fn helper() -> u32 {\n    (1\n}"
        }));
        let err = res.error.unwrap();
        assert!(
            err.starts_with("edit refused: the edit introduces a syntax error at L"),
            "{err}"
        );
        assert!(err.ends_with("Nothing was written."));
        assert_eq!(f.read("src/lib.rs"), RUST);
        assert!(f.log.lock().unwrap().is_empty());
    }

    #[test]
    fn a_file_already_broken_can_still_be_edited() {
        let broken = format!("{RUST}\nfn oops( {{\n");
        let mut f = fixture(&[("src/lib.rs", &broken)]);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "helper", "action": "replace_body", "content": "3"
        }));
        assert!(res.error.is_none(), "{res:?}");
    }

    #[test]
    fn syntax_check_warn_writes_and_says_so() {
        let mut f = fixture_with(&[("src/lib.rs", RUST)], SyntaxCheck::Warn);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "helper", "action": "replace_body", "content": "(1"
        }));
        assert!(res.error.is_none());
        assert!(
            res.content
                .contains("warning: the edit introduces a syntax error"),
            "{}",
            res.content
        );
        assert!(f.read("src/lib.rs").contains("(1"));
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let src = "fn a() {\r\n    1;\r\n}\r\n\r\nfn b() {}\r\n";
        let mut f = fixture(&[("src/lib.rs", src)]);
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "a", "action": "replace_body", "content": "2;\n3;"
        }));
        assert!(res.error.is_none(), "{res:?}");
        assert_eq!(
            f.read("src/lib.rs"),
            "fn a() {\r\n    2;\r\n    3;\r\n}\r\n\r\nfn b() {}\r\n"
        );
    }

    #[test]
    fn a_stale_index_cannot_misplace_an_edit() {
        let mut f = fixture(&[("src/lib.rs", RUST)]);
        // Changed on disk behind the index's back: ten lines pushed down.
        let shifted = format!("{}{RUST}", "// pad\n".repeat(10));
        std::fs::write(f.root().join("src/lib.rs"), &shifted).unwrap();
        let res = f.edit(serde_json::json!({
            "path": "src/lib.rs", "symbol": "helper", "action": "replace_body", "content": "7"
        }));
        assert!(res.error.is_none(), "{res:?}");
        assert!(f
            .read("src/lib.rs")
            .ends_with("pub fn helper() -> u32 {\n    7\n}\n"));
        assert!(f.read("src/lib.rs").starts_with("// pad\n"));
    }

    #[test]
    fn unsupported_languages_and_missing_bodies_are_model_errors() {
        let mut f = fixture(&[
            ("notes.lua", "function x() end\n"),
            ("src/t.rs", "pub trait T {\n    fn run(&self);\n}\n"),
        ]);
        let res = f.edit(serde_json::json!({
            "path": "notes.lua", "symbol": "x", "action": "delete"
        }));
        assert!(res.error.unwrap().contains("use str_replace_editor"));
        let res = f.edit(serde_json::json!({
            "path": "src/t.rs", "symbol": "T::run", "action": "replace_body", "content": "1"
        }));
        assert!(res.error.unwrap().contains("has no body"));
    }

    #[test]
    fn reindent_converts_flush_left_and_mixed_indentation() {
        assert_eq!(
            reindent_block("if x {\n    go();\n}", "\t", "\t"),
            "\tif x {\n\t\tgo();\n\t}"
        );
        assert_eq!(
            reindent_block("\tif x {\n\t\tgo();\n\t}\n", "  ", "  "),
            "  if x {\n    go();\n  }"
        );
        // Relative indentation survives an already-indented block.
        assert_eq!(
            reindent_block("        a\n            b\n        c", "    ", "    "),
            "    a\n        b\n    c"
        );
        assert_eq!(detect_unit("fn a() {\n  b;\n    c;\n}\n"), "  ");
        assert_eq!(detect_unit("func a() {\n\tb\n}\n"), "\t");
    }

    #[test]
    fn it_is_not_offered_without_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = infra_config::Config {
            working_dir: dir.path().to_path_buf(),
            lsp_defaults: false,
            ..Default::default()
        };
        let names = |cfg: &infra_config::Config| -> Vec<String> {
            crate::ToolRegistry::from_config(cfg)
                .unwrap()
                .list()
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        assert!(names(&cfg).contains(&TOOL_EDIT_SYMBOL.to_string()));
        cfg.index.enabled = false;
        assert!(!names(&cfg).contains(&TOOL_EDIT_SYMBOL.to_string()));
    }
}
