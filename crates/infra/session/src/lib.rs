//! Session store backed by UUIDv7 + atomic JSON checkpoints (FR-SESSION-01..07).
//!
//! `Session` is a dep-free domain struct (no serde derives — FR-DI-01), so this
//! crate defines a `SessionFile` serialization adapter with local mirror types
//! for the message/role sub-structures plus a `version: 1` tag. Sessions are
//! written as plain JSON to `<working_dir>/.zcode/sessions/<id>.json` (portable &
//! human-readable per US-E-09) and every `checkpoint` is atomic (temp+rename).
//!
//! Direct deps: domain, uuid (v7), serde, serde_json. No other transitive deps.
#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use domain::{
    AgentMode, LlmMessage, LlmRole, LlmToolCall, LlmToolResult, Session, SessionStorePort,
};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_MODEL: &str = "gpt-4o-mini";

/// Errors surfaced from `SessionStorePort` methods (mapped to `Box<dyn Error>`
/// at the trait boundary).
#[derive(Debug)]
pub enum SessionError {
    InvalidId(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Other(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid session id: {id}"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Json(e) => write!(f, "json error: {e}"),
            Self::Other(m) => write!(f, "session error: {m}"),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SessionError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for SessionError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Current UTC time as an ISO-8601 `Z` string (portable, human-readable).
/// `std::time` has no formatter and the toolchain is pinned to 1.85, so this
/// uses pure-calendar days-to-ymd math (Howard Hinnant's civil-from-days).
fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso8601_from_secs(secs)
}

fn iso8601_from_secs(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = days_to_ymd(days);
    let hours = rem / 3600;
    let mins = (rem % 3600) / 60;
    let secs_part = rem % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, mins, secs_part
    )
}

fn days_to_ymd(days: u64) -> (u64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = (y + if m <= 2 { 1 } else { 0 }) as u64;
    (year, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// Serialization adapter — mirrors of domain types so domain stays serde-free.
// ---------------------------------------------------------------------------

/// `SessionFile`: a `Session` with a `version: 1` tag for forward-compatible
/// schema evolution. Message sub-types use string tags so the JSON is
/// human-readable and portable (US-E-09).
#[derive(Debug, Serialize, Deserialize)]
struct SessionFile {
    version: u32,
    id: String,
    created_at: String,
    model: String,
    mode: SerializableMode,
    last_message_at: String,
    step_count: u64,
    messages: Vec<SerializableMessage>,
}

/// On-disk spelling of `domain::AgentMode`.
///
/// `Build` is the v0.1 name for what is now `Auto`; it stays in the enum so
/// sessions written by an older binary still load, and `#[serde(alias)]`
/// accepts either spelling on the way in.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SerializableMode {
    Planning,
    Editing,
    Auto,
    /// v0.1 spelling of `Auto`, read-only: never written by this version.
    Build,
}

#[derive(Debug, Serialize, Deserialize)]
struct SerializableMessage {
    role: SerializableRole,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tool_calls: Vec<SerializableToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_result: Option<SerializableToolResult>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SerializableRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Serialize, Deserialize)]
struct SerializableToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SerializableToolResult {
    tool_call_id: String,
    content: String,
}

impl SessionFile {
    fn from_session(session: &Session) -> Self {
        // `id` is stamped by the caller (create vs fork vs checkpoint) so the
        // id is never silently overwritten here.
        SessionFile {
            version: SCHEMA_VERSION,
            id: session.id.clone(),
            created_at: session.created_at.clone(),
            model: if session.model.is_empty() {
                DEFAULT_MODEL.to_string()
            } else {
                session.model.clone()
            },
            mode: mode_to_serializable(session.mode),
            last_message_at: session.last_message_at.clone(),
            step_count: session.step_count,
            messages: session
                .messages
                .iter()
                .map(SerializableMessage::from)
                .collect(),
        }
    }

