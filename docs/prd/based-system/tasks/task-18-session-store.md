# Task 18 — Session Store (UUIDv7, create/continue/fork/import/export, auto-checkpoint)

**Related PRD sections:** §3.2 Session & State Management (FR-SESSION-01..07), §3.8 Engine Loop (FR-LOOP / checkpoint), §8 DQ9 (UUIDv7)
**Depends on:** task-02 (Domain — `Session`/`SessionStorePort` defined in §4.4 of the technical plan)
**Status:** To do
**Priority:** High (the engine loop checkpoints here per turn; session lifecycle is a user-facing story US-E-09)

## Objective

Implement `crates/infra/session` with `UuidSessionStore` writing portable, human-readable JSON sessions to `.ag/sessions/<id>.json`. Supports the full CLI subcommand set (`create`, `continue`, `fork`, `import`, `export`) and writes an **atomic checkpoint after every completed tool round** (FR-SESSION-06) so a killed process resumes from the last good state (NFR-REL-03). Session IDs are UUIDv7 (time-ordered for easy sorting).

## Step-by-step

### 1. New crate `crates/infra/session`

`Cargo.toml`:
```toml
[dependencies]
domain = { path = "../../domain", version = "0.1.0" }
uuid = { workspace = true }      # features=["v7"]
serde_json = { workspace = true }
serde = { workspace = true }
[dev-dependencies]
tempfile = "3.10"
```

### 2. `src/lib.rs` — `UuidSessionStore`

```rust
pub struct UuidSessionStore { base: PathBuf }   // base = <working_dir>/.ag/sessions
impl UuidSessionStore {
    pub fn new(base: PathBuf) -> Self;          // creates dir if missing
    fn id_path(&self, id: &str) -> Result<PathBuf, SessionError>; // validates id is UUIDv7, sanitizes
    fn atomic_write(&self, path: &Path, json: &str) -> Result<(), SessionError>; // temp+rename (FR-SESS checkpoint safe)
}
impl SessionStorePort for UuidSessionStore {
    fn create(&mut self) -> Result<String, Box<dyn Error>> {
        let id = uuid::Uuid::now_v7().to_string();
        let session = Session { id: id.clone(), created_at: now_iso(), model: default, mode: Build, last_message_at: now_iso(), step_count: 0, messages: Box::new([]) };
        self.checkpoint(&id, &session)?;   // FR-SESSION-01
        Ok(id)
    }
    fn load(&self, id: &str) -> Result<Session, Box<dyn Error>>;           // FR-SESSION-02
    fn checkpoint(&mut self, id: &str, session: &Session) -> Result<(), Box<dyn Error>>;  // FR-SESSION-06 (atomic)
    fn fork(&mut self, id: &str, new_id: &str) -> Result<(), Box<dyn Error>>;  // FR-SESSION-03
    fn import_from(&mut self, path: &Path) -> Result<String, Box<dyn Error>>;  // FR-SESSION-04 → new UUIDv7
    fn export_to(&self, id: &str, path: &Path) -> Result<(), Box<dyn Error>>;  // FR-SESSION-05
}
```

### 3. Safety details

- **ID validation/sanitization:** `id_path` rejects any id not parseable as `Uuid::now_v7` or containing path separators (`/`, `\`, `..`) — prevents path traversal from a user-supplied session id.
- **Atomic checkpoint:** write to `.ag/sessions/<id>.json.tmp` then `fs::rename` (rename is atomic on the same filesystem). On crash mid-write, the `.tmp` is ignored on next load (FR-SESSION-06, NFR-REL-03).
- **Portability:** `.ag` is created relative to `config.working_dir`. Sessions are plain JSON (transcript + telemetry + metadata) so a teammate can copy `*.json` and `import` it (US-E-09, FR-SESSION-04/05).
- **Metadata:** every session carries `created_at`, `model`, `mode`, `last_message_at`, `step_count` (FR-SESSION-07).

### 4. `Session` serialization

`Session` (a domain type, §4.4) is `Serialize`/`Deserialize` — but **domain is dep-free**, so it cannot depend on `serde`. **Resolution:** keep `Session` as a plain domain struct (no serde derives there); `UuidSessionStore` maps to a local `SessionFile` serde struct with identical fields + a `version: 1` tag. This keeps domain pure and lets the on-disk schema evolve independently (FR-SESSION imports an external JSON session regardless of its origin).

### 5. Tests

- `create_writes_valid_uuidv7`: `create()` → id parses as `Uuid::now_v7` and a file exists.
- `checkpoint_then_load_roundtrip`: create; mutate `step_count` + `messages`; `checkpoint`; `load` → equal.
- `checkpoint_is_atomic_on_crash`: simulate by leaving a `.tmp` file; `load` ignores it and reads the last complete checkpoint.
- `fork_copies_history`: create → push messages → checkpoint → `fork` → child has identical `messages` length but distinct id.
- `import_export_roundtrip`: `export_to` a temp file → `import_from` that file → new UUIDv7; `load` the imported id equals the exported transcript.
- `path_traversal_blocked`: `load("../../../etc/passwd")` → `SessionError::InvalidId`.
- `id_with_separator_blocked`: `load("ab/cd")` → error.
- `create_creates_dot_ag_dir`: `base` dir did not exist → `create` makes `.ag/sessions/`.

## Test-case scenario

- `ag session create` prints a UUIDv7; `.ag/sessions/<id>.json` exists with `version`, `created_at`, `model`, `mode`, `step_count`.
- Mid-`ag run` kill (SIGKILL) → `.ag/sessions/<id>.json` reflects the last completed checkpoint (FR-SESSION-06).
- `ag session export <id> --to /tmp/x.json && ag session import /tmp/x.json` → new id, full transcript restored (US-E-09).

## How to verify

```
cargo test -p infra-session
cargo clippy -p infra-session -- -D warnings
cargo tree -p infra-session        # deps: domain, uuid, serde_json, serde (+ tempfile dev)
```

**Pass criteria:** UUIDv7 ids generated and validated; atomic checkpoint (temp+rename); path traversal on id rejected; import/export round-trips; zero `unsafe`; `cargo tree -p infra-session` shows `{domain, uuid, serde_json, serde}`.

## Success metric mapping

- M1.2 (tests), M1.8 (session lifecycle create/continue/fork/export/import), NFR-REL-03 (crash recovery via checkpoint), FR-SESSION-01..07, FR-LOOP-04 (max_tool_output_chars is engine-side, but checkpoint writes full transcript), DQ9 (UUIDv7), L3 (deps ≤ 6: domain, uuid, serde_json, serde, tempfile-dev).

## Notes / risks

- The `Session` domain struct intentionally lacks serde derives (FR-DI-01); the `SessionFile` wrapper in this crate is the serialization adapter. If serde is ever allowed into domain, merge them back.
- `last_message_at`/`created_at` use ISO-8601 UTC strings (portable, human-readable — PRD wants sessions "portable, human-readable JSON").
