//! The in-memory index: interned strings, one record per file, and a
//! sorted name table kept current on every insert (technical plan §7.2).
//!
//! Memory is the constraint (NFR-CTX-MEM-02): every string is interned once
//! and records hold `u32` ids, so a name that occurs in a thousand files
//! costs its bytes once.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use domain::{Span, SymbolDef, SymbolKind};

use crate::extract::Extracted;
use crate::lang::Lang;

/// String table. `Arc<str>` so the lookup map and the table share bytes.
#[derive(Default)]
pub(crate) struct Interner {
    strings: Vec<Arc<str>>,
    ids: HashMap<Arc<str>, u32>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(id) = self.ids.get(s) {
            return *id;
        }
        let id = self.strings.len() as u32;
        let arc: Arc<str> = Arc::from(s);
        self.strings.push(arc.clone());
        self.ids.insert(arc, id);
        id
    }

    pub fn get(&self, id: u32) -> &str {
        self.strings.get(id as usize).map_or("", |s| s)
    }

    pub fn arc(&self, id: u32) -> Arc<str> {
        self.strings
            .get(id as usize)
            .cloned()
            .unwrap_or_else(|| Arc::from(""))
    }

    pub fn lookup(&self, s: &str) -> Option<u32> {
        self.ids.get(s).copied()
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    fn heap_bytes(&self) -> usize {
        // Each string once, plus a table slot and a map slot per string.
        self.strings.iter().map(|s| s.len() + 16).sum::<usize>()
            + self.strings.capacity() * 16
            + self.ids.capacity() * 24
    }
}

/// Why a file has no definitions recorded (FR-INDEX-11, FR-FILTER-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Skip {
    None,
    TooLarge,
    Timeout,
    Binary,
    Minified,
    Generated,
    Unreadable,
}

impl Skip {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Skip> {
        Some(match code {
            0 => Skip::None,
            1 => Skip::TooLarge,
            2 => Skip::Timeout,
            3 => Skip::Binary,
            4 => Skip::Minified,
            5 => Skip::Generated,
            6 => Skip::Unreadable,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Skip::None => "indexed",
            Skip::TooLarge => "too large",
            Skip::Timeout => "parse timeout",
            Skip::Binary => "binary",
            Skip::Minified => "minified",
            Skip::Generated => "generated",
            Skip::Unreadable => "unreadable",
        }
    }
}

pub(crate) const KINDS: [SymbolKind; 11] = [
    SymbolKind::Function,
    SymbolKind::Method,
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Trait,
    SymbolKind::Interface,
    SymbolKind::Class,
    SymbolKind::Type,
    SymbolKind::Const,
    SymbolKind::Module,
    SymbolKind::Impl,
];

pub(crate) fn kind_code(kind: SymbolKind) -> u8 {
    KINDS.iter().position(|k| *k == kind).unwrap_or(0) as u8
}

/// A definition with its strings interned. The signature is not kept:
/// it is the name's line, and storing 80k of them was a third of the heap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Def {
    pub name: u32,
    pub qualified: u32,
    pub kind: SymbolKind,
    pub depth: u8,
    pub span: Span,
    pub body: Option<Span>,
    pub name_line: u32,
    pub name_col: u32,
}

