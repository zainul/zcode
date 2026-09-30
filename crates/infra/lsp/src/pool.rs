//! One language server per language, started on first use, capped, and shut
//! down when idle (FR-LSP-10, CE-DQ20).
//!
//! v0.6 started the first configured server that came up and routed every
//! file to it, so a monorepo's second language had no server at all, and a
//! Go repository on a machine with rust-analyzer installed paid for a
//! process that could answer nothing. The pool starts nothing until a
//! request names a file of its language.
//!
//! Queries without a file (`workspace/symbol`, all-document diagnostics)
//! go to the servers already running and never start one: a question about
//! "everything" is not a reason to boot every toolchain on the machine.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use domain::{
    BoxError, LspDiagnostic, LspLocation, LspPort, LspReadiness, LspSymbolInfo, LspWorkspaceEdit,
};

use crate::{language_id_for, uri_to_path, LspClient};

/// One server as configured. Several languages may share a command
/// (`typescript-language-server` serves JS and TS); they share a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSpec {
    /// Canonical language names this server answers for.
    pub languages: Vec<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Starts a server. Injectable so the pool is testable without processes.
pub type Starter =
    Box<dyn Fn(&ServerSpec, &Path, u64) -> Result<Box<dyn LspPort + Send>, String> + Send>;

struct Slot {
    spec: ServerSpec,
    client: Option<Box<dyn LspPort + Send>>,
    last_used: Instant,
    /// Why the server would not start; not retried in this process.
    failed: Option<String>,
    /// Documents opened on this slot, replayed into a (re)started server.
    docs: HashSet<String>,
}

pub struct LspPool {
    root: PathBuf,
    timeout_ms: u64,
    max_servers: usize,
    idle: Duration,
    slots: Vec<Slot>,
    starter: Starter,
}

/// The language a document belongs to, in the names servers are configured
/// under: `typescriptreact` is TypeScript for this purpose.
fn language_of(uri: &str) -> &'static str {
    match language_id_for(uri) {
        "typescriptreact" => "typescript",
        "javascriptreact" => "javascript",
        other => other,
    }
}

fn process_starter() -> Starter {
    Box::new(|spec: &ServerSpec, root: &Path, timeout_ms: u64| {
        LspClient::start_with_timeout(&spec.command, &spec.args, &spec.env, root, timeout_ms)
            .map(|c| Box::new(c) as Box<dyn LspPort + Send>)
            .map_err(|e| e.to_string())
    })
}

impl LspPool {
    /// A pool over `specs`, in preference order. Starts nothing.
    pub fn new(
        root: &Path,
        specs: Vec<ServerSpec>,
        max_servers: usize,
        idle: Duration,
        timeout_ms: u64,
    ) -> Self {
        Self::with_starter(
            root,
            specs,
            max_servers,
            idle,
            timeout_ms,
            process_starter(),
        )
    }

