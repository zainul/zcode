//! Line wrapping shared by the renderer and the scroll arithmetic.
//!
//! `ratatui`'s `Wrap` widget wraps at draw time, which means the widget knows
//! how many rows it produced but the scroll offset — computed *before* the
//! draw — does not. That mismatch is why long answers used to scroll past
//! their own tail. Wrapping here, once, keeps both in agreement.
//!
//! Width is counted in `char`s rather than display columns. Adding
//! `unicode-width` for the East-Asian and emoji cases would be correct, but it
//! is a dependency for a cosmetic column or two, and this crate stays small on
//! purpose (NFR-PERF-03). Wide glyphs wrap one or two columns early.

/// Break `text` into lines that each fit in `width` chars, preserving explicit
/// newlines. Words longer than `width` (a URL, a base64 blob) are hard-broken
/// rather than allowed to overflow.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for logical in text.split('\n') {
        if logical.is_empty() {
            out.push(String::new());
            continue;
        }
        // Preserve leading indentation on continuation lines: wrapped code and
        // list items stay readable instead of collapsing to the left margin.
        let indent: String = logical
            .chars()
            .take_while(|c| *c == ' ')
            .take(width.saturating_sub(1))
            .collect();
        let mut line = String::new();
        let mut line_len = 0usize;
        for word in logical.split_inclusive(' ') {
            let word_len = word.chars().count();
            if line_len + word_len > width && line_len > 0 {
                out.push(std::mem::take(&mut line).trim_end().to_string());
                line.push_str(&indent);
                line_len = indent.chars().count();
            }
            if word_len > width {
                // Hard-break an over-long word across as many rows as it needs.
                for c in word.chars() {
                    if line_len == width {
                        out.push(std::mem::take(&mut line));
                        line_len = 0;
                    }
                    line.push(c);
                    line_len += 1;
                }
            } else {
                line.push_str(word);
                line_len += word_len;
            }
        }
        out.push(line.trim_end().to_string());
    }
    out
}

/// How many rows `text` occupies at `width`, without building them.
///
/// The render loop needs the total height to place the scroll window, but
/// materialising every row of a 400-entry timeline sixty times a minute is
/// thousands of throwaway allocations per second. Counting is allocation-free,
/// so only the rows actually on screen are ever built.
pub fn height(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut rows = 0;
    for logical in text.split('\n') {
        if logical.is_empty() {
            rows += 1;
            continue;
        }
        let indent = logical
            .chars()
            .take_while(|c| *c == ' ')
            .take(width.saturating_sub(1))
            .count();
        let mut line_len = 0usize;
        let mut emitted = false;
        for word in logical.split_inclusive(' ') {
            let word_len = word.chars().count();
            if line_len + word_len > width && line_len > 0 {
                rows += 1;
                emitted = true;
                line_len = indent;
            }
            if word_len > width {
                for _ in 0..word_len {
                    if line_len == width {
                        rows += 1;
                        emitted = true;
                        line_len = 0;
                    }
                    line_len += 1;
                }
            } else {
                line_len += word_len;
            }
        }
        // The trailing partial row.
        rows += usize::from(line_len > 0 || !emitted);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_unchanged() {
        assert_eq!(wrap("hello", 20), vec!["hello"]);
    }

    #[test]
    fn wraps_on_word_boundaries() {
        assert_eq!(
            wrap("the quick brown fox", 10),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn explicit_newlines_are_preserved() {
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn an_over_long_word_is_hard_broken_not_overflowed() {
        let url = "https://example.com/a/very/long/path/that/never/ends";
        let lines = wrap(url, 10);
        assert!(lines.iter().all(|l| l.chars().count() <= 10), "{lines:?}");
        assert_eq!(lines.concat(), url);
    }

    #[test]
    fn continuation_lines_keep_the_indent() {
        let lines = wrap("    let x = compute(alpha, beta, gamma);", 20);
        assert!(lines.len() > 1);
        assert!(lines[1].starts_with("    "), "{lines:?}");
    }

    #[test]
    fn no_line_exceeds_the_width() {
        let text = "a bb ccc dddd eeeee ffffff ggggggg hhhhhhhh";
        for width in 4..20 {
            for line in wrap(text, width) {
                assert!(line.chars().count() <= width, "width {width}: {line:?}");
            }
        }
    }

    #[test]
    fn multibyte_text_is_counted_in_characters() {
        // Six chars, twelve bytes: must not wrap as if it were twelve columns.
        assert_eq!(wrap("ééééée", 6), vec!["ééééée"]);
    }

    #[test]
    fn height_agrees_with_wrap_for_every_width() {
        // The scroll window is placed from `height` and drawn from `wrap`; if
        // they ever disagree the view jumps.
        let cases = [
            "",
            "short",
            "the quick brown fox jumps over the lazy dog",
            "a\n\nb",
            "    indented continuation that has to wrap somewhere sensible",
            "https://example.com/a/very/long/path/that/never/ends",
            "trailing space ",
            "ééééée",
            "one\ntwo\nthree",
        ];
        for text in cases {
            for width in 1..40 {
                assert_eq!(
                    height(text, width),
                    wrap(text, width).len(),
                    "text {text:?} at width {width}"
                );
            }
        }
    }

    #[test]
    fn height_is_one_at_zero_width() {
        assert_eq!(height("text", 0), wrap("text", 0).len());
    }

    #[test]
    fn zero_width_is_survivable() {
        assert_eq!(wrap("text", 0), vec!["text"]);
    }
}