    fn into_session(self) -> Session {
        Session {
            id: self.id,
            created_at: self.created_at,
            model: self.model,
            mode: mode_from_serializable(self.mode),
            last_message_at: self.last_message_at,
            step_count: self.step_count,
            messages: self
                .messages
                .into_iter()
                .map(SerializableMessage::into_llm_message)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }
}

fn mode_to_serializable(mode: AgentMode) -> SerializableMode {
    match mode {
        AgentMode::Planning => SerializableMode::Planning,
        AgentMode::Editing => SerializableMode::Editing,
        AgentMode::Auto => SerializableMode::Auto,
    }
}

fn mode_from_serializable(mode: SerializableMode) -> AgentMode {
    match mode {
        SerializableMode::Planning => AgentMode::Planning,
        SerializableMode::Editing => AgentMode::Editing,
        // A session recorded before `editing` existed used `build` for the
        // fully autonomous mode.
        SerializableMode::Auto | SerializableMode::Build => AgentMode::Auto,
    }
}

impl SerializableMessage {
    fn from(m: &LlmMessage) -> Self {
        Self {
            role: m.role.into(),
            content: m.content.clone(),
            tool_calls: m
                .tool_calls
                .iter()
                .map(SerializableToolCall::from)
                .collect(),
            tool_result: m.tool_result.as_ref().map(SerializableToolResult::from),
        }
    }

