//! Native, in-process tools (FR-TOOL-FS-01/02/03, FR-TOOL-SHELL-01, FR-OUTPUT-09).
//!
//! Every edit happens in-process through `StdFs` — never by shelling out to
//! `sed`/`awk` — so file edits work identically on every platform and are not
//! subject to the shell allowlist.
//!
//! Convention: a failure the model can recover from (missing file, bad
//! arguments, blocked command) comes back as `ToolResult { error: Some(..) }`
//! so the engine can feed it to the LLM and let it retry. Only genuine
//! infrastructure failures return `Err`, which aborts the run.

use std::path::{Path, PathBuf};

use domain::{BoxError, ShellCommand, Subject, Tool, ToolResult, ToolSpec};
use infra_filesystem::StdFs;
use serde_json::Value;

use crate::guard::GuardedShell;

pub const TOOL_READ: &str = "read";
pub const TOOL_WRITE: &str = "write";
pub const TOOL_STR_REPLACE: &str = "str_replace_editor";
pub const TOOL_APPLY_PATCH: &str = "apply_patch";
pub const TOOL_LIST_DIR: &str = "list_dir";
pub const TOOL_SHELL: &str = "shell";
/// Wire name for the skill tool. The PRD spells it `zcode:skill`, but provider
/// function-calling APIs only accept `[A-Za-z0-9_-]`, so `zcode_skill` is the
/// canonical name and `zcode:skill` is accepted as an alias by the registry.
pub const TOOL_SKILL: &str = "zcode_skill";

/// A model-visible failure (as opposed to an engine-level one).
pub(crate) fn tool_error(message: impl Into<String>) -> ToolResult {
    ToolResult {
        tool_call_id: String::new(),
        content: String::new(),
        error: Some(message.into()),
        subject: None,
    }
}

pub(crate) fn parse_args(args_json: &str) -> Result<Value, ToolResult> {
    if args_json.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let strict = match serde_json::from_str::<Value>(args_json) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    // Smaller models slip on JSON syntax in ways whose meaning is not in
    // doubt — `{'pattern': '*.rs'}`, `{"pattern": **/*.rs}`, a trailing
    // comma. Refusing them costs a round trip and, since the model cannot
    // see what it sent, often a second identical failure.
    if let Some(v) = repair_json(args_json)
        .and_then(|fixed| serde_json::from_str::<Value>(&fixed).ok())
        .filter(Value::is_object)
    {
        return Ok(v);
    }
    let sent: String = args_json.chars().take(200).collect();
    let more = if args_json.chars().count() > 200 {
        "…"
    } else {
        ""
    };
    Err(tool_error(format!(
        "arguments must be a JSON object: {strict}. You sent: {sent}{more} — quote every \
         string with double quotes, e.g. {{\"pattern\": \"**/*.rs\"}}"
    )))
}

/// Rewrite common, unambiguous JSON slips into JSON: single-quoted strings,
/// unquoted keys, bare unquoted values (`**/*.rs`) and trailing commas.
/// `None` when the text has no slip this knows how to mend.
fn repair_json(text: &str) -> Option<String> {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut changed = false;
    let mut i = 0;
    // What the last significant character was, to tell keys from values.
    let mut prev = ' ';
    let quote = |s: &str, out: &mut String| {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                c => out.push(c),
            }
        }
        out.push('"');
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                // A proper string: copy it through, escapes and all.
                out.push(c);
                i += 1;
                while i < chars.len() {
                    let d = chars[i];
                    out.push(d);
                    i += 1;
                    if d == '\\' && i < chars.len() {
                        out.push(chars[i]);
                        i += 1;
                    } else if d == '"' {
                        break;
                    }
                }
                prev = '"';
            }
            '\'' => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    s.push(chars[i]);
                    i += 1;
                }
                i += 1; // closing quote
                quote(&s, &mut out);
                changed = true;
                prev = '"';
            }
            ',' => {
                // A trailing comma: drop it.
                let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
                if matches!(next, Some('}') | Some(']')) {
                    changed = true;
                } else {
                    out.push(c);
                    prev = c;
                }
                i += 1;
            }
            c if c.is_whitespace() => {
                out.push(c);
                i += 1;
            }
            '{' | '}' | '[' | ']' | ':' => {
                out.push(c);
                prev = c;
                i += 1;
            }
            _ => {
                // A bare token: a number or literal stays; a key or any
                // other value is quoted. It runs to the next delimiter.
                let in_key = matches!(prev, '{' | ',')
                    && chars[i..].iter().position(|&d| d == ':').is_some_and(|p| {
                        !chars[i..i + p].iter().any(|d| matches!(d, ',' | '}' | '"'))
                    });
                let end = chars[i..]
                    .iter()
                    .position(|&d| {
                        if in_key {
                            d == ':'
                        } else {
                            matches!(d, ',' | '}' | ']')
                        }
                    })
                    .map_or(chars.len(), |p| i + p);
                let token: String = chars[i..end].iter().collect();
                let token = token.trim_end();
                let literal = matches!(token, "true" | "false" | "null")
                    || serde_json::from_str::<serde_json::Number>(token).is_ok();
                if literal && !in_key {
                    out.push_str(token);
                } else {
                    quote(token, &mut out);
                    changed = true;
                }
                prev = '"';
                i = end;
            }
        }
    }
    changed.then_some(out)
}

fn str_arg(args: &Value, key: &str) -> Result<String, ToolResult> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| tool_error(format!("missing required string argument `{key}`")))
}

/// Render a path for humans and for the model: relative to the working
/// directory when it is inside it, so tool output stays short and readable
/// instead of repeating a long absolute prefix on every line.
pub(crate) fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Resolve a model-supplied path against the working directory so relative
/// paths mean what the user expects.
pub(crate) fn resolve(root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        root.join(p)
    }
}

// ---------------------------------------------------------------------------
// read (FR-READ-01..04)
// ---------------------------------------------------------------------------

/// Lines a `read` with no range returns (config `read.default_limit`).
pub const DEFAULT_READ_LIMIT: u32 = 400;
/// Outline lines the large-file guard appends at most (FR-READ-02).
const READ_GUARD_OUTLINE: usize = 40;
/// Longest line shown whole (FR-READ-04).
const MAX_LINE_CHARS: usize = 2_000;

