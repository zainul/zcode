//! The prompt editor: a cursor, multi-line text, and the editing keys people
//! expect from a shell.
//!
//! Kept separate from rendering so every operation is unit-testable without a
//! terminal. All indices are **byte** offsets into `text` and are maintained
//! on `char` boundaries — slicing a UTF-8 string anywhere else panics, and a
//! pasted diff full of `—` and `…` is exactly the input that would find it.

/// Capacity the buffer may keep once it is emptied. Anything larger is
/// released: pasting a 500 KB file and deleting it should not cost that much
/// for the rest of the session (NFR-PERF-03).
const KEEP_CAPACITY: usize = 8 * 1024;

/// An editable prompt buffer.
#[derive(Debug, Default, Clone)]
pub struct Input {
    text: String,
    /// Byte offset of the caret, always on a char boundary, always <= len.
    cursor: usize,
}

impl Input {
    /// Give back an outsized allocation once the text no longer needs it.
    ///
    /// `String::clear` and `replace_range` both keep the capacity — which is
    /// right between keystrokes and wrong after a huge paste is deleted.
    fn release_slack(&mut self) {
        if self.text.capacity() > KEEP_CAPACITY && self.text.len() < KEEP_CAPACITY / 2 {
            self.text.shrink_to(KEEP_CAPACITY);
        }
    }

    /// Bytes the buffer holds, for the memory tests.
    pub fn heap_bytes(&self) -> usize {
        self.text.capacity()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.release_slack();
    }