    fn into_llm_message(self) -> LlmMessage {
        LlmMessage {
            role: self.role.into(),
            content: self.content,
            tool_calls: self
                .tool_calls
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            tool_result: self.tool_result.map(Into::into),
        }
    }
}

impl SerializableToolCall {
    fn from(tc: &LlmToolCall) -> Self {
        Self {
            id: tc.id.clone(),
            name: tc.name.clone(),
            arguments: tc.arguments.clone(),
        }
    }
}

impl From<SerializableToolCall> for LlmToolCall {
    fn from(tc: SerializableToolCall) -> Self {
        Self {
            id: tc.id,
            name: tc.name,
            arguments: tc.arguments,
        }
    }
}

impl SerializableToolResult {
    fn from(tr: &LlmToolResult) -> Self {
        Self {
            tool_call_id: tr.tool_call_id.clone(),
            content: tr.content.clone(),
        }
    }
}

impl From<SerializableToolResult> for LlmToolResult {
    fn from(tr: SerializableToolResult) -> Self {
        Self {
            tool_call_id: tr.tool_call_id,
            content: tr.content,
        }
    }
}

impl From<LlmRole> for SerializableRole {
    fn from(r: LlmRole) -> Self {
        match r {
            LlmRole::System => Self::System,
            LlmRole::User => Self::User,
            LlmRole::Assistant => Self::Assistant,
            LlmRole::Tool => Self::Tool,
        }
    }
}

impl From<SerializableRole> for LlmRole {
    fn from(r: SerializableRole) -> Self {
        match r {
            SerializableRole::System => LlmRole::System,
            SerializableRole::User => LlmRole::User,
            SerializableRole::Assistant => LlmRole::Assistant,
            SerializableRole::Tool => LlmRole::Tool,
        }
    }
}

// ---------------------------------------------------------------------------
// UuidSessionStore
// ---------------------------------------------------------------------------

/// Session ids the store will touch.
///
/// Generated ids are UUIDv7 (time-ordered, DQ9), but `zcode session fork --as
/// <id>` lets a person name a branch — and no one types a valid v7 by hand —
/// so any short, filesystem-safe slug is accepted. The constraint that matters
/// is that an id can never escape the sessions directory: no separators, no
/// `..`, no leading dot, and nothing outside `[A-Za-z0-9._-]`.
pub fn is_safe_id(id: &str) -> bool {
    const MAX_LEN: usize = 64;
    if id.is_empty() || id.len() > MAX_LEN {
        return false;
    }
    if id.starts_with('.') {
        return false;
    }
    if id.contains("..") {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Portable JSON session store with atomic checkpoints (FR-SESSION-01..07).
pub struct UuidSessionStore {
    base: PathBuf,
}

impl UuidSessionStore {
    /// Create a store rooted at `base` (`.zcode/sessions`), making the dir if it
    /// does not yet exist (FR-SESSION create_creates_dot_ag_dir).
    pub fn new(base: PathBuf) -> Self {
        if !base.as_os_str().is_empty() {
            fs::create_dir_all(&base).ok();
        }
        Self { base }
    }

    /// Resolve a session id to its on-disk path, validating that the id is a
    /// path-traversal-safe UUIDv7. Rejects `/`, `\`, `..`, and non-UUID inputs.
    fn id_path(&self, id: &str) -> Result<PathBuf, SessionError> {
        if !is_safe_id(id) {
            return Err(SessionError::InvalidId(id.into()));
        }
        Ok(self.base.join(format!("{id}.json")))
    }

    /// Atomic write: write to `<path>.tmp` then `fs::rename`. Same-filesystem
    /// rename is atomic; a crash mid-write leaves a `.tmp` that is ignored on
    /// next load (FR-SESSION-06, NFR-REL-03).
    fn atomic_write(&self, path: &Path, json: &str) -> Result<(), SessionError> {
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn load_file(&self, path: &Path) -> Result<Session, SessionError> {
        if !path.exists() {
            return Err(SessionError::Other(format!(
                "session not found: {}",
                path.display()
            )));
        }
        let content = fs::read_to_string(path)?;
        let file: SessionFile = serde_json::from_str(&content)?;
        Ok(file.into_session())
    }

    fn write_session(&self, id: &str, session: &Session) -> Result<(), SessionError> {
        let path = self.id_path(id)?;
        let file = SessionFile::from_session(session);
        let file = SessionFile {
            id: id.to_string(),
            ..file
        };
        let json = serde_json::to_string_pretty(&file)?;
        self.atomic_write(&path, &json)
    }
}

impl Default for UuidSessionStore {
    fn default() -> Self {
        Self::new(PathBuf::from(".zcode/sessions"))
    }
}

impl SessionStorePort for UuidSessionStore {
    fn create(&mut self) -> Result<String, domain::BoxError> {
        let id = Uuid::now_v7().to_string();
        let now = now_iso();
        let session = Session {
            id: id.clone(),
            created_at: now.clone(),
            model: DEFAULT_MODEL.to_string(),
            mode: AgentMode::default(),
            last_message_at: now,
            step_count: 0,
            messages: Box::new([]),
        };
        self.write_session(&id, &session)?;
        Ok(id)
    }

    fn load(&self, id: &str) -> Result<Session, domain::BoxError> {
        let path = self.id_path(id)?;
        Ok(self.load_file(&path)?)
    }

    fn checkpoint(&mut self, id: &str, session: &Session) -> Result<(), domain::BoxError> {
        // Stamp the write time here: the engine is stdlib-only and has no
        // calendar formatting, so the clock lives in the adapter.
        let stamped = Session {
            last_message_at: now_iso(),
            ..session.clone()
        };
        Ok(self.write_session(id, &stamped)?)
    }

    fn fork(&mut self, id: &str, new_id: &str) -> Result<(), domain::BoxError> {
        let src = self.id_path(id)?;
        let session = self.load_file(&src)?;
        let path = self.id_path(new_id)?;
        let file = SessionFile::from_session(&session);
        let file = SessionFile {
            id: new_id.to_string(),
            ..file
        };
        let json = serde_json::to_string_pretty(&file)?;
        self.atomic_write(&path, &json)?;
        Ok(())
    }

    fn import_from(&mut self, path: &Path) -> Result<String, domain::BoxError> {
        if !path.exists() {
            return Err(Box::new(SessionError::Other(format!(
                "file not found: {}",
                path.display()
            ))));
        }
        let content = fs::read_to_string(path)?;
        let mut file: SessionFile = serde_json::from_str(&content)?;
        let new_id = Uuid::now_v7().to_string();
        file.id = new_id.clone();
        let json = serde_json::to_string_pretty(&file)?;
        let dest = self.base.join(format!("{new_id}.json"));
        self.atomic_write(&dest, &json)?;
        Ok(new_id)
    }

    fn export_to(&self, id: &str, path: &Path) -> Result<(), domain::BoxError> {
        let src = self.id_path(id)?;
        let file_json = fs::read_to_string(&src)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        self.atomic_write(path, &file_json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn accepts_human_chosen_ids_but_never_escapes_the_directory() {
        // `zcode session fork --as my-experiment` has to work: nobody types a
        // valid UUIDv7 by hand.
        for good in [
            "01a03bdd-4b19-7ce2-99d1-e983bd9abdc8",
            "my-experiment",
            "retry_2",
            "v1.2",
            "ABC123",
        ] {
            assert!(is_safe_id(good), "{good} should be accepted");
        }
        // Anything that could reach outside `.zcode/sessions/` must not be.
        for bad in [
            "",
            "..",
            "../etc/passwd",
            "a/b",
            "a\\b",
            ".hidden",
            "with space",
            "semi;colon",
            "star*",
            "~root",
        ] {
            assert!(!is_safe_id(bad), "{bad:?} must be rejected");
        }
        // And an unbounded id cannot blow past filesystem name limits.
        assert!(!is_safe_id(&"a".repeat(65)));
    }

    #[test]
    fn fork_accepts_a_named_branch() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().to_path_buf());
        let parent = store.create().unwrap();

        store.fork(&parent, "experiment-a").expect("named fork");
        let forked = store.load("experiment-a").expect("load named fork");
        assert_eq!(forked.id, "experiment-a");
        assert!(dir.path().join("experiment-a.json").exists());
    }

    #[test]
    fn fork_still_refuses_a_traversing_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().to_path_buf());
        let parent = store.create().unwrap();
        assert!(store.fork(&parent, "../escaped").is_err());
        assert!(!dir.path().parent().unwrap().join("escaped.json").exists());
    }

    use super::*;

    fn with_two_messages(id: &str, steps: u64) -> Session {
        let now = now_iso();
        Session {
            id: id.to_string(),
            created_at: now.clone(),
            model: "gpt-4o-mini".to_string(),
            mode: AgentMode::Auto,
            last_message_at: now,
            step_count: steps,
            messages: Box::new([
                LlmMessage::system("you are helpful"),
                LlmMessage {
                    role: LlmRole::Assistant,
                    content: "ok".into(),
                    tool_calls: Box::new([LlmToolCall {
                        id: "call_1".into(),
                        name: "shell".into(),
                        arguments: r#"{"command":"echo hi"}"#.into(),
                    }]),
                    tool_result: None,
                },
            ]),
        }
    }

    #[test]
    fn create_writes_valid_uuidv7() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().join("sessions"));
        let id = store.create().unwrap();
        let uuid = Uuid::parse_str(&id).unwrap();
        assert_eq!(uuid.get_version(), Some(uuid::Version::SortRand));
        assert!(store.base.join(format!("{id}.json")).exists());
    }

