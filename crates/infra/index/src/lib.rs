//! The code index (PRD-CTX-EFF-003 §5.4, technical plan §7.2).
//!
//! A syntactic index of the project — definitions, imports and identifier
//! occurrences per file — built by tree-sitter on a background thread, kept
//! in a small binary store under `.zcode/index/v1/`, and kept fresh on
//! zcode's own writes, on query, and by a stat-only rescan at turn start.
//!
//! Nothing here blocks the first model request (FR-INDEX-01): [`spawn`]
//! returns at once and every query answers from whatever has been indexed
//! so far, reporting [`IndexState::Building`] until the walk completes.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

pub mod extract;
mod graph;
pub mod lang;
mod resolve;
mod snapshot;
mod store;

#[cfg(test)]
mod extract_tests;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};
use std::time::{Duration, Instant};

use domain::{
    BoxError, CodeIndexPort, IndexState, ParsedFile, Related, SearchPort, SymbolDef, SymbolKind,
};
use infra_search::heuristics;

use crate::extract::{Extracted, ParseError};
use crate::lang::Lang;
use crate::snapshot::{fnv1a, Snapshot, Stamp, Update};

pub use crate::snapshot::Skip;

/// Files are published to readers in batches this size, so a query during
/// the build sees partial results without contending on every file.
const BATCH: usize = 200;
/// A dirty index is saved at most this often outside the build.
const SAVE_EVERY: Duration = Duration::from_secs(30);
/// Turn-start rescans closer together than this are skipped.
const RESCAN_EVERY: Duration = Duration::from_secs(2);
/// References listed by `related`.
const MAX_REFERENCES: usize = 100;

/// Where the index lives and what it may spend.
#[derive(Clone, Debug)]
pub struct IndexOptions {
    pub root: PathBuf,
    pub store_path: PathBuf,
    /// Files larger than this are recorded as skipped (FR-INDEX-11).
    pub max_file_bytes: u64,
    /// Longest a single parse may take (FR-INDEX-11).
    pub parse_budget: Duration,
    /// Below this many indexed files there is no repo map: a small project
    /// is cheaper to list than to summarise (PRD §14.2 Q2).
    pub repo_map_min_files: usize,
    /// How long a session start waits for a building index before taking
    /// a partial map (CE-DQ22).
    pub repo_map_wait: Duration,
}

impl IndexOptions {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            store_path: default_store_path(root),
            max_file_bytes: 1_000_000,
            parse_budget: Duration::from_millis(500),
            repo_map_min_files: 200,
            repo_map_wait: Duration::from_millis(1_500),
        }
    }
}

pub fn default_store_path(root: &Path) -> PathBuf {
    root.join(".zcode")
        .join("index")
        .join("v1")
        .join("index.zcix")
}

/// What a build did, for telemetry and `zcode index rebuild`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildReport {
    pub files: u32,
    pub symbols: u32,
    pub parsed: u32,
    pub reused: u32,
    pub skipped: BTreeMap<&'static str, u32>,
    pub elapsed_ms: u64,
}

/// A summary of what the index holds (`zcode index status`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub files: u32,
    pub symbols: u32,
    pub languages: BTreeMap<&'static str, u32>,
    pub skipped: BTreeMap<&'static str, u32>,
}

type ReadyHook = Box<dyn FnOnce(&BuildReport) + Send>;

const BUILDING: u8 = 0;
const READY: u8 = 1;

pub struct CodeIndex {
    opts: IndexOptions,
    root_canon: Option<PathBuf>,
    search: Arc<dyn SearchPort>,
    snap: RwLock<Snapshot>,
    phase: AtomicU8,
    done: AtomicU32,
    total: AtomicU32,
    dirty: AtomicBool,
    last_save: Mutex<Instant>,
    parses: AtomicU64,
    rescanning: AtomicBool,
    last_rescan: Mutex<Option<Instant>>,
    go_module: OnceLock<Option<String>>,
    on_ready: Mutex<Option<ReadyHook>>,
    me: Weak<CodeIndex>,
}

