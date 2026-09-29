//! Content search on ripgrep's engine (FR-SEARCH-01..08).
//!
//! The walk is parallel (the `ignore` crate's walker, std threads); results
//! are gathered on the calling thread and sorted, so the output never depends
//! on thread scheduling (FR-SEARCH-05). Cancellation: the caller's `cancel`
//! closure is not `Sync`, so the calling thread polls it while draining
//! results and flips an atomic that every worker checks.

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use domain::{CaseMode, GrepFileHit, GrepLine, GrepOutcome, GrepQuery};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::WalkState;

use crate::{DiscoveryFilter, SearchError};

/// Stop counting past this many matching lines; the footer says "N+".
pub const MATCH_HARD_CAP: u64 = 100_000;
/// Longest line text kept in memory; a minified file's one line can be
/// megabytes. The window keeps the match in view (FR-SEARCH-04).
const MAX_KEPT_LINE: usize = 1_024;

fn matcher(q: &GrepQuery) -> Result<RegexMatcher, SearchError> {
    let mut b = RegexMatcherBuilder::new();
    b.case_smart(q.case == CaseMode::Smart)
        .case_insensitive(q.case == CaseMode::Insensitive)
        .fixed_strings(q.literal)
        .multi_line(q.multiline)
        .dot_matches_new_line(q.multiline)
        // A pattern that compiles to a huge automaton is refused rather
        // than allowed to eat memory (FR-SEARCH-07).
        .size_limit(10 << 20)
        .dfa_size_limit(10 << 20);
    if !q.multiline {
        b.line_terminator(Some(b'\n'));
    }
    b.build(&q.pattern)
        .map_err(|e| SearchError::Pattern(e.to_string()))
}

fn floor_char(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Keep at most `MAX_KEPT_LINE` bytes of `line` around byte column `col`,
/// marking cuts with `…`, and return the column within the kept text.
fn window(line: &str, col: usize) -> (String, u32) {
    if line.len() <= MAX_KEPT_LINE {
        return (line.to_string(), u32::try_from(col).unwrap_or(0));
    }
    let start = floor_char(line, col.saturating_sub(MAX_KEPT_LINE / 3));
    let end = floor_char(line, start + MAX_KEPT_LINE);
    let mut out = String::with_capacity(MAX_KEPT_LINE + 8);
    let mut new_col = col - start;
    if start > 0 {
        out.push('…');
        new_col += '…'.len_utf8();
    }
    out.push_str(&line[start..end]);
    if end < line.len() {
        out.push('…');
    }
    (out, u32::try_from(new_col).unwrap_or(0))
}

struct Collect<'m> {
    matcher: &'m RegexMatcher,
    keep_lines: usize,
    lines: Vec<GrepLine>,
    matches: u32,
}

fn text_of(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.trim_end_matches(['\n', '\r']).to_string()
}

impl Sink for Collect<'_> {
    type Error = io::Error;

    fn matched(&mut self, _s: &Searcher, m: &SinkMatch<'_>) -> Result<bool, io::Error> {
        self.matches = self.matches.saturating_add(1);
        if self.lines.len() < self.keep_lines {
            let col = self
                .matcher
                .find(m.bytes())
                .ok()
                .flatten()
                .map_or(0, |found| found.start());
            let (text, match_col) = window(&text_of(m.bytes()), col);
            self.lines.push(GrepLine {
                line: m
                    .line_number()
                    .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
                text,
                is_context: false,
                match_col,
            });
        }
        Ok(true)
    }

    fn context(&mut self, _s: &Searcher, c: &SinkContext<'_>) -> Result<bool, io::Error> {
        if self.lines.len() < self.keep_lines {
            let (text, _) = window(&text_of(c.bytes()), 0);
            self.lines.push(GrepLine {
                line: c
                    .line_number()
                    .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
                text,
                is_context: true,
                match_col: 0,
            });
        }
        Ok(true)
    }
}

enum Found {
    Hit(GrepFileHit),
    Searched,
    SkippedLarge,
}