/// Where a file stood when it was read — what freshness compares against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub mtime_ns: u64,
    pub size: u64,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Stamp {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        Stamp {
            mtime_ns,
            size: meta.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileRecord {
    pub path: u32,
    pub stamp: Stamp,
    /// FNV-1a of the content, so a touch without a change is not a re-parse.
    pub hash: u64,
    pub lang: Lang,
    pub skip: Skip,
    pub error_nodes: u32,
    pub defs: Box<[Def]>,
    pub imports: Box<[u32]>,
    /// (identifier, line), deduplicated.
    pub idents: Box<[(u32, u32)]>,
    /// False once the file is gone; its slot is kept so ids stay stable.
    pub live: bool,
}

impl FileRecord {
    fn heap_bytes(&self) -> usize {
        self.defs.len() * std::mem::size_of::<Def>()
            + self.imports.len() * 4
            + self.idents.len() * 8
    }
}

/// A parse result for one file, not yet interned.
pub(crate) struct Update {
    pub path: String,
    pub stamp: Stamp,
    pub hash: u64,
    pub lang: Lang,
    pub skip: Skip,
    pub extracted: Extracted,
}

/// The index proper. Record slots are stable: a file keeps its slot for the
/// life of the process, so `(file, def)` pairs in `by_name` stay valid.
#[derive(Default)]
pub(crate) struct Snapshot {
    pub strings: Interner,
    pub files: Vec<FileRecord>,
    pub by_path: HashMap<u32, u32>,
    /// Definition name → (file slot, def index), for exact and prefix lookup.
    pub by_name: BTreeMap<Arc<str>, Vec<(u32, u32)>>,
}

impl Snapshot {
    pub fn record(&self, path: &str) -> Option<&FileRecord> {
        let sid = self.strings.lookup(path)?;
        let slot = *self.by_path.get(&sid)?;
        self.files.get(slot as usize).filter(|r| r.live)
    }

    pub fn path(&self, rec: &FileRecord) -> &str {
        self.strings.get(rec.path)
    }

    pub fn live_files(&self) -> impl Iterator<Item = &FileRecord> {
        self.files.iter().filter(|r| r.live)
    }

    /// Insert or replace one file.
    pub fn apply(&mut self, u: Update) {
        let path = self.strings.intern(&u.path);
        let defs: Box<[Def]> = u
            .extracted
            .defs
            .iter()
            .map(|d| Def {
                name: self.strings.intern(&d.name),
                qualified: self.strings.intern(&d.qualified),
                kind: d.kind,
                depth: d.depth,
                span: d.span,
                body: d.body,
                name_line: d.name_line,
                name_col: d.name_col,
            })
            .collect();
        let imports = u
            .extracted
            .imports
            .iter()
            .map(|i| self.strings.intern(i))
            .collect();
        let idents = u
            .extracted
            .idents
            .iter()
            .map(|(n, l)| (self.strings.intern(n), *l))
            .collect();
        self.put(FileRecord {
            path,
            stamp: u.stamp,
            hash: u.hash,
            lang: u.lang,
            skip: u.skip,
            error_nodes: u.extracted.error_nodes,
            defs,
            imports,
            idents,
            live: true,
        });
    }

    /// Insert a record whose strings are already in this snapshot's table.
    pub fn put(&mut self, rec: FileRecord) {
        let names: Vec<(Arc<str>, u32)> = rec
            .defs
            .iter()
            .enumerate()
            .map(|(i, d)| (self.strings.arc(d.name), i as u32))
            .collect();
        let slot = match self.by_path.get(&rec.path).copied() {
            Some(slot) => {
                self.unlink(slot);
                self.files[slot as usize] = rec;
                slot
            }
            None => {
                let slot = self.files.len() as u32;
                self.by_path.insert(rec.path, slot);
                self.files.push(rec);
                slot
            }
        };
        for (name, i) in names {
            self.by_name.entry(name).or_default().push((slot, i));
        }
    }

    /// Update only the stamp of a file whose content did not change.
    pub fn restamp(&mut self, path: &str, stamp: Stamp) {
        if let Some(sid) = self.strings.lookup(path) {
            if let Some(slot) = self.by_path.get(&sid) {
                self.files[*slot as usize].stamp = stamp;
            }
        }
    }

    /// Mark a file gone.
    pub fn remove(&mut self, path: &str) {
        let Some(sid) = self.strings.lookup(path) else {
            return;
        };
        if let Some(slot) = self.by_path.get(&sid).copied() {
            self.unlink(slot);
            let rec = &mut self.files[slot as usize];
            rec.live = false;
            rec.defs = Box::new([]);
            rec.imports = Box::new([]);
            rec.idents = Box::new([]);
        }
    }

    fn unlink(&mut self, slot: u32) {
        let names: Vec<u32> = self.files[slot as usize]
            .defs
            .iter()
            .map(|d| d.name)
            .collect();
        for name in names {
            let key = self.strings.get(name);
            if let Some(v) = self.by_name.get_mut(key) {
                v.retain(|(f, _)| *f != slot);
                if v.is_empty() {
                    self.by_name.remove(key);
                }
            }
        }
    }

    pub fn symbol(&self, slot: u32, def: u32) -> Option<SymbolDef> {
        let rec = self.files.get(slot as usize).filter(|r| r.live)?;
        let d = rec.defs.get(def as usize)?;
        Some(self.to_symbol(rec, d))
    }

    pub fn to_symbol(&self, rec: &FileRecord, d: &Def) -> SymbolDef {
        SymbolDef {
            name: self.strings.get(d.name).to_string(),
            qualified: self.strings.get(d.qualified).to_string(),
            kind: d.kind,
            path: self.strings.get(rec.path).to_string(),
            span: d.span,
            body: d.body,
            name_line: d.name_line,
            name_col: d.name_col,
            // Not stored (NFR-CTX-MEM-02): `CodeIndex` reads it back from
            // the file for the few definitions a query returns.
            signature: String::new(),
            depth: d.depth,
        }
    }

    pub fn heap_bytes(&self) -> usize {
        self.strings.heap_bytes()
            + self.files.capacity() * std::mem::size_of::<FileRecord>()
            + self.files.iter().map(FileRecord::heap_bytes).sum::<usize>()
            + self.by_path.capacity() * 12
            + self
                .by_name
                .values()
                .map(|v| 48 + v.capacity() * 8)
                .sum::<usize>()
    }
}

/// FNV-1a, 64-bit: a cheap content fingerprint, not a security boundary.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}