/// Start indexing `opts.root` in the background and return at once.
/// `on_ready` runs on the index thread when the first build completes.
pub fn spawn(
    opts: IndexOptions,
    search: Arc<dyn SearchPort>,
    on_ready: Option<ReadyHook>,
) -> Arc<CodeIndex> {
    let index = CodeIndex::open(opts, search);
    if let Ok(mut hook) = index.on_ready.lock() {
        *hook = on_ready;
    }
    let worker = index.clone();
    let spawned = std::thread::Builder::new()
        .name("zcode-index".into())
        .spawn(move || {
            worker.build();
        });
    if let Err(e) = spawned {
        log::warn!("code index disabled: cannot start its thread: {e}");
        index.phase.store(READY, Ordering::Release);
    }
    index
}

fn read(lock: &RwLock<Snapshot>) -> RwLockReadGuard<'_, Snapshot> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write(lock: &RwLock<Snapshot>) -> RwLockWriteGuard<'_, Snapshot> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

/// `.` and `::` are the same separator for matching purposes.
fn norm_qualified(q: &str) -> String {
    q.replace("::", ".")
}

fn leaf(q: &str) -> &str {
    let q = q.rsplit("::").next().unwrap_or(q);
    q.rsplit('.').next().unwrap_or(q)
}

fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut hay = hay.chars();
    needle.chars().all(|n| hay.any(|h| h == n))
}

/// How well a definition name matches a query: lower is better.
fn tier(query: &str, query_lower: &str, name: &str) -> Option<u8> {
    if name == query {
        return Some(0);
    }
    let lower = name.to_lowercase();
    if lower == query_lower {
        Some(1)
    } else if lower.starts_with(query_lower) {
        Some(2)
    } else if lower.contains(query_lower) {
        Some(3)
    } else if is_subsequence(query_lower, &lower) {
        Some(4)
    } else {
        None
    }
}

fn kind_matches(want: Option<SymbolKind>, got: SymbolKind) -> bool {
    match want {
        None => true,
        // "functions" includes methods: the model rarely knows which it has.
        Some(SymbolKind::Function) => matches!(got, SymbolKind::Function | SymbolKind::Method),
        Some(k) => k == got,
    }
}

fn under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Read and analyse one file: the heuristics of FR-FILTER-06, then a parse
/// within budget (FR-INDEX-11).
fn analyse(lang: Lang, bytes: &[u8], budget: Duration) -> (Skip, Extracted) {
    if heuristics::looks_binary(bytes) {
        return (Skip::Binary, Extracted::default());
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (Skip::Binary, Extracted::default());
    };
    if heuristics::looks_minified(text) {
        return (Skip::Minified, Extracted::default());
    }
    if heuristics::looks_generated(text) {
        return (Skip::Generated, Extracted::default());
    }
    match extract::parse(lang, text, budget) {
        Ok(x) => (Skip::None, x),
        Err(ParseError::Timeout) => (Skip::Timeout, Extracted::default()),
        Err(ParseError::NoGrammar) => (Skip::Unreadable, Extracted::default()),
    }
}

/// What to do with one walked file.
enum Step {
    Keep,
    Restamp(String, Stamp),
    Put(Update),
    Remove(String),
}