    /// Take the contents, leaving the buffer empty.
    ///
    /// `mem::take` hands the allocation to the caller and starts fresh, so a
    /// sent prompt never leaves its capacity behind.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert a whole string at the caret — the paste path.
    ///
    /// Carriage returns are normalised away: a terminal paste of CRLF text
    /// would otherwise leave stray `\r` that the model sees and the renderer
    /// has to strip.
    pub fn insert_str(&mut self, s: &str) {
        let cleaned: String = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &cleaned);
        self.cursor += cleaned.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_boundary(self.cursor);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..next, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.prev_boundary(self.cursor);
    }

    pub fn right(&mut self) {
        self.cursor = self.next_boundary(self.cursor);
    }

    /// Start of the current visual line (after the previous `\n`).
    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    /// End of the current visual line (before the next `\n`).
    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    pub fn start(&mut self) {
        self.cursor = 0;
    }

    pub fn finish(&mut self) {
        self.cursor = self.text.len();
    }

    /// Move to the beginning of the previous word (Ctrl/Alt-Left).
    pub fn word_left(&mut self) {
        let mut i = self.cursor;
        while i > 0 && self.char_before(i).is_some_and(char::is_whitespace) {
            i = self.prev_boundary(i);
        }
        while i > 0 && self.char_before(i).is_some_and(|c| !c.is_whitespace()) {
            i = self.prev_boundary(i);
        }
        self.cursor = i;
    }

    /// Move to the end of the next word (Ctrl/Alt-Right).
    pub fn word_right(&mut self) {
        let mut i = self.cursor;
        let len = self.text.len();
        while i < len && self.char_at(i).is_some_and(char::is_whitespace) {
            i = self.next_boundary(i);
        }
        while i < len && self.char_at(i).is_some_and(|c| !c.is_whitespace()) {
            i = self.next_boundary(i);
        }
        self.cursor = i;
    }

    /// Delete the word before the caret (Ctrl-W).
    pub fn kill_word(&mut self) {
        let end = self.cursor;
        self.word_left();
        self.text.replace_range(self.cursor..end, "");
    }

    /// Delete from the caret to the end of the line (Ctrl-K).
    pub fn kill_to_end(&mut self) {
        let end = self.line_end(self.cursor);
        self.text.replace_range(self.cursor..end, "");
        self.release_slack();
    }

    /// Delete from the start of the line to the caret (Ctrl-U).
    pub fn kill_to_start(&mut self) {
        let start = self.line_start(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.release_slack();
    }

    /// The caret as (line index, column in chars) over the *logical* lines,
    /// which is what the renderer needs before wrapping is applied.
    pub fn line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.matches('\n').count();
        let col = before
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        (line, col)
    }

    // -- boundary helpers ---------------------------------------------------

    fn prev_boundary(&self, from: usize) -> usize {
        let mut i = from.saturating_sub(1);
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_boundary(&self, from: usize) -> usize {
        let mut i = (from + 1).min(self.text.len());
        while i < self.text.len() && !self.text.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    fn char_before(&self, at: usize) -> Option<char> {
        self.text[..at].chars().next_back()
    }

    fn char_at(&self, at: usize) -> Option<char> {
        self.text[at..].chars().next()
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map(|i| at + i)
            .unwrap_or(self.text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> Input {
        let mut i = Input::default();
        i.set(text);
        i
    }

    #[test]
    fn typing_advances_the_cursor() {
        let mut i = Input::default();
        for c in "hi".chars() {
            i.insert_char(c);
        }
        assert_eq!(i.text(), "hi");
        assert_eq!(i.cursor(), 2);
    }

    #[test]
    fn editing_happens_at_the_cursor_not_the_end() {
        let mut i = input("helo");
        i.left();
        i.insert_char('l');
        assert_eq!(i.text(), "hello");
    }

    #[test]
    fn backspace_removes_a_whole_character_not_a_byte() {
        // A caret that lands mid-codepoint panics on the next slice.
        // h é l l o ␠ — ␠ o k  — ten characters, thirteen bytes.
        let mut i = input("héllo — ok");
        assert_eq!(i.text().len(), 13);
        for _ in 0..3 {
            i.backspace();
        }
        assert_eq!(i.text(), "héllo —");
        for _ in 0..3 {
            i.backspace();
        }
        assert_eq!(i.text(), "héll");
        for _ in 0..4 {
            i.backspace();
        }
        assert_eq!(i.text(), "");
        assert_eq!(i.cursor(), 0);
    }

    #[test]
    fn arrows_step_over_multibyte_characters() {
        let mut i = input("aé…b");
        i.start();
        for _ in 0..4 {
            i.right();
        }
        assert_eq!(i.cursor(), i.text().len());
        for _ in 0..4 {
            i.left();
        }
        assert_eq!(i.cursor(), 0);
    }

    #[test]
    fn paste_inserts_everything_not_the_first_line() {
        // The reported bug: pasted content arrived truncated.
        let mut i = Input::default();
        let payload = "line one\nline two\nline three";
        i.insert_str(payload);
        assert_eq!(i.text(), payload);
        assert_eq!(i.cursor(), payload.len());
    }

    #[test]
    fn paste_lands_at_the_cursor() {
        let mut i = input("ab");
        i.left();
        i.insert_str("XY");
        assert_eq!(i.text(), "aXYb");
    }

    #[test]
    fn paste_normalises_crlf() {
        let mut i = Input::default();
        i.insert_str("a\r\nb\rc");
        assert_eq!(i.text(), "a\nb\nc");
    }

    #[test]
    fn paste_preserves_a_large_payload_byte_for_byte() {
        let payload: String = (0..500).map(|n| format!("line {n}\n")).collect();
        let mut i = Input::default();
        i.insert_str(&payload);
        assert_eq!(i.text().len(), payload.len());
        assert_eq!(i.text().lines().count(), 500);
    }

    #[test]
    fn home_and_end_work_per_line() {
        let mut i = input("first\nsecond");
        i.home();
        assert_eq!(i.cursor(), 6);
        i.end();
        assert_eq!(i.cursor(), 12);
        i.start();
        assert_eq!(i.cursor(), 0);
        i.end();
        assert_eq!(i.cursor(), 5);
    }

    #[test]
    fn word_motion_skips_whitespace_then_the_word() {
        let mut i = input("alpha beta gamma");
        i.word_left();
        assert_eq!(&i.text()[i.cursor()..], "gamma");
        i.word_left();
        assert_eq!(&i.text()[i.cursor()..], "beta gamma");
        i.word_right();
        assert_eq!(&i.text()[..i.cursor()], "alpha beta");
    }

    #[test]
    fn kill_word_deletes_the_word_before_the_caret() {
        let mut i = input("cargo test --workspace");
        i.kill_word();
        assert_eq!(i.text(), "cargo test ");
        i.kill_word();
        assert_eq!(i.text(), "cargo ");
    }

    #[test]
    fn kill_to_end_and_start_respect_line_boundaries() {
        let mut i = input("keep\ndrop this");
        i.set("keep\ndrop this");
        i.home();
        i.kill_to_end();
        assert_eq!(i.text(), "keep\n");

        let mut i = input("prefix suffix");
        i.end();
        i.kill_to_start();
        assert_eq!(i.text(), "");
    }

    #[test]
    fn line_col_tracks_the_caret_across_lines() {
        let mut i = input("ab\ncdé");
        assert_eq!(i.line_col(), (1, 3));
        i.start();
        assert_eq!(i.line_col(), (0, 0));
    }

    #[test]
    fn take_empties_the_buffer_and_resets_the_caret() {
        let mut i = input("prompt");
        assert_eq!(i.take(), "prompt");
        assert!(i.is_empty());
        assert_eq!(i.cursor(), 0);
    }

    #[test]
    fn a_huge_paste_is_not_held_after_it_is_deleted() {
        // Regression: pasting a 500 KB file and clearing it left the buffer's
        // capacity allocated for the rest of the session.
        let mut i = Input::default();
        i.insert_str(&"x".repeat(500_000));
        assert!(i.heap_bytes() >= 500_000);
        i.clear();
        assert!(
            i.heap_bytes() <= KEEP_CAPACITY,
            "kept {} bytes",
            i.heap_bytes()
        );
    }

    #[test]
    fn killing_a_huge_line_releases_it_too() {
        let mut i = Input::default();
        i.insert_str(&"y".repeat(500_000));
        i.kill_to_start();
        assert!(i.heap_bytes() <= KEEP_CAPACITY, "{}", i.heap_bytes());

        let mut i = Input::default();
        i.insert_str(&"z".repeat(500_000));
        i.start();
        i.kill_to_end();
        assert!(i.heap_bytes() <= KEEP_CAPACITY, "{}", i.heap_bytes());
    }

    #[test]
    fn sending_a_prompt_leaves_no_capacity_behind() {
        let mut i = Input::default();
        i.insert_str(&"w".repeat(500_000));
        let sent = i.take();
        assert_eq!(sent.len(), 500_000);
        assert_eq!(i.heap_bytes(), 0, "the allocation went with the prompt");
    }

    #[test]
    fn ordinary_typing_does_not_thrash_the_allocation() {
        // Shrinking on every keystroke would be worse than keeping slack.
        let mut i = Input::default();
        i.insert_str("a normal prompt");
        let before = i.heap_bytes();
        i.kill_to_start();
        assert_eq!(i.heap_bytes(), before, "small buffers are left alone");
    }

    #[test]
    fn motion_at_the_edges_is_a_no_op_not_a_panic() {
        let mut i = Input::default();
        i.left();
        i.right();
        i.backspace();
        i.delete();
        i.word_left();
        i.word_right();
        i.kill_word();
        i.kill_to_end();
        i.kill_to_start();
        assert!(i.is_empty());
    }
}
