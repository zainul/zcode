//! Edit helpers shared by the write tools (FR-EDIT-06..09, PRD §5.6).
//!
//! * Matching that is **safe on ambiguity** — never silently editing the
//!   first of several matches — and **forgiving with disclosure**: one retry
//!   that tolerates indentation and trailing-whitespace drift, and when even
//!   that fails, the closest region with line numbers so the model can retry
//!   without re-reading the whole file.
//! * Results that **never echo files**: a diffstat and the lines around each
//!   changed region ("seams").
//! * A **write log** every write path records into, so the registry can keep
//!   the language server (and the code index) in sync after any edit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::render::{digits, gutter};

/// Every file a tool wrote, with its new text, drained by the registry after
/// each call (FR-EDIT-09, FR-LSP-09).
pub type WriteLog = Arc<Mutex<Vec<(PathBuf, String)>>>;

pub fn record_write(log: &WriteLog, path: &Path, text: &str) {
    if let Ok(mut entries) = log.lock() {
        entries.push((path.to_path_buf(), text.to_string()));
    }
}

/// 1-based line of byte offset `at`.
pub fn line_of(text: &str, at: usize) -> u32 {
    let at = at.min(text.len());
    (text.as_bytes()[..at]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1) as u32
}

/// Byte offsets where each line starts.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|i| *i <= text.len())
        .collect()
}

/// Width of leading whitespace, a tab counting 4.
fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// A region found by whitespace-normalised matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedMatch {
    /// Byte range in the file (whole lines, without the final newline).
    pub start: usize,
    pub end: usize,
    /// Leading whitespace of the first non-blank line, in the file and in
    /// `old_str` — used to re-indent the replacement.
    pub file_indent: String,
    pub old_indent: String,
}

/// Every window of `content` whose lines equal `old`'s once leading and
/// trailing whitespace is ignored, where the indentation differs by the same
/// amount on every non-blank line (the same block, re-indented).
pub fn find_normalised(content: &str, old: &str) -> Vec<NormalisedMatch> {
    let old_lines: Vec<&str> = old.trim_end_matches('\n').split('\n').collect();
    let file_lines: Vec<&str> = content.split('\n').collect();
    if old_lines.iter().all(|l| l.trim().is_empty()) || old_lines.len() > file_lines.len() {
        return Vec::new();
    }
    let starts = line_starts(content);
    let old_first = old_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .copied()
        .unwrap_or("");
    let mut found = Vec::new();
    for w in 0..=file_lines.len() - old_lines.len() {
        let window = &file_lines[w..w + old_lines.len()];
        let mut delta: Option<isize> = None;
        let same = window.iter().zip(&old_lines).all(|(f, o)| {
            if f.trim() != o.trim() {
                return false;
            }
            if f.trim().is_empty() {
                return true;
            }
            let d = indent_width(f) as isize - indent_width(o) as isize;
            *delta.get_or_insert(d) == d
        });
        if !same {
            continue;
        }
        let file_first = window
            .iter()
            .find(|l| !l.trim().is_empty())
            .copied()
            .unwrap_or("");
        let start = starts[w];
        let last = w + old_lines.len() - 1;
        let end = starts[last] + file_lines[last].len();
        found.push(NormalisedMatch {
            start,
            end,
            file_indent: leading_ws(file_first).to_string(),
            old_indent: leading_ws(old_first).to_string(),
        });
    }
    found
}