impl CodeIndex {
    /// An index over `opts.root` that does not build — for `zcode index
    /// status` and for tests that drive [`CodeIndex::build`] themselves.
    pub fn open(opts: IndexOptions, search: Arc<dyn SearchPort>) -> Arc<CodeIndex> {
        let root_canon = opts.root.canonicalize().ok();
        Arc::new_cyclic(|me| CodeIndex {
            opts,
            root_canon,
            search,
            snap: RwLock::new(Snapshot::default()),
            phase: AtomicU8::new(BUILDING),
            done: AtomicU32::new(0),
            total: AtomicU32::new(0),
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now()),
            parses: AtomicU64::new(0),
            rescanning: AtomicBool::new(false),
            last_rescan: Mutex::new(None),
            go_module: OnceLock::new(),
            on_ready: Mutex::new(None),
            me: me.clone(),
        })
    }

    pub fn options(&self) -> &IndexOptions {
        &self.opts
    }

    /// Parses performed since start — what the incremental tests count.
    pub fn parses(&self) -> u64 {
        self.parses.load(Ordering::Relaxed)
    }

    /// Estimated heap held by the index (NFR-CTX-MEM-02).
    pub fn heap_bytes(&self) -> usize {
        read(&self.snap).heap_bytes()
    }

    pub fn stats(&self) -> IndexStats {
        stats_of(&read(&self.snap))
    }

    /// A project-relative, `/`-separated path.
    fn rel(&self, path: &str) -> String {
        let path = path.replace('\\', "/");
        let p = Path::new(&path);
        let stripped = if p.is_absolute() {
            p.strip_prefix(&self.opts.root)
                .ok()
                .or_else(|| {
                    self.root_canon
                        .as_ref()
                        .and_then(|root| p.strip_prefix(root).ok())
                })
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or(path.clone())
        } else {
            path.clone()
        };
        let mut s = stripped.as_str();
        while let Some(rest) = s.strip_prefix("./") {
            s = rest;
        }
        if s == "." {
            s = "";
        }
        s.trim_end_matches('/').to_string()
    }

    fn abs(&self, rel: &str) -> PathBuf {
        self.opts.root.join(rel)
    }

    /// Decide what one file needs, reading it only when its stamp moved.
    fn step(&self, rel: &str, lang: Lang, known: Option<(Stamp, u64)>) -> Step {
        let abs = self.abs(rel);
        let Ok(meta) = std::fs::metadata(&abs) else {
            return Step::Remove(rel.to_string());
        };
        let stamp = Stamp::of(&meta);
        if known.is_some_and(|(s, _)| s == stamp) {
            return Step::Keep;
        }
        let update = |skip, hash, extracted| {
            Step::Put(Update {
                path: rel.to_string(),
                stamp,
                hash,
                lang,
                skip,
                extracted,
            })
        };
        if stamp.size > self.opts.max_file_bytes {
            return update(Skip::TooLarge, 0, Extracted::default());
        }
        let Ok(bytes) = std::fs::read(&abs) else {
            return update(Skip::Unreadable, 0, Extracted::default());
        };
        let hash = fnv1a(&bytes);
        if known.is_some_and(|(_, h)| h == hash) {
            return Step::Restamp(rel.to_string(), stamp);
        }
        self.parses.fetch_add(1, Ordering::Relaxed);
        let (skip, extracted) = analyse(lang, &bytes, self.opts.parse_budget);
        update(skip, hash, extracted)
    }

    fn known(&self, rel: &str) -> Option<(Stamp, u64)> {
        read(&self.snap).record(rel).map(|r| (r.stamp, r.hash))
    }

    fn commit(&self, steps: Vec<Step>) -> bool {
        if steps.iter().all(|s| matches!(s, Step::Keep)) {
            return false;
        }
        let mut snap = write(&self.snap);
        for s in steps {
            match s {
                Step::Keep => {}
                Step::Restamp(path, stamp) => snap.restamp(&path, stamp),
                Step::Put(u) => snap.apply(u),
                Step::Remove(path) => snap.remove(&path),
            }
        }
        drop(snap);
        self.dirty.store(true, Ordering::Release);
        true
    }

    /// Re-index one file now. Returns whether anything changed.
    fn reindex(&self, rel: &str) -> bool {
        let Some(lang) = Lang::for_path(rel) else {
            return false;
        };
        let known = self.known(rel);
        if known.is_none() && self.search.explain(&self.abs(rel)).is_some() {
            return false; // excluded from discovery: not ours to index
        }
        let changed = self.commit(vec![self.step(rel, lang, known)]);
        if changed {
            self.maybe_save();
        }
        changed
    }

    /// Re-index whichever of `paths` changed on disk since they were read
    /// (FR-INDEX-04: freshness on query).
    fn refresh(&self, paths: &HashSet<String>) -> bool {
        let mut changed = false;
        for p in paths {
            changed |= self.reindex(p);
        }
        changed
    }

    fn indexable(entry: &domain::WalkEntry) -> Option<Lang> {
        if entry.is_dir {
            return None;
        }
        let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        if heuristics::is_lockfile(name) {
            return None;
        }
        Lang::for_path(&entry.path)
    }

    /// Walk, parse what changed, drop what vanished, persist. Blocking;
    /// [`spawn`] runs it on its own thread.
    pub fn build(&self) -> BuildReport {
        let started = Instant::now();
        self.phase.store(BUILDING, Ordering::Release);
        let empty = read(&self.snap).files.is_empty();
        if empty {
            if let Some(loaded) = store::load(&self.opts.store_path) {
                *write(&self.snap) = loaded;
            }
        }
        let parses_before = self.parses();
        let walked = match self.search.walk_files(&self.opts.root) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("code index: cannot walk {}: {e}", self.opts.root.display());
                Box::new([])
            }
        };
        let files: Vec<(&str, Lang)> = walked
            .iter()
            .filter_map(|e| Self::indexable(e).map(|l| (e.path.as_str(), l)))
            .collect();
        self.total.store(files.len() as u32, Ordering::Release);
        self.done.store(0, Ordering::Release);
        let mut reused = 0u32;
        for chunk in files.chunks(BATCH) {
            let mut steps = Vec::with_capacity(chunk.len());
            for (path, lang) in chunk {
                let s = self.step(path, *lang, self.known(path));
                if matches!(s, Step::Keep | Step::Restamp(..)) {
                    reused += 1;
                }
                steps.push(s);
                self.done.fetch_add(1, Ordering::AcqRel);
                std::thread::yield_now();
            }
            self.commit(steps);
        }
        // Whatever the walk no longer offers is gone (or newly excluded).
        let seen: HashSet<&str> = files.iter().map(|(p, _)| *p).collect();
        let stale: Vec<Step> = {
            let snap = read(&self.snap);
            snap.live_files()
                .map(|r| snap.path(r))
                .filter(|p| !seen.contains(p))
                .map(|p| Step::Remove(p.to_string()))
                .collect()
        };
        self.commit(stale);
        self.phase.store(READY, Ordering::Release);
        self.save();
        let stats = self.stats();
        let report = BuildReport {
            files: stats.files,
            symbols: stats.symbols,
            parsed: (self.parses() - parses_before) as u32,
            reused,
            skipped: stats.skipped,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        log::debug!(
            "code index ready: {} files, {} symbols, {} parsed in {}ms",
            report.files,
            report.symbols,
            report.parsed,
            report.elapsed_ms
        );
        let hook = self.on_ready.lock().ok().and_then(|mut h| h.take());
        if let Some(hook) = hook {
            hook(&report);
        }
        report
    }

    /// Stat every indexed file and re-parse what changed outside zcode.
    /// No reads unless a stamp moved.
    pub fn rescan_changed(&self) -> bool {
        let Ok(walked) = self.search.walk_files(&self.opts.root) else {
            return false;
        };
        let mut steps = Vec::new();
        let mut seen = HashSet::new();
        for e in walked.iter() {
            if let Some(lang) = Self::indexable(e) {
                seen.insert(e.path.as_str());
                steps.push(self.step(&e.path, lang, self.known(&e.path)));
            }
        }
        {
            let snap = read(&self.snap);
            for r in snap.live_files() {
                let p = snap.path(r);
                if !seen.contains(p) {
                    steps.push(Step::Remove(p.to_string()));
                }
            }
        }
        let changed = self.commit(steps);
        if changed {
            self.maybe_save();
        }
        changed
    }

    /// Persist now.
    pub fn save(&self) {
        let result = store::save(&self.opts.store_path, &read(&self.snap));
        match result {
            Ok(()) => {
                self.dirty.store(false, Ordering::Release);
                if let Ok(mut at) = self.last_save.lock() {
                    *at = Instant::now();
                }
            }
            Err(e) => log::warn!(
                "code index not saved to {}: {e}",
                self.opts.store_path.display()
            ),
        }
    }

    fn maybe_save(&self) {
        let due = self
            .last_save
            .lock()
            .map(|at| at.elapsed() >= SAVE_EVERY)
            .unwrap_or(false);
        if due && self.phase.load(Ordering::Acquire) == READY {
            self.save();
        }
    }

    /// Resolution context over the live paths. Built per call: it borrows
    /// the snapshot, and a `related` query is rare next to a lookup.
    fn go_module(&self) -> Option<String> {
        self.go_module
            .get_or_init(|| {
                std::fs::read_to_string(self.abs("go.mod"))
                    .ok()
                    .and_then(|s| resolve::go_module(&s))
            })
            .clone()
    }

    fn collect_symbols(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Vec<SymbolDef> {
        let snap = read(&self.snap);
        let wanted_leaf = leaf(query);
        let qualified = (wanted_leaf.len() != query.len()).then(|| norm_qualified(query));
        let lower = wanted_leaf.to_lowercase();
        let mut hits: Vec<(u8, SymbolDef)> = Vec::new();
        for (name, slots) in &snap.by_name {
            let Some(t) = tier(wanted_leaf, &lower, name) else {
                continue;
            };
            for (slot, def) in slots {
                let Some(d) = snap.symbol(*slot, *def) else {
                    continue;
                };
                if !kind_matches(kind, d.kind) {
                    continue;
                }
                if path_prefix.is_some_and(|p| !under(&d.path, p)) {
                    continue;
                }
                if let Some(q) = &qualified {
                    let full = norm_qualified(&d.qualified);
                    if !(full == *q || full.ends_with(&format!(".{q}"))) {
                        continue;
                    }
                }
                hits.push((t, d));
            }
        }
        hits.sort_by(|(ta, a), (tb, b)| {
            ta.cmp(tb)
                .then_with(|| infra_search::path_order(&a.path, &b.path))
                .then(a.span.start_line.cmp(&b.span.start_line))
        });
        hits.truncate(limit);
        hits.into_iter().map(|(_, d)| d).collect()
    }

    fn collect_locate(&self, path: Option<&str>, qualified: &str) -> Vec<SymbolDef> {
        let snap = read(&self.snap);
        let q = norm_qualified(qualified);
        let suffix = format!(".{q}");
        let Some(slots) = snap.by_name.get(leaf(qualified)) else {
            return Vec::new();
        };
        let mut exact = Vec::new();
        let mut partial = Vec::new();
        for (slot, def) in slots {
            let Some(d) = snap.symbol(*slot, *def) else {
                continue;
            };
            if path.is_some_and(|p| !under(&d.path, p)) {
                continue;
            }
            let full = norm_qualified(&d.qualified);
            if full == q {
                exact.push(d);
            } else if full.ends_with(&suffix) {
                partial.push(d);
            }
        }
        let mut out = if exact.is_empty() { partial } else { exact };
        out.sort_by(|a, b| {
            infra_search::path_order(&a.path, &b.path)
                .then(a.span.start_line.cmp(&b.span.start_line))
        });
        out
    }

    fn file_outline(&self, rel: &str) -> Result<Option<Vec<SymbolDef>>, BoxError> {
        let Some(lang) = Lang::for_path(rel) else {
            return Ok(None);
        };
        self.reindex(rel);
        let stored = {
            let snap = read(&self.snap);
            match snap.record(rel) {
                Some(rec) if rec.skip != Skip::None => {
                    return Err(format!("{rel}: not indexed ({})", rec.skip.as_str()).into());
                }
                Some(rec) => Some(
                    rec.defs
                        .iter()
                        .map(|d| snap.to_symbol(rec, d))
                        .collect::<Vec<_>>(),
                ),
                None => None,
            }
        };
        if let Some(defs) = stored {
            return Ok(Some(self.with_signatures(defs)));
        }
        // Not in the index (excluded from discovery, or not reached yet):
        // parse it on demand, without storing it.
        let text = std::fs::read_to_string(self.abs(rel)).map_err(|e| format!("{rel}: {e}"))?;
        let parsed = extract::parse(lang, &text, self.opts.parse_budget)
            .map_err(|e| format!("{rel}: {e:?}"))?;
        Ok(Some(
            parsed
                .defs
                .into_iter()
                .map(|d| SymbolDef {
                    path: rel.to_string(),
                    ..d
                })
                .collect(),
        ))
    }

    fn dir_outline(&self, rel: &str) -> Vec<SymbolDef> {
        let snap = read(&self.snap);
        let mut out: Vec<SymbolDef> = snap
            .live_files()
            .filter(|r| {
                let p = snap.path(r);
                let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
                dir == rel
            })
            .flat_map(|r| {
                r.defs
                    .iter()
                    .filter(|d| d.depth == 0)
                    .map(|d| snap.to_symbol(r, d))
                    .collect::<Vec<_>>()
            })
            .collect();
        out.sort_by(|a, b| {
            infra_search::path_order(&a.path, &b.path)
                .then(a.span.start_line.cmp(&b.span.start_line))
        });
        out
    }

    fn file_related(&self, rel: &str) -> Related {
        let snap = read(&self.snap);
        let go_module = self.go_module();
        let ctx = resolve::Ctx::new(snap.live_files().map(|r| snap.path(r)), go_module);
        let mut related = Related::default();
        if let Some(rec) = snap.record(rel) {
            for spec in rec.imports.iter() {
                let spec = snap.strings.get(*spec);
                related.imports.push(
                    resolve::resolve(rel, rec.lang, spec, &ctx).unwrap_or_else(|| spec.to_string()),
                );
            }
        }
        let dir = rel
            .rsplit_once('/')
            .map_or(String::new(), |(d, _)| format!("{d}/"));
        for other in snap.live_files() {
            let from = snap.path(other);
            if from == rel {
                continue;
            }
            let imports_me = other.imports.iter().any(|spec| {
                resolve::resolve(from, other.lang, snap.strings.get(*spec), &ctx)
                    .is_some_and(|to| to == rel || (other.lang == Lang::Go && to == dir))
            });
            if imports_me {
                related.imported_by.push(from.to_string());
            }
        }
        related
            .imported_by
            .sort_by(|a, b| infra_search::path_order(a, b));
        related
    }

    fn symbol_related(&self, target: &str) -> Related {
        let defs = self.collect_locate(None, target);
        let snap = read(&self.snap);
        let mut related = Related {
            defined_in: defs
                .iter()
                .map(|d| format!("{}:{}", d.path, d.name_line))
                .collect(),
            ..Related::default()
        };
        let Some(name) = snap.strings.lookup(leaf(target)) else {
            return related;
        };
        let definitions: HashSet<(&str, u32)> = defs
            .iter()
            .map(|d| (d.path.as_str(), d.name_line))
            .collect();
        let mut refs: Vec<(&str, u32)> = Vec::new();
        for rec in snap.live_files() {
            let path = snap.path(rec);
            for (n, line) in rec.idents.iter() {
                if *n == name && !definitions.contains(&(path, *line)) {
                    refs.push((path, *line));
                }
            }
        }
        refs.sort_by(|a, b| infra_search::path_order(a.0, b.0).then(a.1.cmp(&b.1)));
        related.referenced_in = refs
            .into_iter()
            .take(MAX_REFERENCES)
            .map(|(p, l)| format!("{p}:{l}"))
            .collect();
        related
    }

    fn paths_of(defs: &[SymbolDef]) -> HashSet<String> {
        defs.iter().map(|d| d.path.clone()).collect()
    }

    /// Fill in signatures from the files themselves, one read per file.
    fn with_signatures(&self, mut defs: Vec<SymbolDef>) -> Vec<SymbolDef> {
        let mut cache: Option<(String, String)> = None;
        for d in defs.iter_mut().filter(|d| d.signature.is_empty()) {
            if cache.as_ref().is_none_or(|(p, _)| *p != d.path) {
                let text = std::fs::read_to_string(self.abs(&d.path)).unwrap_or_default();
                cache = Some((d.path.clone(), text));
            }
            if let Some((_, text)) = &cache {
                d.signature = extract::signature_at(text, d.name_line);
            }
        }
        defs
    }
}