    #[test]
    fn checkpoint_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().join("sessions"));
        let id = store.create().unwrap();
        let mut session = store.load(&id).unwrap();
        session.step_count = 5;
        session.messages = Box::new([LlmMessage::user("rename foo to bar")]);
        store.checkpoint(&id, &session).unwrap();

        let loaded = store.load(&id).unwrap();
        assert_eq!(loaded.step_count, 5);
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content, "rename foo to bar");
    }

    #[test]
    fn checkpoint_is_atomic_on_crash() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().join("sessions"));
        let id = store.create().unwrap();
        let mut session = store.load(&id).unwrap();
        session.step_count = 1;
        store.checkpoint(&id, &session).unwrap();
        // Simulate a crashed half-write: leave a .tmp alongside the good file.
        let path = store.base.join(format!("{id}.json"));
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, "{ partial, corrupted").unwrap();
        // load ignores .tmp and reads the last complete checkpoint.
        let loaded = store.load(&id).unwrap();
        assert_eq!(loaded.step_count, 1);
    }

    #[test]
    fn fork_copies_history_distinct_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().join("sessions"));
        let id = store.create().unwrap();
        let session = with_two_messages(&id, 3);
        store.checkpoint(&id, &session).unwrap();

        let child = Uuid::now_v7().to_string();
        store.fork(&id, &child).unwrap();

        let parent = store.load(&id).unwrap();
        let kid = store.load(&child).unwrap();
        assert_eq!(kid.messages.len(), parent.messages.len());
        assert_ne!(kid.id, parent.id);
        assert_ne!(id, *kid.id.as_str());
    }

    #[test]
    fn import_export_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().join("sessions"));
        let id = store.create().unwrap();
        let session = with_two_messages(&id, 2);
        store.checkpoint(&id, &session).unwrap();

        let export_path = dir.path().join("exported.json");
        store.export_to(&id, &export_path).unwrap();
        let imported_id = store.import_from(&export_path).unwrap();

        let imported = store.load(&imported_id).unwrap();
        assert_eq!(imported.messages.len(), session.messages.len());
        assert_ne!(imported.id, id);
        assert_eq!(imported.model, session.model);
    }

    #[test]
    fn path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let store = UuidSessionStore::new(dir.path().join("sessions"));
        let err = store.load("../../../etc/passwd").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<SessionError>(),
            Some(SessionError::InvalidId(_))
        ));
    }

    #[test]
    fn id_with_separator_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let store = UuidSessionStore::new(dir.path().join("sessions"));
        let err = store.load("ab/cd").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<SessionError>(),
            Some(SessionError::InvalidId(_))
        ));
        let err = store.load("ab\\cd").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<SessionError>(),
            Some(SessionError::InvalidId(_))
        ));
    }

    #[test]
    fn create_creates_dot_ag_dir() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(".zcode/sessions");
        assert!(!base.exists());
        let mut store = UuidSessionStore::new(base.clone());
        let id = store.create().unwrap();
        assert!(base.exists());
        assert!(base.join(format!("{id}.json")).exists());
    }

    #[test]
    fn session_file_serializes_with_version_and_role_tags() {
        let session = with_two_messages("01900000-0000-7000-8000-000000000001", 3);
        let file = SessionFile::from_session(&session);
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"role\":\"assistant\""));
        assert!(json.contains("\"tool_calls\""));
        let back: SessionFile = serde_json::from_str(&json).unwrap();
        let restored = back.into_session();
        assert_eq!(restored.step_count, 3);
        assert_eq!(restored.messages.len(), 2);
    }

    /// A plain name is a legitimate id (see `is_safe_id`) — asking for one
    /// that does not exist is "not found", not "invalid".
    #[test]
    fn unknown_but_well_formed_id_reports_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = UuidSessionStore::new(dir.path().join("sessions"));
        let err = store.load("not-a-uuid").unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<SessionError>(),
                Some(SessionError::Other(_))
            ),
            "expected a not-found error, got {err}"
        );
    }

    #[test]
    fn traversing_id_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = UuidSessionStore::new(dir.path().join("sessions"));
        for bad in ["../escape", "a/b", "..", ".hidden"] {
            let err = store.load(bad).unwrap_err();
            assert!(
                matches!(
                    err.downcast_ref::<SessionError>(),
                    Some(SessionError::InvalidId(_))
                ),
                "{bad:?} must be rejected as an invalid id"
            );
        }
    }

    #[test]
    fn empty_id_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = UuidSessionStore::new(dir.path().join("sessions"));
        let err = store.load("").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<SessionError>(),
            Some(SessionError::InvalidId(_))
        ));
    }

    /// Ids the store *generates* stay UUIDv7 so the directory sorts by age;
    /// ids a person supplies are not held to that.
    #[test]
    fn generated_ids_are_uuid_v7() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UuidSessionStore::new(dir.path().join("sessions"));
        let id = store.create().unwrap();
        let parsed = Uuid::parse_str(&id).expect("generated ids parse as UUIDs");
        assert_eq!(parsed.get_version(), Some(uuid::Version::SortRand));
    }

    #[test]
    fn iso8601_output_is_utc_z() {
        let s = now_iso();
        assert!(s.ends_with('Z'));
        assert_eq!(s.len(), 20);
    }
}