/// `12345` → `12,345`, for footers a person reads too.
pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn clip_long_line(line: &str) -> std::borrow::Cow<'_, str> {
    let chars = line.chars().count();
    if chars <= MAX_LINE_CHARS {
        return std::borrow::Cow::Borrowed(line);
    }
    let kept: String = line.chars().take(MAX_LINE_CHARS).collect();
    std::borrow::Cow::Owned(format!("{kept}…[+{} chars]", chars - MAX_LINE_CHARS))
}

/// What a read of a range returns: a 1-based gutter on every line, so the
/// model can address `offset`, LSP positions and edit hints without
/// counting (FR-READ-03), and a footer whenever the file goes on.
///
/// `offset`/`limit` are `None` when the model did not ask for a range; a
/// file longer than `default_limit` then returns its head with a footer
/// saying how to get the rest (FR-READ-02) instead of all of it.
pub(crate) fn read_excerpt(
    root: &Path,
    full: &Path,
    offset: Option<u32>,
    limit: Option<u32>,
    default_limit: u32,
    rest_outline: Option<&dyn Fn(u32) -> Option<String>>,
) -> ToolResult {
    let shown = display_path(root, full);
    let bytes = match std::fs::read(full) {
        Ok(b) => b,
        Err(e) => return tool_error(format!("cannot read {shown}: {e}")),
    };
    // A NUL in the first 8 KiB is binary, as ripgrep decides it.
    if bytes.iter().take(8 * 1024).any(|b| *b == 0) {
        let kind = full
            .extension()
            .and_then(|e| e.to_str())
            .map_or("binary".to_string(), |e| format!(".{e} binary"));
        return tool_error(format!(
            "{shown} is a {kind} file ({} bytes) — not shown",
            thousands(bytes.len())
        ));
    }
    let hash = crate::render::fnv64(&bytes);
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return ToolResult::ok("(empty file)").with_subject(Subject::FileRange {
            path: shown,
            start: 0,
            end: 0,
            hash,
        });
    }
    let start = offset.unwrap_or(1).max(1) as usize;
    if start > total {
        return tool_error(format!(
            "offset {start} is past the end of {shown} ({} lines)",
            thousands(total)
        ));
    }
    let explicit = offset.is_some() || limit.is_some();
    let limit = limit.unwrap_or(default_limit).max(1) as usize;
    let end = (start + limit - 1).min(total);
    let width = crate::render::digits(end as u32);
    let mut out = String::with_capacity((end - start + 1) * 48);
    for (i, line) in lines[start - 1..end].iter().enumerate() {
        out.push_str(&crate::render::gutter((start + i) as u32, width, false));
        out.push_str(&clip_long_line(line));
        out.push('\n');
    }
    if end < total {
        if explicit {
            out.push_str(&format!(
                "[lines {start}-{end} of {}; next: offset {}]",
                thousands(total),
                end + 1
            ));
        } else {
            // FR-READ-02: the guard fired. What follows is described, not
            // shown, so the next call can be a targeted range.
            match rest_outline.and_then(|f| f(end as u32)) {
                Some(outline) => {
                    out.push_str(&format!(
                        "[lines 1-{end} of {} — read the part you need with offset/limit]\n",
                        thousands(total)
                    ));
                    out.push_str(&outline);
                }
                None => out.push_str(&format!(
                    "[lines 1-{end} of {} — use offset/limit, or grep to find the part you need]",
                    thousands(total)
                )),
            }
        }
    } else if out.ends_with('\n') {
        out.pop();
    }
    ToolResult::ok(&out).with_subject(Subject::FileRange {
        path: shown,
        start: start as u32,
        end: end as u32,
        hash,
    })
}

pub struct ReadTool {
    root: PathBuf,
    default_limit: u32,
    index: Option<crate::index_tools::IndexSlot>,
}

impl ReadTool {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            default_limit: DEFAULT_READ_LIMIT,
            index: None,
        }
    }

    /// Append an outline of the unshown rest when the large-file guard
    /// fires (FR-READ-02).
    pub fn with_index(mut self, slot: crate::index_tools::IndexSlot) -> Self {
        self.index = Some(slot);
        self
    }

    /// Lines returned when no range is given (`read.default_limit`).
    pub fn with_default_limit(mut self, limit: u32) -> Self {
        self.default_limit = limit.max(1);
        self
    }
}

fn u32_arg(args: &Value, key: &str) -> Option<u32> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_READ.into(),
            description: "Read a text file with 1-based line numbers. Use offset/limit to read \
                          just the lines you need; a long file returns its first lines and a \
                          note on how to get the rest."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"File path, absolute or relative to the working directory"},"offset":{"type":"integer","minimum":1,"description":"First line to return (1-based)"},"limit":{"type":"integer","minimum":1,"description":"Number of lines"}},"required":["path"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let path = match str_arg(&args, "path") {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let full = resolve(&self.root, &path);
        let index = self.index.as_ref().and_then(|slot| slot.get());
        let rest = |after: u32| {
            index.and_then(|ix| {
                crate::index_tools::outline_after(ix.as_ref(), &full, after, READ_GUARD_OUTLINE)
            })
        };
        Ok(read_excerpt(
            &self.root,
            &full,
            u32_arg(&args, "offset"),
            u32_arg(&args, "limit"),
            self.default_limit,
            Some(&rest),
        ))
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

/// Past this many lines, rewriting a whole file to change a few is wasteful
/// enough to say so (FR-EDIT-08).
const REWRITE_HINT_LINES: usize = 200;

/// Write `content` to `full` and describe the change without echoing it
/// (FR-EDIT-08): `created x (57 lines)` or `wrote x: 412 → 430 lines (+31 −13)`.
pub(crate) fn write_file(
    root: &Path,
    fs: &StdFs,
    log: &crate::edit::WriteLog,
    full: &Path,
    content: &str,
) -> ToolResult {
    let shown = display_path(root, full);
    let before = std::fs::read_to_string(full).ok();
    if let Err(e) = fs.write_atomic(full, content) {
        return tool_error(format!("cannot write {shown}: {e}"));
    }
    crate::edit::record_write(log, full, content);
    let lines = content.lines().count();
    let summary = match before {
        None => format!("created {shown} ({lines} lines)"),
        Some(old) => {
            let old_lines = old.lines().count();
            let mut s = match crate::edit::line_diffstat(&old, content) {
                Some((added, removed)) => {
                    format!("wrote {shown}: {old_lines} → {lines} lines (+{added} −{removed})")
                }
                None => format!("wrote {shown}: {old_lines} → {lines} lines"),
            };
            if old_lines > REWRITE_HINT_LINES {
                s.push_str(
                    "\ntip: for a small change use str_replace_editor — it sends only the \
                     lines that change",
                );
            }
            s
        }
    };
    ToolResult::ok(&summary).with_subject(Subject::FileWrite { path: shown })
}

pub struct WriteTool {
    root: PathBuf,
    fs: StdFs,
    log: crate::edit::WriteLog,
}

impl WriteTool {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            fs: StdFs::new(),
            log: crate::edit::WriteLog::default(),
        }
    }

    /// Record writes where the registry can see them (FR-EDIT-09).
    pub fn with_write_log(mut self, log: crate::edit::WriteLog) -> Self {
        self.log = log;
        self
    }
}

impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_WRITE.into(),
            description: "Create a file, or overwrite one whole (atomic). To change part of an \
                          existing file, use str_replace_editor instead."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let path = match str_arg(&args, "path") {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        // Some models emit `file_text` (Anthropic editor convention) instead.
        let content = args
            .get("content")
            .or_else(|| args.get("file_text"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let full = resolve(&self.root, &path);
        Ok(write_file(&self.root, &self.fs, &self.log, &full, &content))
    }
}

// ---------------------------------------------------------------------------
// str_replace_editor
// ---------------------------------------------------------------------------

/// The OpenCode/Anthropic-style editor tool: one entry point with a `command`
/// discriminator (`view` | `create` | `str_replace` | `list_dir`).
pub struct StrReplaceTool {
    root: PathBuf,
    fs: StdFs,
    /// For the `list_dir` command, which shares `list_dir`'s filtered tree.
    search: crate::search_tools::Search,
    log: crate::edit::WriteLog,
}

impl StrReplaceTool {
    pub fn new(root: PathBuf) -> Self {
        let search = crate::search_tools::default_search(&root);
        Self {
            root,
            fs: StdFs::new(),
            search,
            log: crate::edit::WriteLog::default(),
        }
    }

    /// Record writes where the registry can see them (FR-EDIT-09).
    pub fn with_write_log(mut self, log: crate::edit::WriteLog) -> Self {
        self.log = log;
        self
    }

    /// Share the registry's search service (and so its configured filter).
    pub fn with_search(mut self, search: crate::search_tools::Search) -> Self {
        self.search = search;
        self
    }

    /// Replace `old` with `new` in `full` (FR-EDIT-06/07).
    ///
    /// Exactly one match, or every match with `replace_all`. Several matches
    /// otherwise is an error listing their lines — v0.6 edited the first and
    /// only mentioned the count afterwards, so an ambiguous edit could land
    /// in the wrong place. No exact match: one retry that tolerates
    /// indentation and trailing-whitespace drift, disclosed in the result;
    /// failing that, the closest region, so the retry needs no re-read.
    fn str_replace(
        &self,
        full: &Path,
        old: &str,
        new: &str,
        replace_all: bool,
        expected: Option<usize>,
    ) -> ToolResult {
        let shown = display_path(&self.root, full);
        if old.is_empty() {
            return tool_error("`old_str` must not be empty");
        }
        let content = match domain::FileSystemPort::read(&self.fs, full) {
            Ok(c) => c,
            Err(e) => return tool_error(format!("cannot read {shown}: {e}")),
        };
        // (start, end, replacement) in file order.
        let mut edits: Vec<(usize, usize, String)> = content
            .match_indices(old)
            .map(|(at, m)| (at, at + m.len(), new.to_string()))
            .collect();
        let mut note = "";
        if edits.is_empty() {
            let found = crate::edit::find_normalised(&content, old);
            if found.is_empty() {
                return tool_error(match crate::edit::closest_region(&content, old) {
                    Some((from, to, score, of, text)) => format!(
                        "old_str not found in {shown}. Closest match ({score} of {of} lines \
                         equal), lines {from}-{to}:\n{text}Copy the exact text from here, or \
                         read a wider range."
                    ),
                    None => format!(
                        "old_str not found in {shown}, and no region resembles it — read the \
                         part of the file you mean to change first"
                    ),
                });
            }
            edits = found
                .into_iter()
                .map(|m| {
                    let replacement = crate::edit::reindent(
                        new.trim_end_matches('\n'),
                        &m.old_indent,
                        &m.file_indent,
                    );
                    (m.start, m.end, replacement)
                })
                .collect();
            note = " (matched with whitespace normalisation)";
        }
        if edits.len() > 1 && !replace_all {
            let lines: Vec<String> = edits
                .iter()
                .take(10)
                .map(|(at, _, _)| crate::edit::line_of(&content, *at).to_string())
                .collect();
            return tool_error(format!(
                "old_str matches {} places in {shown} (lines {}{}) — include surrounding lines \
                 to make it unique, or set replace_all",
                edits.len(),
                lines.join(", "),
                if edits.len() > 10 { ", …" } else { "" }
            ));
        }
        if let Some(n) = expected {
            if n != edits.len() {
                return tool_error(format!(
                    "expected {n} replacement(s) in {shown}, found {}; nothing was written",
                    edits.len()
                ));
            }
        }
        // Apply back to front so earlier offsets stay valid; record where
        // each replacement lands in the new text for the seams.
        let mut updated = content.clone();
        for (start, end, replacement) in edits.iter().rev() {
            updated.replace_range(*start..*end, replacement);
        }
        let mut changed = Vec::with_capacity(edits.len());
        let mut shift: isize = 0;
        for (start, end, replacement) in &edits {
            let new_start = (*start as isize + shift) as usize;
            let first = crate::edit::line_of(&updated, new_start);
            let last = first + replacement.matches('\n').count() as u32;
            changed.push((first, last));
            shift += replacement.len() as isize - (*end - *start) as isize;
        }
        if let Err(e) = self.fs.write_atomic(full, &updated) {
            return tool_error(format!("cannot write {shown}: {e}"));
        }
        crate::edit::record_write(&self.log, full, &updated);
        let (first, last) = (changed[0].0, changed[changed.len() - 1].1);
        let head = if edits.len() == 1 {
            format!(
                "edited {shown}: 1 replacement at line {first}{}{note}",
                if last > first {
                    format!("-{last}")
                } else {
                    String::new()
                }
            )
        } else {
            format!("edited {shown}: {} replacements{note}", edits.len())
        };
        ToolResult::ok(&format!(
            "{head}\n{}",
            crate::edit::seams(&updated, &changed, 2)
        ))
        .with_subject(Subject::FileWrite { path: shown })
    }
}

impl Tool for StrReplaceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_STR_REPLACE.into(),
            description: "Edit files in place. `view` shows lines (view_range), `create` writes \
                          a file, `str_replace` swaps old_str for new_str — old_str must match \
                          one place unless replace_all is set — `list_dir` lists a directory."
                .into(),
            params_json: r#"{"type":"object","properties":{"command":{"type":"string","enum":["view","create","str_replace","list_dir"]},"path":{"type":"string"},"view_range":{"type":"array","items":{"type":"integer"},"description":"[start, end] lines, 1-based; end -1 = EOF"},"old_str":{"type":"string"},"new_str":{"type":"string"},"replace_all":{"type":"boolean","description":"Replace every match; otherwise several matches is an error"},"expected_replacements":{"type":"integer"},"file_text":{"type":"string"}},"required":["command","path"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let command = match str_arg(&args, "command") {
            Ok(c) => c,
            Err(e) => return Ok(e),
        };
        let path = match str_arg(&args, "path") {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let full = resolve(&self.root, &path);

        let result = match command.as_str() {
            "view" => {
                // `view_range: [start, end]`, the Anthropic editor convention;
                // `end = -1` means to the end of the file.
                let range = args.get("view_range").and_then(Value::as_array);
                let (offset, limit) =
                    match range.map(|r| {
                        (
                            r.first().and_then(Value::as_i64),
                            r.get(1).and_then(Value::as_i64),
                        )
                    }) {
                        Some((Some(a), Some(b))) if a >= 1 && (b == -1 || b >= a) => (
                            Some(a as u32),
                            (b != -1).then(|| (b - a + 1) as u32).or(Some(u32::MAX)),
                        ),
                        Some(_) => return Ok(tool_error(
                            "view_range must be [start, end] with 1 <= start <= end, or end = -1",
                        )),
                        None => (None, None),
                    };
                read_excerpt(&self.root, &full, offset, limit, DEFAULT_READ_LIMIT, None)
            }
            "create" => {
                let text = args
                    .get("file_text")
                    .or_else(|| args.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                write_file(&self.root, &self.fs, &self.log, &full, text)
            }
            "str_replace" => {
                let old = match str_arg(&args, "old_str") {
                    Ok(o) => o,
                    Err(e) => return Ok(e),
                };
                let new = args
                    .get("new_str")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let replace_all = args
                    .get("replace_all")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let expected = args
                    .get("expected_replacements")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize);
                self.str_replace(&full, &old, new, replace_all, expected)
            }
            "list_dir" => {
                match crate::search_tools::list_tree(self.search.as_ref(), &self.root, &full, 1) {
                    Ok(listing) => ToolResult::ok(&listing),
                    Err(e) => tool_error(format!(
                        "cannot list {}: {e}",
                        display_path(&self.root, &full)
                    )),
                }
            }
            other => tool_error(format!(
                "unknown command `{other}`; expected view|create|str_replace|list_dir"
            )),
        };
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// apply_patch
// ---------------------------------------------------------------------------

/// Applies a unified diff across one or more files in a single call — the
/// efficient way for a model to make several related edits, and the only tool
/// that can create and delete files in one shot.
pub struct ApplyPatchTool {
    root: PathBuf,
    log: crate::edit::WriteLog,
}

impl ApplyPatchTool {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            log: crate::edit::WriteLog::default(),
        }
    }

    /// Record writes where the registry can see them (FR-EDIT-09).
    pub fn with_write_log(mut self, log: crate::edit::WriteLog) -> Self {
        self.log = log;
        self
    }
}

