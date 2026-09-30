//! Language-server tools: addressing, rendering, diagnostics (FR-LSP-05..07,
//! technical plan CE-DQ14, CE-DQ21).
//!
//! * **Positions are 1-based** here, like every other zcode tool, and the
//!   column counts *characters*. LSP positions are 0-based with columns in
//!   UTF-16 code units; the conversion happens once, in this module.
//! * **A symbol can be named instead of located**: `symbol: "AgentLoop::execute"`
//!   resolves through `workspace/symbol`. Asking the model for line and column
//!   meant it had to read the file first — making the semantic tools cost
//!   *more* than reading (PRD B10).
//! * **Results are compact**: relative paths and the source line, grouped by
//!   file and capped with a footer — never `file:///…` URIs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use domain::{LspDiagnostic, LspLocation, LspPort, LspSymbolInfo, LspWorkspaceEdit};
use serde_json::Value;

use crate::render::{clip_line, count, Footer};

pub const LSP_DIAGNOSTICS: &str = "lsp__diagnostics";

/// Most locations or diagnostics one result lists.
const MAX_ITEMS: usize = 50;
/// Longest hover text kept.
const MAX_HOVER: usize = 1_500;

/// 1-based character column → 0-based UTF-16 offset on `line`.
pub fn char_col_to_utf16(line: &str, col_1: u32) -> u32 {
    line.chars()
        .take(col_1.saturating_sub(1) as usize)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// 0-based UTF-16 offset on `line` → 1-based character column.
pub fn utf16_to_char_col(line: &str, utf16: u32) -> u32 {
    let mut units = 0;
    let mut chars = 0;
    for c in line.chars() {
        if units >= utf16 {
            break;
        }
        units += c.len_utf16() as u32;
        chars += 1;
    }
    // A line shorter than the offset (unreadable, or changed since the
    // server answered): count the rest one unit per character rather than
    // snapping to column 1.
    chars + utf16.saturating_sub(units) + 1
}

/// Line `line0` (0-based) of the file at `path`, if it can be read.
fn line_of_file(path: &Path, line0: u32) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .nth(line0 as usize)
        .map(str::to_string)
}

/// `file://` URI → path.
pub fn uri_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    // Percent-decoding: servers escape spaces and a few other characters.
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

