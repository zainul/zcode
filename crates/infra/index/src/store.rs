//! The on-disk index (CE-DQ17): a small custom binary format, written
//! atomically, read through a bounds-checked cursor.
//!
//! ```text
//! header:  b"ZCIX" | u16 FORMAT | u32 EXTRACTOR | u32 string_count
//! strings: string_count × (u32 len | UTF-8 bytes)
//! files:   u32 file_count, then per file:
//!          u32 path | u64 mtime_ns | u64 size | u64 fnv1a | u8 lang | u8 skip | u32 error_nodes
//!          u32 n_defs    × (u32 name | u32 qualified | u8 kind | u8 depth
//!                           | span | u8 has_body [| span] | u32 name_line | u32 name_col)
//!          u32 n_imports × u32 spec
//!          u32 n_idents  × (u32 name | u32 line)
//! span:    u32 start_line | u32 end_line | u32 start_byte | u32 end_byte
//! ```
//!
//! Import *resolution* is not stored: it depends on which other files exist,
//! so it is computed against the live path set when asked (`resolve.rs`).
//!
//! Anything unexpected — truncation, a bad id, another version — makes the
//! whole file worthless and the index rebuilds; it is a cache, never a
//! source of truth, so no partial recovery is attempted.

use std::io::Write;
use std::path::Path;

use domain::Span;

use crate::lang::Lang;
use crate::snapshot::{kind_code, Def, FileRecord, Interner, Skip, Snapshot, Stamp, KINDS};

const MAGIC: &[u8; 4] = b"ZCIX";
/// The byte layout above.
pub const FORMAT: u16 = 1;
/// Bumped whenever the queries or the extractor change what is recorded, so
/// a store built by an older zcode is rebuilt rather than trusted.
pub const EXTRACTOR: u32 = 1;

#[derive(Debug)]
pub struct Corrupt(pub &'static str);

impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Corrupt {}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn span(&mut self, s: Span) {
        self.u32(s.start_line);
        self.u32(s.end_line);
        self.u32(s.start_byte);
        self.u32(s.end_byte);
    }
}

/// Serialise the live records. Strings are re-interned into a fresh table,
/// which drops every string no live record refers to any more — the one
/// place the process-lifetime interner gets compacted.
pub fn encode(snap: &Snapshot) -> Vec<u8> {
    let mut table = Interner::default();
    let mut remap = |id: u32| table.intern(snap.strings.get(id));
    let mut files = Vec::new();
    for rec in snap.live_files() {
        let defs: Vec<Def> = rec
            .defs
            .iter()
            .map(|d| Def {
                name: remap(d.name),
                qualified: remap(d.qualified),
                ..d.clone()
            })
            .collect();
        files.push(FileRecord {
            path: remap(rec.path),
            defs: defs.into(),
            imports: rec.imports.iter().map(|i| remap(*i)).collect(),
            idents: rec.idents.iter().map(|(n, l)| (remap(*n), *l)).collect(),
            ..rec.clone()
        });
    }
    let mut w = Writer { buf: Vec::new() };
    w.buf.extend_from_slice(MAGIC);
    w.u16(FORMAT);
    w.u32(EXTRACTOR);
    w.u32(table.len() as u32);
    for i in 0..table.len() as u32 {
        let s = table.get(i);
        w.u32(s.len() as u32);
        w.buf.extend_from_slice(s.as_bytes());
    }
    w.u32(files.len() as u32);
    for f in &files {
        w.u32(f.path);
        w.u64(f.stamp.mtime_ns);
        w.u64(f.stamp.size);
        w.u64(f.hash);
        w.u8(f.lang.code());
        w.u8(f.skip.code());
        w.u32(f.error_nodes);
        w.u32(f.defs.len() as u32);
        for d in f.defs.iter() {
            w.u32(d.name);
            w.u32(d.qualified);
            w.u8(kind_code(d.kind));
            w.u8(d.depth);
            w.span(d.span);
            match d.body {
                Some(b) => {
                    w.u8(1);
                    w.span(b);
                }
                None => w.u8(0),
            }
            w.u32(d.name_line);
            w.u32(d.name_col);
        }
        w.u32(f.imports.len() as u32);
        for i in f.imports.iter() {
            w.u32(*i);
        }
        w.u32(f.idents.len() as u32);
        for (n, l) in f.idents.iter() {
            w.u32(*n);
            w.u32(*l);
        }
    }
    w.buf
}

struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Corrupt> {
        let end = self.at.checked_add(n).ok_or(Corrupt("length overflow"))?;
        let slice = self.buf.get(self.at..end).ok_or(Corrupt("truncated"))?;
        self.at = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, Corrupt> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Corrupt> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, Corrupt> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, Corrupt> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    fn span(&mut self) -> Result<Span, Corrupt> {
        Ok(Span {
            start_line: self.u32()?,
            end_line: self.u32()?,
            start_byte: self.u32()?,
            end_byte: self.u32()?,
        })
    }
    /// A count, sanity-checked against what is left so a corrupt length
    /// cannot ask for a multi-gigabyte allocation.
    fn count(&mut self, min_item: usize) -> Result<usize, Corrupt> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_item) > self.buf.len() - self.at {
            return Err(Corrupt("count exceeds file"));
        }
        Ok(n)
    }
}

/// Decode a store into a fresh snapshot.
pub fn decode(buf: &[u8]) -> Result<Snapshot, Corrupt> {
    let mut c = Cursor { buf, at: 0 };
    if c.take(4)? != MAGIC {
        return Err(Corrupt("not an index file"));
    }
    if c.u16()? != FORMAT {
        return Err(Corrupt("format version differs"));
    }
    if c.u32()? != EXTRACTOR {
        return Err(Corrupt("extractor version differs"));
    }
    let mut snap = Snapshot::default();
    let n_strings = c.count(4)?;
    for i in 0..n_strings {
        let len = c.u32()? as usize;
        let s = std::str::from_utf8(c.take(len)?).map_err(|_| Corrupt("string is not UTF-8"))?;
        if snap.strings.intern(s) as usize != i {
            return Err(Corrupt("duplicate string"));
        }
    }
    let sid = |c: &mut Cursor<'_>| -> Result<u32, Corrupt> {
        let id = c.u32()?;
        if (id as usize) < n_strings {
            Ok(id)
        } else {
            Err(Corrupt("string id out of range"))
        }
    };
    let n_files = c.count(34)?;
    for _ in 0..n_files {
        let path = sid(&mut c)?;
        let stamp = Stamp {
            mtime_ns: c.u64()?,
            size: c.u64()?,
        };
        let hash = c.u64()?;
        let lang_code = c.u8()?;
        let skip = Skip::from_code(c.u8()?).ok_or(Corrupt("bad skip code"))?;
        let error_nodes = c.u32()?;
        let n_defs = c.count(35)?;
        let mut defs = Vec::with_capacity(n_defs);
        for _ in 0..n_defs {
            let name = sid(&mut c)?;
            let qualified = sid(&mut c)?;
            let kind = *KINDS
                .get(usize::from(c.u8()?))
                .ok_or(Corrupt("bad kind code"))?;
            let depth = c.u8()?;
            let span = c.span()?;
            let body = match c.u8()? {
                0 => None,
                1 => Some(c.span()?),
                _ => return Err(Corrupt("bad body flag")),
            };
            defs.push(Def {
                name,
                qualified,
                kind,
                depth,
                span,
                body,
                name_line: c.u32()?,
                name_col: c.u32()?,
            });
        }
        let n_imports = c.count(4)?;
        let mut imports = Vec::with_capacity(n_imports);
        for _ in 0..n_imports {
            imports.push(sid(&mut c)?);
        }
        let n_idents = c.count(8)?;
        let mut idents = Vec::with_capacity(n_idents);
        for _ in 0..n_idents {
            idents.push((sid(&mut c)?, c.u32()?));
        }
        // A language compiled out since the store was written: drop the
        // record; the walk will not offer that file again either.
        let Some(lang) = Lang::from_code(lang_code) else {
            return Err(Corrupt("bad language code"));
        };
        if Lang::for_path(snap.strings.get(path)) != Some(lang) {
            continue;
        }
        snap.put(FileRecord {
            path,
            stamp,
            hash,
            lang,
            skip,
            error_nodes,
            defs: defs.into(),
            imports: imports.into(),
            idents: idents.into(),
            live: true,
        });
    }
    if c.at != buf.len() {
        return Err(Corrupt("trailing bytes"));
    }
    Ok(snap)
}

/// Write atomically: a temporary sibling, synced, then renamed over.
pub fn save(path: &Path, snap: &Snapshot) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("zcix.tmp");
    let bytes = encode(snap);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_data()?;
    }
    std::fs::rename(&tmp, path)
}

/// Load a store. A missing file is `None` quietly; an unusable one is
/// `None` with a warning, and the caller rebuilds.
pub fn load(path: &Path) -> Option<Snapshot> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!("index discarded: {e}; rebuilding");
            return None;
        }
    };
    match decode(&bytes) {
        Ok(snap) => Some(snap),
        Err(e) => {
            log::warn!("index discarded: {e}; rebuilding");
            None
        }
    }
}