impl Tool for ApplyPatchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_APPLY_PATCH.into(),
            description: "Apply a unified diff to the working tree. Supports multiple files, \
                          creation (`--- /dev/null`) and deletion (`+++ /dev/null`). Line \
                          numbers need not be exact — hunks are located by their context. \
                          Nothing is written unless every hunk applies."
                .into(),
            params_json: r#"{"type":"object","properties":{"patch":{"type":"string","description":"Unified diff text, e.g. `--- a/f.rs`, `+++ b/f.rs`, `@@ -1,3 +1,3 @@`"}},"required":["patch"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        // Accept a couple of aliases: models reach for `diff` and `content`.
        let patch = args
            .get("patch")
            .or_else(|| args.get("diff"))
            .or_else(|| args.get("content"))
            .and_then(|v| v.as_str());
        let Some(patch) = patch else {
            return Ok(tool_error("missing required string argument `patch`"));
        };

        match crate::patch::apply_patch(&self.root, patch) {
            Ok(applied) => {
                // FR-EDIT-08: a per-file diffstat, never the patched content.
                let mut summary = format!("patched {} file(s):\n", applied.len());
                let mut paths = Vec::with_capacity(applied.len());
                for file in &applied {
                    let shown = display_path(&self.root, &file.path);
                    let line = match file.action {
                        crate::patch::PatchAction::Create => format!("A {shown} +{}", file.added),
                        crate::patch::PatchAction::Delete => format!("D {shown}"),
                        crate::patch::PatchAction::Modify => {
                            format!("M {shown} +{} −{}", file.added, file.removed)
                        }
                    };
                    summary.push_str(&line);
                    summary.push('\n');
                    if let Some(text) = &file.content {
                        crate::edit::record_write(&self.log, &file.path, text);
                    }
                    paths.push(shown);
                }
                Ok(ToolResult::ok(summary.trim_end()).with_subject(Subject::FileWrites { paths }))
            }
            // Patch failures are the model's to fix, so they come back as a
            // tool error carrying the reason rather than aborting the run.
            Err(e) => Ok(tool_error(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// shell
// ---------------------------------------------------------------------------

pub struct ShellTool {
    root: PathBuf,
    shell: GuardedShell,
    default_timeout_ms: u64,
}

impl ShellTool {
    pub fn new(root: PathBuf, shell: GuardedShell, default_timeout_ms: u64) -> Self {
        Self {
            root,
            shell,
            default_timeout_ms,
        }
    }
}

impl Tool for ShellTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_SHELL.into(),
            description: "Run a shell command. Only commands permitted by the configured \
                          allowlist are executed; anything else is refused."
                .into(),
            params_json: r#"{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout_ms":{"type":"integer"}},"required":["command"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let command = match str_arg(&args, "command") {
            Ok(c) => c,
            Err(e) => return Ok(e),
        };
        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|c| resolve(&self.root, c))
            .unwrap_or_else(|| self.root.clone());
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout_ms);

        let cmd = ShellCommand {
            command,
            cwd: Some(cwd),
            env: Vec::new(),
            timeout_ms,
        };
        match self.shell.run_guarded(&cmd) {
            Ok(output) => Ok(ToolResult::ok(&output)),
            // A blocked command is reported back to the model, not fatal: it
            // can pick a different approach (FR-CONFIG-05).
            Err(e) => Ok(tool_error(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// zcode_skill
// ---------------------------------------------------------------------------

/// Loads a markdown skill as extra context.
///
/// The spec lists the skills that actually exist, because a model cannot call
/// something it does not know the name of — a bare "load a skill from the
/// skills directory" leaves it guessing, and it simply never calls the tool.
pub struct SkillTool {
    index: crate::skills::SkillIndex,
}

impl SkillTool {
    pub fn new(index: crate::skills::SkillIndex) -> Self {
        Self { index }
    }

    /// `name: summary` for each skill, capped so a large library cannot
    /// dominate the prompt.
    fn catalogue(&self) -> String {
        const MAX_LISTED: usize = 60;
        let entries = self.index.entries();
        let mut out = String::with_capacity(entries.len() * 64);
        for entry in entries.iter().take(MAX_LISTED) {
            out.push_str("\n- ");
            out.push_str(&entry.name);
            if !entry.summary.is_empty() {
                out.push_str(": ");
                out.push_str(&entry.summary);
            }
        }
        if entries.len() > MAX_LISTED {
            out.push_str(&format!(
                "\n- …and {} more (names follow the same pattern)",
                entries.len() - MAX_LISTED
            ));
        }
        out
    }
}

impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_SKILL.into(),
            description: format!(
                "Load a skill: project-specific guidance, conventions or checklists, \
                 as markdown. Call this before starting work when one is relevant, and \
                 follow what it says. Available skills:{}",
                self.catalogue()
            ),
            params_json: r#"{"type":"object","properties":{"name":{"type":"string","description":"One of the skill names listed in this tool's description"}},"required":["name"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let name = match str_arg(&args, "name") {
            Ok(n) => n,
            Err(e) => return Ok(e),
        };
        let Some(entry) = self.index.get(&name) else {
            // Name the alternatives so a wrong guess is self-correcting.
            let available = self.index.names().join(", ");
            return Ok(tool_error(format!(
                "no such skill `{name}`. Available: {available}"
            )));
        };
        match std::fs::read_to_string(&entry.path) {
            Ok(content) => Ok(ToolResult::ok(&content)),
            Err(e) => Ok(tool_error(format!("cannot read skill {name}: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_json_slips_in_arguments_are_mended() {
        let ok = |raw: &str, want: serde_json::Value| {
            assert_eq!(parse_args(raw).expect(raw), want, "{raw}");
        };
        ok(
            r#"{"pattern": ["**/*.rs"]}"#,
            serde_json::json!({"pattern": ["**/*.rs"]}),
        );
        ok(
            "{'pattern': '**/*.rs'}",
            serde_json::json!({"pattern": "**/*.rs"}),
        );
        ok(
            r#"{"pattern": **/*.rs}"#,
            serde_json::json!({"pattern": "**/*.rs"}),
        );
        ok(
            r#"{"pattern": [**/*.rs, *.go]}"#,
            serde_json::json!({"pattern": ["**/*.rs", "*.go"]}),
        );
        ok(
            r#"{pattern: "*.rs", limit: 5}"#,
            serde_json::json!({"pattern": "*.rs", "limit": 5}),
        );
        ok(
            r#"{"path": "src", "recursive": true,}"#,
            serde_json::json!({"path": "src", "recursive": true}),
        );
        ok(
            "{'q': 'it\\'s \"x\"'}",
            serde_json::json!({"q": "it's \"x\""}),
        );
        ok(
            r#"{"url": http://a/b?c=1}"#,
            serde_json::json!({"url": "http://a/b?c=1"}),
        );
    }

    #[test]
    fn unmendable_arguments_are_quoted_back_to_the_model() {
        let err = parse_args(r#"{"pattern": "**/*.rs""#)
            .unwrap_err()
            .error
            .unwrap();
        assert!(err.starts_with("arguments must be a JSON object:"), "{err}");
        assert!(err.contains(r#"You sent: {"pattern": "**/*.rs""#), "{err}");
        assert!(err.contains(r#"e.g. {"pattern": "**/*.rs"}"#), "{err}");
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn read_returns_contents_and_reports_missing_files() {
        let dir = tempdir();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let mut tool = ReadTool::new(dir.path().to_path_buf());

        let ok = tool.call(TOOL_READ, r#"{"path":"a.txt"}"#).unwrap();
        // FR-READ-03: every line carries its 1-based number.
        assert_eq!(ok.content, "1│hello");
        assert!(ok.error.is_none());

        // A missing file is the model's problem, not a crash.
        let missing = tool.call(TOOL_READ, r#"{"path":"nope.txt"}"#).unwrap();
        assert!(missing.error.is_some());
    }

    #[test]
    fn read_rejects_bad_arguments_without_failing_the_run() {
        let dir = tempdir();
        let mut tool = ReadTool::new(dir.path().to_path_buf());
        assert!(tool.call(TOOL_READ, "not json").unwrap().error.is_some());
        assert!(tool.call(TOOL_READ, "{}").unwrap().error.is_some());
    }

    #[test]
    fn paths_are_reported_relative_to_the_working_dir() {
        // Long absolute prefixes on every tool line waste the model's context
        // and get elided in the terminal.
        let dir = tempdir();
        let mut tool = WriteTool::new(dir.path().to_path_buf());
        let res = tool
            .call(TOOL_WRITE, r#"{"path":"src/main.rs","content":"x"}"#)
            .unwrap();
        assert!(res.content.starts_with("created src/main.rs"), "{res:?}");
        assert!(!res.content.contains(dir.path().to_str().unwrap()));

        let mut editor = StrReplaceTool::new(dir.path().to_path_buf());
        let res = editor
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"str_replace","path":"src/main.rs","old_str":"x","new_str":"y"}"#,
            )
            .unwrap();
        assert!(
            res.content
                .starts_with("edited src/main.rs: 1 replacement at line 1\n"),
            "{res:?}"
        );
    }

    #[test]
    fn write_creates_parent_dirs_atomically() {
        let dir = tempdir();
        let mut tool = WriteTool::new(dir.path().to_path_buf());
        let res = tool
            .call(
                TOOL_WRITE,
                r#"{"path":"nested/deep/x.rs","content":"fn main() {}"}"#,
            )
            .unwrap();
        assert!(res.error.is_none(), "{res:?}");
        let written = std::fs::read_to_string(dir.path().join("nested/deep/x.rs")).unwrap();
        assert_eq!(written, "fn main() {}");
        // The temp file used for the atomic rename must not linger.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("nested/deep"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("zcode-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
    }

    #[test]
    fn str_replace_round_trip() {
        let dir = tempdir();
        let path = dir.path().join("model.rs");
        std::fs::write(&path, "fn foo() {}\nfn other() { foo(); }\n").unwrap();
        let mut tool = StrReplaceTool::new(dir.path().to_path_buf());

        let res = tool
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"str_replace","path":"model.rs","old_str":"fn foo() {}","new_str":"fn bar() {}"}"#,
            )
            .unwrap();
        assert!(res.error.is_none(), "{res:?}");
        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.starts_with("fn bar() {}"));
        // Only the first occurrence is touched.
        assert!(updated.contains("foo();"));
    }

    #[test]
    fn str_replace_reports_absent_needle() {
        let dir = tempdir();
        std::fs::write(dir.path().join("a.rs"), "content").unwrap();
        let mut tool = StrReplaceTool::new(dir.path().to_path_buf());
        let res = tool
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"str_replace","path":"a.rs","old_str":"absent","new_str":"x"}"#,
            )
            .unwrap();
        assert!(res.error.unwrap().contains("not found"));
    }

    #[test]
    fn str_replace_refuses_ambiguous_matches_and_writes_nothing() {
        let dir = tempdir();
        std::fs::write(dir.path().join("a.rs"), "x\nx\n").unwrap();
        let mut tool = StrReplaceTool::new(dir.path().to_path_buf());
        let res = tool
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"str_replace","path":"a.rs","old_str":"x","new_str":"y"}"#,
            )
            .unwrap();
        // FR-EDIT-06: v0.6 edited the first and mentioned the count after.
        assert_eq!(
            res.error.as_deref(),
            Some(
                "old_str matches 2 places in a.rs (lines 1, 2) — include surrounding lines to \
                 make it unique, or set replace_all"
            )
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "x\nx\n"
        );
    }

    #[test]
    fn str_replace_view_create_and_list() {
        let dir = tempdir();
        let mut tool = StrReplaceTool::new(dir.path().to_path_buf());

        assert!(tool
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"create","path":"new.rs","file_text":"hi"}"#
            )
            .unwrap()
            .error
            .is_none());
        let viewed = tool
            .call(TOOL_STR_REPLACE, r#"{"command":"view","path":"new.rs"}"#)
            .unwrap();
        assert_eq!(viewed.content, "1│hi");

        let listed = tool
            .call(TOOL_STR_REPLACE, r#"{"command":"list_dir","path":"."}"#)
            .unwrap();
        assert!(listed.content.contains("new.rs"));

        let bad = tool
            .call(
                TOOL_STR_REPLACE,
                r#"{"command":"teleport","path":"new.rs"}"#,
            )
            .unwrap();
        assert!(bad.error.is_some());
    }

    #[test]
    fn list_dir_marks_directories() {
        let dir = tempdir();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("f.txt"), "").unwrap();
        let root = dir.path().to_path_buf();
        let mut tool = crate::search_tools::ListDirTool::new(
            root.clone(),
            crate::search_tools::default_search(&root),
        );
        let res = tool.call(TOOL_LIST_DIR, r#"{"path":"."}"#).unwrap();
        assert!(res.content.contains("sub/"));
        assert!(res.content.contains("f.txt"));
    }

    #[test]
    fn apply_patch_tool_edits_files() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let mut tool = ApplyPatchTool::new(dir.path().to_path_buf());

        let patch = "--- a/f.rs\n+++ b/f.rs\n@@ -1,2 +1,2 @@\n fn a() {}\n-fn b() {}\n+fn c() {}\n";
        let args = serde_json::json!({ "patch": patch }).to_string();
        let res = tool.call(TOOL_APPLY_PATCH, &args).unwrap();
        assert!(res.error.is_none(), "{res:?}");
        assert!(res.content.contains("patched"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.rs")).unwrap(),
            "fn a() {}\nfn c() {}\n"
        );
    }

    #[test]
    fn apply_patch_tool_reports_failures_to_the_model() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.rs"), "unrelated\n").unwrap();
        let mut tool = ApplyPatchTool::new(dir.path().to_path_buf());
        let patch = "--- a/f.rs\n+++ b/f.rs\n@@ -1,2 +1,2 @@\n fn a() {}\n-fn b() {}\n+fn c() {}\n";
        let args = serde_json::json!({ "patch": patch }).to_string();
        let res = tool.call(TOOL_APPLY_PATCH, &args).unwrap();
        assert!(res.error.unwrap().contains("re-read the file"));
    }

    #[test]
    fn apply_patch_tool_accepts_a_diff_alias() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.txt"), "a\n").unwrap();
        let mut tool = ApplyPatchTool::new(dir.path().to_path_buf());
        let args = serde_json::json!({
            "diff": "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-a\n+A\n"
        })
        .to_string();
        assert!(tool.call(TOOL_APPLY_PATCH, &args).unwrap().error.is_none());
    }

    #[test]
    fn apply_patch_tool_rejects_non_diff_input() {
        let dir = tempdir();
        let mut tool = ApplyPatchTool::new(dir.path().to_path_buf());
        let args = serde_json::json!({ "patch": "please change the file" }).to_string();
        assert!(tool.call(TOOL_APPLY_PATCH, &args).unwrap().error.is_some());
    }

    #[test]
    fn shell_tool_runs_allowed_and_reports_blocked() {
        let dir = tempdir();
        let guard =
            GuardedShell::new(infra_shell::StdShell::new(), &["echo .*".to_string()]).unwrap();
        let mut tool = ShellTool::new(dir.path().to_path_buf(), guard, 5_000);

        let ok = tool.call(TOOL_SHELL, r#"{"command":"echo hi"}"#).unwrap();
        assert!(ok.content.contains("hi"), "{ok:?}");

        // Blocked commands come back as a tool error the model can read,
        // rather than aborting the whole run.
        let blocked = tool
            .call(TOOL_SHELL, r#"{"command":"git status"}"#)
            .unwrap();
        let message = blocked.error.expect("not in this allowlist");
        assert!(message.contains("blocked"), "{message}");

        // The built-in denylist reports itself distinctly, so the model learns
        // that widening `shell_allowed` would not help.
        let denied = tool.call(TOOL_SHELL, r#"{"command":"rm -rf /"}"#).unwrap();
        let message = denied.error.expect("denylisted");
        assert!(message.contains("denylist"), "{message}");
    }

    #[test]
    fn skill_tool_lists_and_loads_by_name() {
        let dir = tempdir();
        let root = dir.path().join("skills");
        std::fs::create_dir_all(root.join("review")).unwrap();
        std::fs::write(
            root.join("review/SKILL.md"),
            "---\ndescription: How we review code.\n---\n\n# Review\n",
        )
        .unwrap();
        std::fs::write(root.join("style.md"), "# Style\n\nDoc comments please.\n").unwrap();

        let index = crate::skills::SkillIndex::discover(&[root]);
        let mut tool = SkillTool::new(index);

        // The model is told what exists, with a summary for each.
        let description = tool.spec().description;
        assert!(
            description.contains("review: How we review code."),
            "{description}"
        );
        assert!(
            description.contains("style: Doc comments please."),
            "{description}"
        );

        let loaded = tool.call(TOOL_SKILL, r#"{"name":"review"}"#).unwrap();
        assert!(loaded.content.contains("# Review"));
        assert!(loaded.error.is_none());
    }

    #[test]
    fn unknown_skill_names_the_alternatives() {
        let dir = tempdir();
        let root = dir.path().join("skills");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("style.md"), "x\n").unwrap();
        let index = crate::skills::SkillIndex::discover(&[root]);
        let mut tool = SkillTool::new(index);

        // A guess must be self-correcting, and must never touch the filesystem.
        for guess in ["../../etc/passwd", "nope", "/etc/passwd"] {
            let args = serde_json::json!({ "name": guess }).to_string();
            let res = tool.call(TOOL_SKILL, &args).unwrap();
            let err = res.error.expect("unknown skill is an error");
            assert!(err.contains("Available: style"), "{err}");
        }
    }

    // ---- read shaping (FR-READ-01..04) -----------------------------------

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    fn read(dir: &tempfile::TempDir, args: serde_json::Value) -> ToolResult {
        ReadTool::new(dir.path().to_path_buf())
            .call(TOOL_READ, &args.to_string())
            .unwrap()
    }

    #[test]
    fn a_range_returns_those_lines_with_a_gutter_and_a_next_offset() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.txt"), numbered(120)).unwrap();
        let res = read(
            &dir,
            serde_json::json!({ "path": "f.txt", "offset": 98, "limit": 3 }),
        );
        assert_eq!(
            res.content,
            " 98│line 98\n 99│line 99\n100│line 100\n[lines 98-100 of 120; next: offset 101]"
        );
        let tail = read(&dir, serde_json::json!({ "path": "f.txt", "offset": 119 }));
        assert_eq!(tail.content, "119│line 119\n120│line 120");
        assert_eq!(
            tail.subject,
            Some(Subject::FileRange {
                path: "f.txt".into(),
                start: 119,
                end: 120,
                hash: crate::render::fnv64(numbered(120).as_bytes()),
            })
        );
    }

    #[test]
    fn a_long_file_without_a_range_returns_its_head_and_how_to_get_more() {
        let dir = tempdir();
        std::fs::write(dir.path().join("big.rs"), numbered(2_318)).unwrap();
        let res = read(&dir, serde_json::json!({ "path": "big.rs" }));
        // The gutter is as wide as the last line *shown* (400), not the file.
        assert!(res.content.starts_with("  1│line 1\n"));
        assert!(res.content.contains("400│line 400\n"));
        assert!(!res.content.contains("line 401"));
        assert!(res.content.ends_with(
            "[lines 1-400 of 2,318 — use offset/limit, or grep to find the part you need]"
        ));
        let custom = ReadTool::new(dir.path().to_path_buf())
            .with_default_limit(10)
            .call(TOOL_READ, r#"{"path":"big.rs"}"#)
            .unwrap();
        assert!(custom.content.contains("[lines 1-10 of 2,318"));
    }

    #[test]
    fn an_offset_past_the_end_is_a_model_error_naming_the_length() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.txt"), numbered(12)).unwrap();
        let res = read(&dir, serde_json::json!({ "path": "f.txt", "offset": 900 }));
        assert_eq!(
            res.error.as_deref(),
            Some("offset 900 is past the end of f.txt (12 lines)")
        );
    }

    #[test]
    fn binary_files_are_refused_and_long_lines_clipped() {
        let dir = tempdir();
        std::fs::write(dir.path().join("x.png"), b"\x89PNG\0\0rest").unwrap();
        let res = read(&dir, serde_json::json!({ "path": "x.png" }));
        assert!(
            res.error
                .as_deref()
                .is_some_and(|e| e.contains(".png binary file")),
            "{res:?}"
        );
        let line = "é".repeat(2_500);
        std::fs::write(dir.path().join("min.js"), &line).unwrap();
        let res = read(&dir, serde_json::json!({ "path": "min.js" }));
        assert!(
            res.content.ends_with("…[+500 chars]"),
            "{}",
            &res.content[res.content.len() - 20..]
        );
        let empty = tempdir();
        std::fs::write(empty.path().join("e.txt"), "").unwrap();
        assert_eq!(
            read(&empty, serde_json::json!({ "path": "e.txt" })).content,
            "(empty file)"
        );
    }

    #[test]
    fn editor_view_range_maps_onto_the_same_reader() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.txt"), numbered(10)).unwrap();
        let mut tool = StrReplaceTool::new(dir.path().to_path_buf());
        let view = |tool: &mut StrReplaceTool, range: serde_json::Value| {
            tool.call(
                TOOL_STR_REPLACE,
                &serde_json::json!({ "command": "view", "path": "f.txt", "view_range": range })
                    .to_string(),
            )
            .unwrap()
        };
        let mid = view(&mut tool, serde_json::json!([3, 4]));
        assert_eq!(
            mid.content,
            "3│line 3\n4│line 4\n[lines 3-4 of 10; next: offset 5]"
        );
        let to_end = view(&mut tool, serde_json::json!([9, -1]));
        assert_eq!(to_end.content, " 9│line 9\n10│line 10");
        let bad = view(&mut tool, serde_json::json!([5, 2]));
        assert!(bad.error.is_some());
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    // ---- edit safety (FR-EDIT-06..08) -------------------------------------

    fn edit(dir: &tempfile::TempDir, args: serde_json::Value) -> ToolResult {
        let mut args = args;
        args["command"] = "str_replace".into();
        args["path"] = "f.rs".into();
        StrReplaceTool::new(dir.path().to_path_buf())
            .call(TOOL_STR_REPLACE, &args.to_string())
            .unwrap()
    }

    fn file(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("f.rs")).unwrap()
    }

    #[test]
    fn replace_all_and_expected_replacements_are_explicit_opt_ins() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.rs"), "a();\nb();\na();\n").unwrap();
        let res = edit(
            &dir,
            serde_json::json!({ "old_str": "a();", "new_str": "z();", "replace_all": true }),
        );
        assert!(
            res.content.starts_with("edited f.rs: 2 replacements\n"),
            "{res:?}"
        );
        assert_eq!(file(&dir), "z();\nb();\nz();\n");
        let mismatch = edit(
            &dir,
            serde_json::json!({ "old_str": "z();", "new_str": "q();", "replace_all": true,
                                "expected_replacements": 3 }),
        );
        assert_eq!(
            mismatch.error.as_deref(),
            Some("expected 3 replacement(s) in f.rs, found 2; nothing was written")
        );
        assert_eq!(file(&dir), "z();\nb();\nz();\n");
    }

    #[test]
    fn indentation_drift_is_matched_and_disclosed() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("f.rs"),
            "fn a() {\n    if ok {\n        run();\n    }\n}\n",
        )
        .unwrap();
        // The model copied the block without its indentation.
        let res = edit(
            &dir,
            serde_json::json!({ "old_str": "if ok {\n    run();\n}",
                                "new_str": "if ok {\n    run();\n    log();\n}" }),
        );
        assert!(
            res.content
                .contains("(matched with whitespace normalisation)"),
            "{res:?}"
        );
        assert_eq!(
            file(&dir),
            "fn a() {\n    if ok {\n        run();\n        log();\n    }\n}\n"
        );
    }

    #[test]
    fn trailing_whitespace_drift_is_matched() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.rs"), "let x = 1;   \nlet y = 2;\n").unwrap();
        let res = edit(
            &dir,
            serde_json::json!({ "old_str": "let x = 1;", "new_str": "let x = 9;" }),
        );
        assert!(res.error.is_none(), "{res:?}");
        assert!(file(&dir).starts_with("let x = 9;"), "{}", file(&dir));
    }

    #[test]
    fn a_miss_shows_the_closest_region_so_no_reread_is_needed() {
        let dir = tempdir();
        let body: String = (1..=40).map(|i| format!("line {i};\n")).collect();
        let body = body.replace("line 20;", "let total = compute(a, b);");
        std::fs::write(dir.path().join("f.rs"), &body).unwrap();
        let res = edit(
            &dir,
            serde_json::json!({ "old_str": "line 19;\nlet total = compute(a, c);\nline 21;",
                                "new_str": "x" }),
        );
        let err = res.error.unwrap();
        assert!(
            err.contains("Closest match (2 of 3 lines equal), lines 19-21:"),
            "{err}"
        );
        assert!(err.contains("20│let total = compute(a, b);"), "{err}");
        assert_eq!(file(&dir), body, "nothing written");
    }

    #[test]
    fn write_reports_a_diffstat_and_suggests_edits_for_big_rewrites() {
        let dir = tempdir();
        let mut tool = WriteTool::new(dir.path().to_path_buf());
        let big: String = (1..=250).map(|i| format!("l{i}\n")).collect();
        let created = tool
            .call(
                TOOL_WRITE,
                &serde_json::json!({ "path": "f.rs", "content": big }).to_string(),
            )
            .unwrap();
        assert_eq!(created.content, "created f.rs (250 lines)");
        let changed = big.replace("l7\n", "seven\n");
        let res = tool
            .call(
                TOOL_WRITE,
                &serde_json::json!({ "path": "f.rs", "content": changed }).to_string(),
            )
            .unwrap();
        assert!(
            res.content
                .starts_with("wrote f.rs: 250 → 250 lines (+1 −1)"),
            "{}",
            res.content
        );
        assert!(res
            .content
            .contains("tip: for a small change use str_replace_editor"));
        assert_eq!(
            res.subject,
            Some(Subject::FileWrite {
                path: "f.rs".into()
            })
        );
    }

    #[test]
    fn apply_patch_reports_a_per_file_diffstat_without_content() {
        let dir = tempdir();
        std::fs::write(dir.path().join("f.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let mut tool = ApplyPatchTool::new(dir.path().to_path_buf());
        let patch = "--- a/f.rs\n+++ b/f.rs\n@@ -1,2 +1,3 @@\n fn a() {}\n-fn b() {}\n+fn c() {}\n+fn d() {}\n\
                     --- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let res = tool
            .call(
                TOOL_APPLY_PATCH,
                &serde_json::json!({ "patch": patch }).to_string(),
            )
            .unwrap();
        assert_eq!(res.content, "patched 2 file(s):\nM f.rs +2 −1\nA new.rs +2");
        assert_eq!(
            res.subject,
            Some(Subject::FileWrites {
                paths: vec!["f.rs".into(), "new.rs".into()]
            })
        );
    }

    /// FR-EDIT-02/08: an edit result is proportional to the edit, never the
    /// file — for random single-line edits of a 300-line file.
    #[test]
    fn edit_results_never_echo_the_file() {
        let dir = tempdir();
        let body: String = (1..=300).map(|i| format!("statement_{i}();\n")).collect();
        for target in [1, 57, 150, 299, 300] {
            std::fs::write(dir.path().join("f.rs"), &body).unwrap();
            let res = edit(
                &dir,
                serde_json::json!({ "old_str": format!("statement_{target}();"),
                                    "new_str": "changed();" }),
            );
            assert!(res.error.is_none(), "{res:?}");
            assert!(
                res.content.len() < body.len() / 20,
                "{} bytes",
                res.content.len()
            );
            assert!(res.content.contains("changed();"));
        }
    }
}