/// A path as the model should see it: relative to the project when inside it.
pub fn shown(root: &Path, path: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(&root)
        .unwrap_or(&path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Where a position-taking tool should point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 1-based line and character column.
    Position {
        path: String,
        line: u32,
        column: u32,
    },
    /// A symbol name, optionally qualified (`Type::method`, `Class.method`),
    /// optionally narrowed to a file.
    Symbol {
        symbol: String,
        path: Option<String>,
    },
}

/// Read a target from tool arguments. `character` is accepted as an alias of
/// `column` — and, like it, is 1-based.
pub fn parse_target(args: &Value) -> Result<Target, String> {
    let path = args
        .get("path")
        .or_else(|| args.get("uri"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(symbol) = args
        .get("symbol")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Target::Symbol {
            symbol: symbol.to_string(),
            path,
        });
    }
    let path = path.ok_or("give `symbol`, or `path` with `line` and `column`")?;
    let line = args
        .get("line")
        .and_then(Value::as_u64)
        .ok_or("missing `line` (1-based) — or name the `symbol` instead")?;
    let column = args
        .get("column")
        .or_else(|| args.get("character"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if line == 0 {
        return Err("`line` is 1-based; the first line is 1".into());
    }
    Ok(Target::Position {
        path,
        line: line as u32,
        column: column.max(1) as u32,
    })
}

/// Split `a::b::c` or `a.b.c` into (container, name).
fn split_qualified(symbol: &str) -> (Option<&str>, &str) {
    let sep = if symbol.contains("::") { "::" } else { "." };
    match symbol.rsplit_once(sep) {
        Some((container, name)) if !name.is_empty() => (Some(container), name),
        _ => (None, symbol),
    }
}

/// The position of the symbol's *name* inside its declaration range — where
/// servers expect the cursor (CE-DQ21). Falls back to the range start.
fn name_position(sym: &LspSymbolInfo) -> (u32, u32) {
    let path = uri_path(&sym.location.uri);
    let start = sym.location.range.start.clone();
    let end_line = sym.location.range.end.line.max(start.line);
    if let Ok(text) = std::fs::read_to_string(&path) {
        for (i, line) in text
            .lines()
            .enumerate()
            .skip(start.line as usize)
            .take((end_line - start.line + 1) as usize)
        {
            let mut from = 0;
            while let Some(found) = line[from..].find(&sym.name) {
                let at = from + found;
                let before = line[..at].chars().next_back();
                let after = line[at + sym.name.len()..].chars().next();
                let boundary =
                    |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                if boundary(before) && boundary(after) {
                    let col16: u32 = line[..at].chars().map(|c| c.len_utf16() as u32).sum();
                    return (i as u32, col16);
                }
                from = at + sym.name.len();
            }
        }
    }
    (start.line, start.character)
}

/// Resolve a target to (uri, 0-based line, 0-based UTF-16 column), or an
/// error the model can act on — a list of candidates when ambiguous.
pub fn resolve(
    port: &mut dyn LspPort,
    root: &Path,
    target: &Target,
    uri_for: &dyn Fn(&str) -> String,
) -> Result<(String, u32, u32), String> {
    match target {
        Target::Position { path, line, column } => {
            let uri = uri_for(path);
            let text = line_of_file(&uri_path(&uri), line - 1).unwrap_or_default();
            Ok((uri, line - 1, char_col_to_utf16(&text, *column)))
        }
        Target::Symbol { symbol, path } => {
            let (container, name) = split_qualified(symbol);
            let found = port
                .workspace_symbols(name)
                .map_err(|e| format!("could not look up `{symbol}`: {e}"))?;
            let wanted_path = path.as_ref().map(|p| uri_path(&uri_for(p)));
            let mut matches: Vec<&LspSymbolInfo> = found
                .iter()
                .filter(|s| s.name == name)
                .filter(|s| match container {
                    Some(c) => s.container.as_deref().is_some_and(|sc| {
                        sc == c
                            || sc.ends_with(&format!("::{c}"))
                            || sc.ends_with(&format!(".{c}"))
                            || sc.contains(c)
                    }),
                    None => true,
                })
                .filter(|s| {
                    wanted_path.as_ref().is_none_or(|w| {
                        let p = uri_path(&s.location.uri);
                        p.canonicalize().unwrap_or(p) == w.canonicalize().unwrap_or(w.clone())
                    })
                })
                .collect();
            matches.sort_by(|a, b| {
                (&a.location.uri, a.location.range.start.line)
                    .cmp(&(&b.location.uri, b.location.range.start.line))
            });
            match matches.as_slice() {
                [] => Err(format!(
                    "no symbol named `{symbol}` — check the spelling, or find it with grep or \
                     symbols first"
                )),
                [one] => {
                    let (line, col) = name_position(one);
                    Ok((one.location.uri.clone(), line, col))
                }
                many => {
                    let list: Vec<String> = many
                        .iter()
                        .take(10)
                        .map(|s| {
                            format!(
                                "  {}{} — {}:{}",
                                s.container
                                    .as_deref()
                                    .map(|c| format!("{c}::"))
                                    .unwrap_or_default(),
                                s.name,
                                shown(root, &uri_path(&s.location.uri)),
                                s.location.range.start.line + 1
                            )
                        })
                        .collect();
                    Err(format!(
                        "`{symbol}` is ambiguous ({} matches) — qualify it or add `path`:\n{}",
                        many.len(),
                        list.join("\n")
                    ))
                }
            }
        }
    }
}

/// `path:line:col  source line` for one location (1-based, char columns).
fn location_line(root: &Path, loc: &LspLocation) -> (String, u32, String) {
    let path = uri_path(&loc.uri);
    let text = line_of_file(&path, loc.range.start.line).unwrap_or_default();
    let col = utf16_to_char_col(&text, loc.range.start.character);
    let src = clip_line(text.trim(), 160, 0);
    (
        shown(root, &path),
        loc.range.start.line + 1,
        format!("{}:{col}  {src}", loc.range.start.line + 1),
    )
}

pub fn render_definition(root: &Path, loc: &LspLocation) -> String {
    let (path, _, rest) = location_line(root, loc);
    format!("{path}:{rest}")
}

/// References grouped by file, in path/line order, capped (FR-LSP-06).
pub fn render_locations(root: &Path, locs: &[LspLocation], what: &'static str) -> String {
    if locs.is_empty() {
        return format!("no {what} found");
    }
    let mut by_file: BTreeMap<String, Vec<(u32, String)>> = BTreeMap::new();
    for loc in locs {
        let (path, line, rest) = location_line(root, loc);
        by_file.entry(path).or_default().push((line, rest));
    }
    let mut out = String::new();
    let mut shown_n = 0;
    'outer: for (path, mut rows) in by_file {
        rows.sort();
        out.push_str(&path);
        out.push('\n');
        for (_, row) in rows {
            if shown_n == MAX_ITEMS {
                break 'outer;
            }
            out.push_str("  ");
            out.push_str(&row);
            out.push('\n');
            shown_n += 1;
        }
    }
    out.push_str(
        &Footer {
            shown: shown_n,
            total: locs.len(),
            unit: what,
            next_offset: None,
            hint: "narrow with `path`",
            notes: Vec::new(),
        }
        .render(),
    );
    out
}

/// Hover text without markdown fences or repeated blank lines, capped.
pub fn clean_hover(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        let t = line.trim_end();
        if t.starts_with("```") {
            continue;
        }
        if t.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        if out.last() == Some(&t) {
            continue;
        }
        out.push(t);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let joined = out.join("\n");
    if joined.chars().count() > MAX_HOVER {
        let head: String = joined.chars().take(MAX_HOVER).collect();
        format!("{head}…[hover clipped]")
    } else if joined.is_empty() {
        "no hover information".into()
    } else {
        joined
    }
}

/// What a rename would change, grouped by file (advice mode).
pub fn render_rename(root: &Path, edit: &LspWorkspaceEdit) -> String {
    if edit.changes.is_empty() {
        return "no edits proposed".into();
    }
    let mut by_file: BTreeMap<String, usize> = BTreeMap::new();
    for c in edit.changes.iter() {
        *by_file.entry(shown(root, &uri_path(&c.uri))).or_default() += 1;
    }
    let mut out = format!(
        "would edit {} in {}:\n",
        count(edit.changes.len() as u64, "location", "locations"),
        count(by_file.len() as u64, "file", "files")
    );
    for (path, n) in by_file.iter().take(30) {
        out.push_str(&format!("  {path}  ({n})\n"));
    }
    out.push_str("apply with str_replace_editor, or rerun with apply: true");
    out
}

fn severity_name(s: u8) -> &'static str {
    match s {
        1 => "error",
        2 => "warning",
        3 => "info",
        _ => "hint",
    }
}

/// Diagnostics as `path:line:col  error[CODE]  message`, filtered to at most
/// `max_severity` (1 = errors only), sorted, capped (FR-LSP-07).
pub fn render_diagnostics(
    root: &Path,
    diags: &[LspDiagnostic],
    max_severity: u8,
    scope: &str,
) -> String {
    let kept: Vec<&LspDiagnostic> = diags
        .iter()
        .filter(|d| d.severity <= max_severity)
        .collect();
    let what = match max_severity {
        1 => "errors",
        2 => "errors or warnings",
        _ => "diagnostics",
    };
    if kept.is_empty() {
        return format!("no {what} in {scope}");
    }
    let mut out = String::new();
    for d in kept.iter().take(MAX_ITEMS) {
        let path = uri_path(&d.uri);
        let text = line_of_file(&path, d.range.start.line).unwrap_or_default();
        let col = utf16_to_char_col(&text, d.range.start.character);
        let code = d
            .code
            .as_deref()
            .map(|c| format!("[{c}]"))
            .unwrap_or_default();
        let message = d.message.lines().next().unwrap_or_default();
        out.push_str(&format!(
            "{}:{}:{col}  {}{code}  {}\n",
            shown(root, &path),
            d.range.start.line + 1,
            severity_name(d.severity),
            clip_line(message, 200, 0)
        ));
    }
    out.push_str(
        &Footer {
            shown: kept.len().min(MAX_ITEMS),
            total: kept.len(),
            unit: what,
            next_offset: None,
            hint: "fix these first, or narrow with `path`",
            notes: Vec::new(),
        }
        .render(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{LspPosition, LspRange};

    #[test]
    fn columns_convert_between_characters_and_utf16() {
        let line = "let 𝒳 = é + x;";
        // `x` after `+ ` is the 13th character; 𝒳 is two UTF-16 units.
        let col16 = char_col_to_utf16(line, 13);
        assert_eq!(col16, 13);
        assert_eq!(utf16_to_char_col(line, col16), 13);
        assert_eq!(char_col_to_utf16("abc", 1), 0);
        assert_eq!(utf16_to_char_col("abc", 0), 1);
        assert_eq!(
            utf16_to_char_col("", 3),
            4,
            "no line text: assume one unit per char"
        );
    }

    #[test]
    fn targets_are_one_based_and_symbols_win() {
        let t =
            parse_target(&serde_json::json!({ "path": "a.rs", "line": 3, "column": 5 })).unwrap();
        assert_eq!(
            t,
            Target::Position {
                path: "a.rs".into(),
                line: 3,
                column: 5
            }
        );
        let alias = parse_target(&serde_json::json!({ "path": "a.rs", "line": 3, "character": 5 }))
            .unwrap();
        assert_eq!(alias, t, "`character` is an alias of the 1-based column");
        let sym = parse_target(&serde_json::json!({ "symbol": "App::run", "line": 9 })).unwrap();
        assert_eq!(
            sym,
            Target::Symbol {
                symbol: "App::run".into(),
                path: None
            }
        );
        assert!(parse_target(&serde_json::json!({ "path": "a.rs", "line": 0 })).is_err());
        assert!(parse_target(&serde_json::json!({ "path": "a.rs" }))
            .unwrap_err()
            .contains("symbol"));
    }

    #[test]
    fn hover_is_cleaned_and_capped() {
        let raw = "```rust\nfn foo()\n```\n\n\n\nDoes foo.\nDoes foo.\n";
        assert_eq!(clean_hover(raw), "fn foo()\n\nDoes foo.");
        assert!(clean_hover(&"x".repeat(3_000)).ends_with("…[hover clipped]"));
        assert_eq!(clean_hover("```\n```"), "no hover information");
    }

    fn loc(uri: &str, line: u32, ch: u32) -> LspLocation {
        let p = LspPosition {
            line,
            character: ch,
        };
        LspLocation {
            uri: uri.into(),
            range: LspRange {
                start: p.clone(),
                end: p,
            },
        }
    }

    #[test]
    fn locations_render_relative_grouped_and_sorted_with_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {\n    foo();\n}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "foo();\n").unwrap();
        let uri = |n: &str| format!("file://{}", dir.path().join(n).display());
        let text = render_locations(
            dir.path(),
            &[loc(&uri("b.rs"), 0, 0), loc(&uri("a.rs"), 1, 4)],
            "references",
        );
        assert_eq!(
            text,
            "a.rs\n  2:5  foo();\nb.rs\n  1:1  foo();\n[2 references]"
        );
        assert_eq!(
            render_locations(dir.path(), &[], "references"),
            "no references found"
        );
    }

    #[test]
    fn diagnostics_filter_by_severity_and_render_one_based() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn main() {\n    let x: u8 = \"s\";\n}\n",
        )
        .unwrap();
        let uri = format!("file://{}", dir.path().join("a.rs").display());
        let d = |line, severity, msg: &str| LspDiagnostic {
            uri: uri.clone(),
            range: LspRange {
                start: LspPosition {
                    line,
                    character: 16,
                },
                end: LspPosition {
                    line,
                    character: 19,
                },
            },
            severity,
            code: Some("E0308".into()),
            message: msg.into(),
        };
        let diags = [
            d(1, 1, "mismatched types\nexpected u8"),
            d(1, 2, "unused variable"),
        ];
        assert_eq!(
            render_diagnostics(dir.path(), &diags, 1, "a.rs"),
            "a.rs:2:17  error[E0308]  mismatched types\n[1 errors]"
        );
        assert!(render_diagnostics(dir.path(), &diags, 2, "a.rs").contains("warning[E0308]"));
        assert_eq!(
            render_diagnostics(dir.path(), &[], 1, "a.rs"),
            "no errors in a.rs"
        );
    }

    #[test]
    fn uris_are_percent_decoded() {
        assert_eq!(
            uri_path("file:///tmp/my%20dir/a.rs"),
            PathBuf::from("/tmp/my dir/a.rs")
        );
    }
}
