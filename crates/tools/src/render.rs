//! Shared output conventions for tool results (CE-DQ13, PRD §4.2).
//!
//! Every tool that returns a list or a file excerpt renders through these, so
//! the rules hold everywhere at once:
//!
//! * **Bounded, and says what it left out.** A cut list ends with a [`Footer`]
//!   giving the total and the next `offset`, never a silent truncation.
//! * **Deterministic bytes.** Same input, same output — the prompt cache and
//!   the evaluation harness both depend on it.
//! * **1-based line numbers** everywhere the model can see (CE-DQ14).

/// FNV-1a, 64-bit. Stable across runs and platforms (unlike `DefaultHasher`),
/// so it can key content in the transcript and on disk.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The closing line of a bounded list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    pub shown: usize,
    pub total: usize,
    /// What is being counted: "files", "matches", "entries"…
    pub unit: &'static str,
    /// The `offset` that fetches the next page, when there is one.
    pub next_offset: Option<usize>,
    /// How to narrow instead of paging.
    pub hint: &'static str,
    /// Extra notes: skipped files, a partial walk.
    pub notes: Vec<String>,
}

impl Footer {
    pub fn render(&self) -> String {
        let mut out = if self.shown < self.total {
            format!("[showing {} of {} {}", self.shown, self.total, self.unit)
        } else {
            format!("[{} {}", self.total, self.unit)
        };
        if self.shown < self.total && !self.hint.is_empty() {
            out.push_str(" — ");
            out.push_str(self.hint);
        }
        if let Some(next) = self.next_offset {
            out.push_str(&format!("; next page: offset {next}"));
        }
        for note in &self.notes {
            out.push_str("; ");
            out.push_str(note);
        }
        out.push(']');
        out
    }
}

/// Largest char boundary `<= i`.
pub fn floor_char(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Clip `line` to at most `max` characters, keeping byte column `centre` in
/// view and marking each cut with `…` — a minified file's one line is still
/// shown around the part that matched.
pub fn clip_line(line: &str, max: usize, centre: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let centre = floor_char(line, centre);
    let before: Vec<(usize, char)> = line[..centre].char_indices().collect();
    // Start a third of the budget before the centre.
    let lead = (max / 3).min(before.len());
    let start = before.get(before.len() - lead).map_or(centre, |(i, _)| *i);
    let body: String = line[start..].chars().take(max).collect();
    let end = start + body.len();
    let mut out = String::with_capacity(body.len() + 8);
    if start > 0 {
        out.push('…');
    }
    out.push_str(&body);
    if end < line.len() {
        out.push('…');
    }
    out
}

/// Decimal digits of `n` (at least 1).
pub fn digits(n: u32) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

/// A right-aligned 1-based line number and separator: `  42│`. Context lines
/// use `-` in place of `│`, as `grep -C` does.
pub fn gutter(line: u32, width: usize, context: bool) -> String {
    format!("{line:>width$}{}", if context { '-' } else { '│' })
}

/// Singular or plural of a count, e.g. `1 match`, `3 matches`.
pub fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv64_is_stable() {
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv64(b"ab"), fnv64(b"ba"));
    }

    #[test]
    fn footer_says_what_was_left_out_and_how_to_get_it() {
        let f = Footer {
            shown: 20,
            total: 143,
            unit: "files",
            next_offset: Some(20),
            hint: "narrow with `glob` or `path`",
            notes: vec!["2 large files skipped".into()],
        };
        assert_eq!(
            f.render(),
            "[showing 20 of 143 files — narrow with `glob` or `path`; next page: offset 20; \
             2 large files skipped]"
        );
        let whole = Footer {
            shown: 3,
            total: 3,
            unit: "matches",
            next_offset: None,
            hint: "unused",
            notes: vec![],
        };
        assert_eq!(whole.render(), "[3 matches]");
    }

    #[test]
    fn clip_keeps_the_centre_in_view_on_char_boundaries() {
        let line = format!("{}NEEDLE{}", "é".repeat(300), "ü".repeat(300));
        let centre = line.find("NEEDLE").unwrap();
        let clipped = clip_line(&line, 60, centre);
        assert!(clipped.contains("NEEDLE"));
        assert!(clipped.starts_with('…') && clipped.ends_with('…'));
        assert!(clipped.chars().count() <= 62);
        assert_eq!(clip_line("short", 60, 0), "short");
    }

    #[test]
    fn gutter_is_right_aligned_and_one_based() {
        assert_eq!(gutter(7, 3, false), "  7│");
        assert_eq!(gutter(7, 3, true), "  7-");
        assert_eq!(digits(0), 1);
        assert_eq!(digits(999), 3);
        assert_eq!(digits(1000), 4);
    }
}