fn stats_of(snap: &Snapshot) -> IndexStats {
    let mut stats = IndexStats::default();
    for r in snap.live_files() {
        stats.files += 1;
        stats.symbols += r.defs.len() as u32;
        *stats.languages.entry(r.lang.as_str()).or_default() += 1;
        if r.skip != Skip::None {
            *stats.skipped.entry(r.skip.as_str()).or_default() += 1;
        }
    }
    stats
}

/// What a store on disk holds, without building (`zcode index status`).
pub fn stored_stats(store_path: &Path) -> Option<IndexStats> {
    store::load(store_path).map(|snap| stats_of(&snap))
}

impl Drop for CodeIndex {
    fn drop(&mut self) {
        if self.dirty.load(Ordering::Acquire) && self.phase.load(Ordering::Acquire) == READY {
            self.save();
        }
    }
}

impl CodeIndexPort for CodeIndex {
    fn state(&self) -> IndexState {
        if self.phase.load(Ordering::Acquire) == READY {
            IndexState::Ready
        } else {
            IndexState::Building {
                done: self.done.load(Ordering::Acquire),
                total: self.total.load(Ordering::Acquire),
            }
        }
    }

    fn outline(&self, path: &str) -> Result<Option<Vec<SymbolDef>>, BoxError> {
        let rel = self.rel(path);
        let abs = self.abs(&rel);
        if rel.is_empty() || abs.is_dir() {
            return Ok(Some(self.with_signatures(self.dir_outline(&rel))));
        }
        if !abs.exists() {
            if Lang::for_path(&rel).is_some() {
                self.reindex(&rel); // drop a record for a deleted file
            }
            return Err(format!("{rel}: no such file or directory").into());
        }
        self.file_outline(&rel)
    }