/// Re-base `new`'s indentation from `old_indent` onto `file_indent`, so a
/// replacement written at the model's indentation lands at the file's.
pub fn reindent(new: &str, old_indent: &str, file_indent: &str) -> String {
    if old_indent == file_indent {
        return new.to_string();
    }
    new.split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                line.to_string()
            } else if let Some(rest) = line.strip_prefix(old_indent) {
                format!("{file_indent}{rest}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The window most similar to `old` (most lines equal after trimming), as
/// gutter-numbered text, for the error when nothing matched. `None` for a
/// file too large to scan cheaply or with no line in common.
pub fn closest_region(content: &str, old: &str) -> Option<(u32, u32, usize, usize, String)> {
    const MAX_LINES: usize = 20_000;
    const SHOW: usize = 10;
    let old_lines: Vec<&str> = old.trim_end_matches('\n').split('\n').collect();
    let file_lines: Vec<&str> = content.split('\n').collect();
    if file_lines.len() > MAX_LINES || old_lines.is_empty() {
        return None;
    }
    let n = old_lines.len().min(file_lines.len());
    let mut best: Option<(usize, usize)> = None; // (score, window start)
    for w in 0..=file_lines.len() - n {
        let score = file_lines[w..w + n]
            .iter()
            .zip(&old_lines)
            .filter(|(f, o)| !o.trim().is_empty() && f.trim() == o.trim())
            .count();
        if score > best.map_or(0, |b| b.0) {
            best = Some((score, w));
        }
    }
    let (score, w) = best?;
    let shown = n.min(SHOW);
    let width = digits((w + shown) as u32);
    let mut text = String::new();
    for (i, line) in file_lines[w..w + shown].iter().enumerate() {
        text.push_str(&gutter((w + i + 1) as u32, width, false));
        text.push_str(line);
        text.push('\n');
    }
    Some((
        (w + 1) as u32,
        (w + shown) as u32,
        score,
        old_lines.iter().filter(|l| !l.trim().is_empty()).count(),
        text,
    ))
}

/// Lines added and removed between two texts (Myers' O(ND) edit distance on
/// lines). `None` when either side exceeds `MAX_DIFF_LINES` — the caller
/// then reports line counts only rather than spend the time.
pub fn line_diffstat(old: &str, new: &str) -> Option<(usize, usize)> {
    const MAX_DIFF_LINES: usize = 5_000;
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len() > MAX_DIFF_LINES || b.len() > MAX_DIFF_LINES {
        return None;
    }
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let offset = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    for d in 0..=max as isize {
        let mut k = -d;
        while k <= d {
            let idx = |k: isize| (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) {
                v[idx(k + 1)]
            } else {
                v[idx(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx(k)] = x;
            if x >= n && y >= m {
                // d edits: lcs = (n + m - d) / 2.
                let lcs = ((n + m - d) / 2) as usize;
                return Some((b.len() - lcs, a.len() - lcs));
            }
            k += 2;
        }
    }
    None
}

/// The lines around each changed range of `text` (1-based, inclusive), at
/// most three seams, `ctx` lines either side: enough to see the edit landed
/// where intended, without echoing the file.
pub fn seams(text: &str, changed: &[(u32, u32)], ctx: u32) -> String {
    const MAX_SEAMS: usize = 3;
    let lines: Vec<&str> = text.split('\n').collect();
    let total = lines.len() as u32;
    let mut out = String::new();
    for (i, (first, last)) in changed.iter().take(MAX_SEAMS).enumerate() {
        let from = first.saturating_sub(ctx).max(1);
        let to = (last + ctx).min(total);
        let width = digits(to);
        if i > 0 {
            out.push_str("…\n");
        }
        for n in from..=to {
            out.push_str(&gutter(n, width, false));
            out.push_str(lines[(n - 1) as usize]);
            out.push('\n');
        }
    }
    if changed.len() > MAX_SEAMS {
        out.push_str(&format!("(+{} more changes)\n", changed.len() - MAX_SEAMS));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalised_matching_tolerates_uniform_reindentation() {
        let file = "fn a() {\n    if x {\n        go();\n    }\n}\n";
        let old = "if x {\n    go();\n}";
        let found = find_normalised(file, old);
        assert_eq!(found.len(), 1);
        assert_eq!(
            &file[found[0].start..found[0].end],
            "    if x {\n        go();\n    }"
        );
        assert_eq!(found[0].file_indent, "    ");
        // Non-uniform drift is not the same block.
        assert!(find_normalised(file, "if x {\ngo();\n}").is_empty());
    }

    #[test]
    fn reindent_rebases_relative_indentation() {
        assert_eq!(
            reindent("if y {\n    stop();\n}", "", "    "),
            "    if y {\n        stop();\n    }"
        );
        assert_eq!(reindent("\tx", "\t", "\t\t"), "\t\tx");
    }

    #[test]
    fn closest_region_scores_by_equal_lines() {
        let file = "a\nb\nfn run(x: u8) {\n    step();\n    done();\n}\nz\n";
        let old = "fn run(x: u16) {\n    step();\n    done();\n}";
        let (from, to, score, of, text) = closest_region(file, old).unwrap();
        assert_eq!((from, to, score, of), (3, 6, 3, 4));
        assert!(text.starts_with("3│fn run(x: u8) {\n"));
        assert!(closest_region("x\ny\n", "nothing\nshared").is_none());
    }

    #[test]
    fn diffstat_counts_lines_added_and_removed() {
        assert_eq!(line_diffstat("a\nb\nc\n", "a\nB\nc\nd\n"), Some((2, 1)));
        assert_eq!(line_diffstat("", "x\ny\n"), Some((2, 0)));
        assert_eq!(line_diffstat("same\n", "same\n"), Some((0, 0)));
        let big = "x\n".repeat(6_000);
        assert_eq!(line_diffstat(&big, "y\n"), None);
    }

    #[test]
    fn seams_show_context_around_each_change_and_cap_the_count() {
        let text: String = (1..=30).map(|i| format!("l{i}\n")).collect();
        let s = seams(&text, &[(5, 5)], 2);
        assert_eq!(s, "3│l3\n4│l4\n5│l5\n6│l6\n7│l7\n");
        let many = seams(&text, &[(2, 2), (10, 10), (20, 20), (25, 25)], 0);
        assert!(many.contains("…\n") && many.ends_with("(+1 more changes)\n"));
    }

    #[test]
    fn line_of_is_one_based() {
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 4), 3);
    }
}