    pub fn with_starter(
        root: &Path,
        specs: Vec<ServerSpec>,
        max_servers: usize,
        idle: Duration,
        timeout_ms: u64,
        starter: Starter,
    ) -> Self {
        // Merge specs that run the same program, so JS and TS share one
        // `typescript-language-server`.
        let mut merged: Vec<ServerSpec> = Vec::new();
        for spec in specs {
            match merged
                .iter_mut()
                .find(|m| m.command == spec.command && m.args == spec.args && m.env == spec.env)
            {
                Some(m) => {
                    for l in spec.languages {
                        if !m.languages.contains(&l) {
                            m.languages.push(l);
                        }
                    }
                }
                None => merged.push(spec),
            }
        }
        Self {
            root: root.to_path_buf(),
            timeout_ms,
            max_servers: max_servers.max(1),
            idle,
            slots: merged
                .into_iter()
                .map(|spec| Slot {
                    spec,
                    client: None,
                    last_used: Instant::now(),
                    failed: None,
                    docs: HashSet::new(),
                })
                .collect(),
            starter,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Languages with a server running now.
    pub fn running(&self) -> Vec<String> {
        self.slots
            .iter()
            .filter(|s| s.client.is_some())
            .map(|s| s.spec.languages.join("/"))
            .collect()
    }

    fn slot_index(&self, uri: &str) -> Option<usize> {
        let lang = language_of(uri);
        self.slots
            .iter()
            .position(|s| s.spec.languages.iter().any(|l| l == lang))
    }

    /// Drop servers nobody has asked anything for `idle`.
    fn evict_idle(&mut self) {
        let idle = self.idle;
        for slot in &mut self.slots {
            if slot.client.is_some() && slot.last_used.elapsed() > idle {
                log::debug!("lsp: stopping idle {}", slot.spec.command);
                slot.client = None;
            }
        }
    }

    /// The running client for `uri`, starting it if needed.
    fn client_for(&mut self, uri: &str) -> Result<&mut Box<dyn LspPort + Send>, BoxError> {
        self.evict_idle();
        let Some(i) = self.slot_index(uri) else {
            return Err(format!(
                "no language server is configured for {} files — see `zcode config`",
                language_of(uri)
            )
            .into());
        };
        if let Some(why) = &self.slots[i].failed {
            return Err(why.clone().into());
        }
        if self.slots[i].client.is_none() {
            self.start(i)?;
        }
        let slot = &mut self.slots[i];
        slot.last_used = Instant::now();
        slot.client
            .as_mut()
            .ok_or_else(|| BoxError::from("language server is not running"))
    }

    fn start(&mut self, i: usize) -> Result<(), BoxError> {
        // At the cap: the least recently used server makes room.
        let running = self.slots.iter().filter(|s| s.client.is_some()).count();
        if running >= self.max_servers {
            if let Some(lru) = self
                .slots
                .iter_mut()
                .filter(|s| s.client.is_some())
                .min_by_key(|s| s.last_used)
            {
                log::debug!("lsp: stopping {} to make room", lru.spec.command);
                lru.client = None;
            }
        }
        let spec = self.slots[i].spec.clone();
        match (self.starter)(&spec, &self.root, self.timeout_ms) {
            Ok(mut client) => {
                // Whatever this slot had open before an eviction, it gets
                // again — from disk, which is where every write went.
                for uri in &self.slots[i].docs {
                    if let Ok(text) = std::fs::read_to_string(uri_to_path(uri)) {
                        let _ = client.open_document(uri, &text);
                    }
                }
                self.slots[i].client = Some(client);
                self.slots[i].last_used = Instant::now();
                Ok(())
            }
            Err(e) => {
                let why = format!(
                    "language server `{}` failed to start: {e} — check it is installed, or \
                     see `zcode config`",
                    spec.command
                );
                self.slots[i].failed = Some(why.clone());
                Err(why.into())
            }
        }
    }

    fn running_clients(&mut self) -> impl Iterator<Item = &mut Box<dyn LspPort + Send>> {
        self.slots.iter_mut().filter_map(|s| s.client.as_mut())
    }
}

impl LspPort for LspPool {
    fn goto_definition(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<LspLocation, BoxError> {
        self.client_for(uri)?.goto_definition(uri, line, character)
    }

    fn find_references(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Box<[LspLocation]>, BoxError> {
        self.client_for(uri)?.find_references(uri, line, character)
    }

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Result<String, BoxError> {
        self.client_for(uri)?.hover(uri, line, character)
    }

    fn rename_symbol(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<LspWorkspaceEdit, BoxError> {
        self.client_for(uri)?
            .rename_symbol(uri, line, character, new_name)
    }

    /// Remembered on the slot; forwarded only to a server already running —
    /// a write is not a reason to start one. A later start replays it.
    fn open_document(&mut self, uri: &str, text: &str) -> Result<(), BoxError> {
        self.evict_idle();
        let Some(i) = self.slot_index(uri) else {
            return Ok(()); // no server for this language: nothing to tell
        };
        let slot = &mut self.slots[i];
        slot.docs.insert(uri.to_string());
        match slot.client.as_mut() {
            Some(client) => client.open_document(uri, text),
            None => Ok(()),
        }
    }

    fn diagnostics(&mut self, uri: Option<&str>) -> Result<Box<[LspDiagnostic]>, BoxError> {
        match uri {
            Some(u) => {
                if !self.serves(u) {
                    return Ok(Box::new([]));
                }
                self.client_for(u)?.diagnostics(Some(u))
            }
            None => {
                let mut all = Vec::new();
                for client in self.running_clients() {
                    all.extend(client.diagnostics(None)?.into_vec());
                }
                Ok(all.into_boxed_slice())
            }
        }
    }

    fn diagnostics_within(
        &mut self,
        uri: &str,
        cap: Duration,
    ) -> Result<Box<[LspDiagnostic]>, BoxError> {
        if !self.serves(uri) {
            return Ok(Box::new([]));
        }
        self.client_for(uri)?.diagnostics_within(uri, cap)
    }

    fn stored_diagnostics(&self, uri: &str) -> Box<[LspDiagnostic]> {
        self.slot_index(uri)
            .and_then(|i| self.slots[i].client.as_ref())
            .map_or_else(
                || Box::new([]) as Box<[LspDiagnostic]>,
                |c| c.stored_diagnostics(uri),
            )
    }

    fn workspace_symbols(&mut self, query: &str) -> Result<Box<[LspSymbolInfo]>, BoxError> {
        let mut all = Vec::new();
        for client in self.running_clients() {
            if let Ok(found) = client.workspace_symbols(query) {
                all.extend(found.into_vec());
            }
        }
        Ok(all.into_boxed_slice())
    }

    fn readiness(&self) -> LspReadiness {
        let mut indexing = false;
        let mut pct: Option<u8> = None;
        for c in self.slots.iter().filter_map(|s| s.client.as_ref()) {
            if let LspReadiness::Indexing(p) = c.readiness() {
                indexing = true;
                pct = match (pct, p) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
        }
        if indexing {
            LspReadiness::Indexing(pct)
        } else {
            LspReadiness::Ready
        }
    }

    /// A server not started yet is not indexing — it will be once asked,
    /// and the request then waits on it like any other.
    fn readiness_for(&self, uri: &str) -> LspReadiness {
        self.slot_index(uri)
            .and_then(|i| self.slots[i].client.as_ref())
            .map_or(LspReadiness::Ready, |c| c.readiness())
    }

    fn serves(&self, uri: &str) -> bool {
        self.slot_index(uri)
            .is_some_and(|i| self.slots[i].client.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What the fake servers were asked, by command.
    type Journal = Arc<Mutex<Vec<String>>>;

    struct Fake {
        name: String,
        journal: Journal,
        indexing: bool,
    }

    impl LspPort for Fake {
        fn goto_definition(&mut self, uri: &str, _: u32, _: u32) -> Result<LspLocation, BoxError> {
            self.journal
                .lock()
                .unwrap()
                .push(format!("{} definition {uri}", self.name));
            Err("fake".into())
        }
        fn find_references(
            &mut self,
            _: &str,
            _: u32,
            _: u32,
        ) -> Result<Box<[LspLocation]>, BoxError> {
            Ok(Box::new([]))
        }
        fn hover(&mut self, _: &str, _: u32, _: u32) -> Result<String, BoxError> {
            Ok(self.name.clone())
        }
        fn rename_symbol(
            &mut self,
            _: &str,
            _: u32,
            _: u32,
            _: &str,
        ) -> Result<LspWorkspaceEdit, BoxError> {
            Err("fake".into())
        }
        fn open_document(&mut self, uri: &str, _: &str) -> Result<(), BoxError> {
            self.journal
                .lock()
                .unwrap()
                .push(format!("{} open {uri}", self.name));
            Ok(())
        }
        fn workspace_symbols(&mut self, q: &str) -> Result<Box<[LspSymbolInfo]>, BoxError> {
            self.journal
                .lock()
                .unwrap()
                .push(format!("{} symbols {q}", self.name));
            Ok(Box::new([]))
        }
        fn readiness(&self) -> LspReadiness {
            if self.indexing {
                LspReadiness::Indexing(Some(40))
            } else {
                LspReadiness::Ready
            }
        }
    }

    fn spec(langs: &[&str], command: &str) -> ServerSpec {
        ServerSpec {
            languages: langs.iter().map(|l| l.to_string()).collect(),
            command: command.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    fn pool(max: usize, idle: Duration) -> (LspPool, Journal) {
        let journal = Journal::default();
        let j = journal.clone();
        let starter: Starter = Box::new(move |s: &ServerSpec, _: &Path, _: u64| {
            if s.command == "broken" {
                return Err("not found".into());
            }
            j.lock().unwrap().push(format!("start {}", s.command));
            Ok(Box::new(Fake {
                name: s.command.clone(),
                journal: j.clone(),
                indexing: s.command == "slow",
            }) as Box<dyn LspPort + Send>)
        });
        let p = LspPool::with_starter(
            Path::new("/proj"),
            vec![
                spec(&["go"], "gopls"),
                spec(&["typescript"], "tsls"),
                spec(&["javascript"], "tsls"),
                spec(&["rust"], "ra"),
                spec(&["python"], "broken"),
                spec(&["c"], "slow"),
            ],
            max,
            idle,
            1_000,
            starter,
        );
        (p, journal)
    }

    fn log(j: &Journal) -> Vec<String> {
        j.lock().unwrap().clone()
    }

    #[test]
    fn servers_start_lazily_one_per_language() {
        let (mut p, j) = pool(3, Duration::from_secs(600));
        assert!(log(&j).is_empty(), "nothing at construction");
        p.open_document("file:///proj/a.go", "package a").unwrap();
        assert!(log(&j).is_empty(), "a write does not start a server");
        p.hover("file:///proj/a.go", 0, 0).unwrap();
        // (The replayed didOpen reads the file from disk; this one does not
        // exist, so there is nothing to replay — the idle test covers it.)
        assert_eq!(log(&j), ["start gopls"]);
        // JS and TS share one process; TSX is TypeScript.
        p.hover("file:///proj/web/page.tsx", 0, 0).unwrap();
        p.hover("file:///proj/web/util.js", 0, 0).unwrap();
        let starts: Vec<String> = log(&j)
            .into_iter()
            .filter(|l| l.starts_with("start"))
            .collect();
        assert_eq!(starts, ["start gopls", "start tsls"]);
        assert_eq!(p.running(), ["go", "typescript/javascript"]);
    }

    #[test]
    fn the_least_recently_used_server_makes_room_at_the_cap() {
        let (mut p, _) = pool(2, Duration::from_secs(600));
        p.hover("file:///proj/a.go", 0, 0).unwrap();
        p.hover("file:///proj/a.ts", 0, 0).unwrap();
        p.hover("file:///proj/a.go", 0, 0).unwrap(); // go is now the recent one
        p.hover("file:///proj/a.rs", 0, 0).unwrap();
        assert_eq!(p.running(), ["go", "rust"]);
    }

    #[test]
    fn idle_servers_shut_down_and_restart_with_their_documents() {
        let (mut p, j) = pool(3, Duration::ZERO);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.go");
        std::fs::write(&file, "package a").unwrap();
        let uri = crate::path_to_uri(&file);
        p.hover(&uri, 0, 0).unwrap();
        p.open_document(&uri, "package a").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        p.hover(&uri, 0, 0).unwrap();
        let starts = log(&j).iter().filter(|l| l.starts_with("start")).count();
        assert_eq!(starts, 2, "evicted when idle, started again: {:?}", log(&j));
        assert_eq!(
            log(&j).last().map(String::as_str),
            Some(format!("gopls open {uri}").as_str()),
            "the document is replayed into the new server"
        );
    }

    #[test]
    fn a_failed_start_is_remembered_and_reported() {
        let (mut p, _) = pool(3, Duration::from_secs(600));
        let e = p.hover("file:///proj/a.py", 0, 0).unwrap_err().to_string();
        assert!(
            e.starts_with("language server `broken` failed to start: not found"),
            "{e}"
        );
        let again = p.hover("file:///proj/b.py", 0, 0).unwrap_err().to_string();
        assert_eq!(e, again);
        let none = p.hover("file:///proj/a.lua", 0, 0).unwrap_err().to_string();
        assert!(none.contains("no language server is configured"), "{none}");
    }

    #[test]
    fn questions_without_a_file_do_not_start_servers() {
        let (mut p, j) = pool(3, Duration::from_secs(600));
        p.workspace_symbols("Foo").unwrap();
        assert!(p.diagnostics(None).unwrap().is_empty());
        assert!(p.diagnostics(Some("file:///proj/a.go")).unwrap().is_empty());
        assert!(log(&j).is_empty());
        p.hover("file:///proj/a.go", 0, 0).unwrap();
        p.workspace_symbols("Foo").unwrap();
        assert_eq!(
            log(&j).last().map(String::as_str),
            Some("gopls symbols Foo")
        );
    }

    #[test]
    fn readiness_is_per_language() {
        let (mut p, _) = pool(3, Duration::from_secs(600));
        assert_eq!(p.readiness_for("file:///proj/a.c"), LspReadiness::Ready);
        p.hover("file:///proj/a.c", 0, 0).unwrap();
        p.hover("file:///proj/a.go", 0, 0).unwrap();
        assert_eq!(
            p.readiness_for("file:///proj/a.c"),
            LspReadiness::Indexing(Some(40))
        );
        assert_eq!(p.readiness_for("file:///proj/a.go"), LspReadiness::Ready);
        assert_eq!(p.readiness(), LspReadiness::Indexing(Some(40)));
        assert!(p.serves("file:///proj/a.go"));
        assert!(!p.serves("file:///proj/a.rs"));
    }
}