    fn symbols(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SymbolDef>, BoxError> {
        let prefix = path_prefix.map(|p| self.rel(p));
        let prefix = prefix.as_deref();
        let mut hits = self.collect_symbols(query, kind, prefix, limit);
        if self.refresh(&Self::paths_of(&hits)) {
            hits = self.collect_symbols(query, kind, prefix, limit);
        }
        Ok(self.with_signatures(hits))
    }

    fn locate(&self, path: Option<&str>, qualified: &str) -> Result<Vec<SymbolDef>, BoxError> {
        let rel = path.map(|p| self.rel(p));
        if let Some(r) = &rel {
            // The file named is the one the caller cares about: make sure
            // it is current even if nothing matched in the stale record.
            self.reindex(r);
        }
        let mut hits = self.collect_locate(rel.as_deref(), qualified);
        if self.refresh(&Self::paths_of(&hits)) {
            hits = self.collect_locate(rel.as_deref(), qualified);
        }
        Ok(self.with_signatures(hits))
    }

    fn related(&self, target: &str) -> Result<Related, BoxError> {
        let rel = self.rel(target);
        if Lang::for_path(&rel).is_some() && self.abs(&rel).is_file() {
            self.reindex(&rel);
            return Ok(self.file_related(&rel));
        }
        Ok(self.symbol_related(target))
    }

    fn parse_text(&self, path: &str, text: &str) -> Result<Option<ParsedFile>, BoxError> {
        let rel = self.rel(path);
        let Some(lang) = Lang::for_path(&rel) else {
            return Ok(None);
        };
        // An edit gate may run on a file the build skipped for time; give
        // it more room than a background parse gets.
        let parsed = extract::parse(lang, text, self.opts.parse_budget * 4)
            .map_err(|e| format!("{rel}: parse failed ({e:?})"))?;
        Ok(Some(ParsedFile {
            defs: parsed
                .defs
                .into_iter()
                .map(|d| SymbolDef {
                    path: rel.clone(),
                    ..d
                })
                .collect(),
            error_nodes: parsed.error_nodes,
            first_error: parsed.first_error,
        }))
    }

    fn repo_map(&self, prompt: &str, budget_tokens: u32) -> Result<String, BoxError> {
        if budget_tokens == 0 {
            return Ok(String::new());
        }
        let deadline = Instant::now() + self.opts.repo_map_wait;
        while self.phase.load(Ordering::Acquire) != READY && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let partial = self.phase.load(Ordering::Acquire) != READY;
        let snap = read(&self.snap);
        let indexed = snap.live_files().filter(|r| r.skip == Skip::None).count();
        if indexed < self.opts.repo_map_min_files {
            return Ok(String::new());
        }
        let ranked = graph::rank(&snap, prompt);
        let signatures = |path: &str, lines: &[u32]| -> Vec<String> {
            let text = std::fs::read_to_string(self.abs(path)).unwrap_or_default();
            lines
                .iter()
                .map(|l| extract::signature_at(&text, *l))
                .filter(|s| !s.is_empty())
                .collect()
        };
        Ok(graph::render(
            &snap,
            &ranked,
            budget_tokens,
            partial,
            &signatures,
        ))
    }

    fn notify_changed(&self, path: &str) {
        let rel = self.rel(path);
        self.reindex(&rel);
    }

    fn notify_turn_start(&self) {
        if self.phase.load(Ordering::Acquire) != READY {
            return; // the build itself will see every change
        }
        {
            let Ok(mut last) = self.last_rescan.lock() else {
                return;
            };
            if last.is_some_and(|at| at.elapsed() < RESCAN_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        if self.rescanning.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(me) = self.me.upgrade() else {
            self.rescanning.store(false, Ordering::Release);
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("zcode-index-rescan".into())
            .spawn(move || {
                me.rescan_changed();
                me.rescanning.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.rescanning.store(false, Ordering::Release);
        }
    }
}