pub(crate) fn grep(
    filter: &DiscoveryFilter,
    q: &GrepQuery,
    cancel: &dyn Fn() -> bool,
) -> Result<GrepOutcome, SearchError> {
    if !q.root.exists() {
        return Err(SearchError::Io {
            path: filter.relative(&q.root),
            source: io::Error::new(io::ErrorKind::NotFound, "no such file or directory"),
        });
    }
    let matcher = matcher(q)?;
    let mut walker = filter.walker(&q.root);
    if !q.types.is_empty() {
        let mut types = ignore::types::TypesBuilder::new();
        types.add_defaults();
        for t in q.types.iter() {
            types.select(t);
        }
        walker.types(types.build().map_err(|e| {
            SearchError::Pattern(format!("{e} (types are ripgrep's, e.g. rust, go, ts, py)"))
        })?);
    }
    // `glob` narrows discovery; it never widens it. The walker's overrides
    // would be the obvious tool, but in the `ignore` crate an override glob
    // *beats* `.gitignore` (ripgrep's `-g`), so `glob: ["*.log"]` would
    // re-admit a gitignored log. Filter the discovered files instead.
    let globs = if q.globs.is_empty() {
        None
    } else {
        Some(crate::compile_globs(&q.globs)?)
    };
    let glob_root = if q.root.is_dir() {
        q.root.clone()
    } else {
        q.root.parent().map(Into::into).unwrap_or_default()
    };
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    walker.threads(threads);

    let stop = AtomicBool::new(false);
    let total_matches = AtomicU64::new(0);
    let files_with_hits = AtomicU64::new(0);
    let keep_lines = q.max_lines_per_file.max(1) * (1 + 2 * usize::from(q.context));
    let (tx, rx) = mpsc::channel::<Found>();

    // Keyed by path components so the order matches every other listing in
    // this crate (`a/b` before `a-c`), whatever order the threads finished in.
    let mut hits: BTreeMap<Vec<String>, GrepFileHit> = BTreeMap::new();
    let (mut searched, mut skipped_large) = (0u64, 0u64);
    let mut cancelled = false;

    std::thread::scope(|scope| {
        let walk = walker.build_parallel();
        let (stop, total_matches, files_with_hits, matcher, globs, glob_root) = (
            &stop,
            &total_matches,
            &files_with_hits,
            &matcher,
            &globs,
            &glob_root,
        );
        scope.spawn(move || {
            walk.run(|| {
                let tx = tx.clone();
                let mut searcher: Searcher = SearcherBuilder::new()
                    .line_number(true)
                    .binary_detection(BinaryDetection::quit(b'\0'))
                    .before_context(usize::from(q.context))
                    .after_context(usize::from(q.context))
                    .multi_line(q.multiline)
                    .build();
                Box::new(move |result| {
                    if stop.load(Ordering::Relaxed) {
                        return WalkState::Quit;
                    }
                    let Ok(entry) = result else {
                        return WalkState::Continue;
                    };
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        return WalkState::Continue;
                    }
                    if let Some(set) = globs {
                        if !set.is_match(crate::relative_to(glob_root, entry.path())) {
                            return WalkState::Continue;
                        }
                    }
                    if entry.metadata().is_ok_and(|m| m.len() > q.max_file_bytes) {
                        let _ = tx.send(Found::SkippedLarge);
                        return WalkState::Continue;
                    }
                    // Past the file cap, keep counting but stop keeping lines.
                    let collecting = files_with_hits.load(Ordering::Relaxed) < q.max_files as u64;
                    let mut sink = Collect {
                        matcher,
                        keep_lines: if collecting { keep_lines } else { 0 },
                        lines: Vec::new(),
                        matches: 0,
                    };
                    let _ = searcher.search_path(matcher, entry.path(), &mut sink);
                    if sink.matches == 0 {
                        let _ = tx.send(Found::Searched);
                        return WalkState::Continue;
                    }
                    files_with_hits.fetch_add(1, Ordering::Relaxed);
                    let total = total_matches.fetch_add(u64::from(sink.matches), Ordering::Relaxed)
                        + u64::from(sink.matches);
                    let _ = tx.send(Found::Hit(GrepFileHit {
                        path: filter.relative(entry.path()),
                        count: sink.matches,
                        lines: sink.lines.into_boxed_slice(),
                    }));
                    if total >= MATCH_HARD_CAP {
                        stop.store(true, Ordering::Relaxed);
                        return WalkState::Quit;
                    }
                    WalkState::Continue
                })
            });
            drop(tx);
        });
        // Drain on this thread, the only one allowed to call `cancel`.
        loop {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Found::Hit(hit)) => {
                    searched += 1;
                    let key = hit.path.split('/').map(str::to_string).collect();
                    hits.insert(key, hit);
                }
                Ok(Found::Searched) => searched += 1,
                Ok(Found::SkippedLarge) => skipped_large += 1,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if !cancelled && cancel() {
                cancelled = true;
                stop.store(true, Ordering::Relaxed);
            }
        }
    });

    let total_matches = total_matches.into_inner();
    let total_files = hits.len() as u64;
    Ok(GrepOutcome {
        files: hits.into_values().collect(),
        total_files,
        total_matches,
        files_searched: searched,
        skipped_large,
        partial: cancelled || total_matches >= MATCH_HARD_CAP,
    })
}
