use crate::claude::Claude;
use crate::message_log::MessageLog;
use crate::{Backend, Error, Output};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Directory-backed session ID store.
///
/// Manages `sessions.json` — a flat map of key → session_id.
/// Thread-safe via `Arc<Mutex<>>`.
#[derive(Clone)]
pub struct SessionStore {
    inner: Arc<Mutex<StoreInner>>,
}

struct StoreInner {
    data_dir: PathBuf,
    sessions: HashMap<String, String>,
}

impl SessionStore {
    /// Create a store at `~/.local/share/{app}/{project}/`.
    pub fn new(app: &str, project: &str) -> Self {
        let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let data_dir = base.join(app).join(project);
        Self::load(data_dir)
    }

    /// Create a store at an explicit directory path.
    pub fn load(data_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let sessions = read_sessions(&data_dir);
        Self {
            inner: Arc::new(Mutex::new(StoreInner { data_dir, sessions })),
        }
    }

    /// Expose the data directory (e.g. for message log construction).
    pub fn data_dir(&self) -> PathBuf {
        self.inner.lock().unwrap().data_dir.clone()
    }

    /// Load (or create empty) a `MessageLog` for the given key.
    pub fn message_log(&self, key: &str) -> MessageLog {
        let data_dir = self.data_dir();
        MessageLog::load(&data_dir, key)
    }

    /// Delete the message log file for the given key, if it exists.
    pub fn remove_message_log(&self, key: &str) {
        let data_dir = self.data_dir();
        let path = data_dir.join("message_logs").join(format!("{key}.json"));
        let _ = std::fs::remove_file(&path);
    }

    /// Remove a session key, forcing a fresh session on next `session()` call.
    pub fn remove(&self, key: &str) {
        let mut inner = self.inner.lock().unwrap();
        if inner.sessions.remove(key).is_some() {
            write_sessions(&inner.data_dir, &inner.sessions);
        }
    }

    /// Get or create a session for the given key.
    pub fn session(&self, key: &str) -> Session {
        let inner = self.inner.lock().unwrap();
        let (session_id, created) = match inner.sessions.get(key) {
            Some(id) => (id.clone(), true),
            None => (new_uuid(), false),
        };
        Session {
            key: key.to_string(),
            session_id,
            created,
            system_prompt: None,
            store: self.inner.clone(),
        }
    }
}

/// One conversation's lifecycle — handles resume, expiry, and persistence.
pub struct Session {
    key: String,
    session_id: String,
    created: bool,
    system_prompt: Option<String>,
    store: Arc<Mutex<StoreInner>>,
}

impl Session {
    /// Set the system prompt for fresh sessions.
    pub fn system_prompt(mut self, sp: impl Into<String>) -> Self {
        self.system_prompt = Some(sp.into());
        self
    }

    /// Current session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Return the data directory for this session's store.
    pub fn data_dir(&self) -> PathBuf {
        self.store.lock().unwrap().data_dir.clone()
    }

    /// Reset to a new session (new UUID, not yet created).
    pub fn reset(&mut self) {
        let data_dir = self.data_dir();
        append_log(&data_dir, &self.key, &LogEntry::SessionReset);
        self.session_id = new_uuid();
        self.created = false;
        self.persist();
    }

    /// Run a completion, handling resume and session expiry transparently.
    pub async fn complete(&mut self, base: &Claude, prompt: &str) -> Result<Output, Error> {
        let result = self.try_complete(base, prompt).await;
        match result {
            Err(Error::SessionExpired) if self.created => {
                tracing::info!(key = %self.key, "session expired, starting fresh");
                self.session_id = new_uuid();
                self.created = false;
                self.persist();
                self.try_complete(base, prompt).await
            }
            other => other,
        }
    }

    async fn try_complete(&mut self, base: &Claude, prompt: &str) -> Result<Output, Error> {
        let claude = if self.created {
            base.clone().resume(&self.session_id)
        } else {
            let mut c = base.clone().session_id(&self.session_id);
            if let Some(ref sp) = self.system_prompt {
                c = c.system_prompt(sp);
            }
            c
        };
        let data_dir = self.data_dir();
        append_log(
            &data_dir,
            &self.key,
            &LogEntry::User {
                text: prompt.to_string(),
                timestamp: now_utc(),
            },
        );

        let output = claude.complete(prompt).await?;
        self.created = true;
        self.persist();

        append_log(
            &data_dir,
            &self.key,
            &LogEntry::Assistant {
                text: output.text.clone(),
                timestamp: now_utc(),
                usage: output.usage.as_ref().map(|u| LogUsage {
                    input: u.input_tokens,
                    output: u.output_tokens,
                    cache_read: u.cache_read_input_tokens,
                    cache_creation: u.cache_creation_input_tokens,
                }),
            },
        );

        Ok(output)
    }

    fn persist(&self) {
        let mut inner = self.store.lock().unwrap();
        inner
            .sessions
            .insert(self.key.clone(), self.session_id.clone());
        write_sessions(&inner.data_dir, &inner.sessions);
    }
}

/// A single entry in the per-key JSONL conversation log.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum LogEntry {
    #[serde(rename = "user")]
    User { text: String, timestamp: String },
    #[serde(rename = "assistant")]
    Assistant {
        text: String,
        timestamp: String,
        usage: Option<LogUsage>,
    },
    #[serde(rename = "session_reset")]
    SessionReset,
}

/// Token usage recorded in the log.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LogUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

/// Return the current UTC time as an ISO-8601 string (no external deps).
pub fn now_utc() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;
    let (y, m, day) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    // Howard Hinnant's algorithm
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Append a `LogEntry` as a JSONL line to `{data_dir}/logs/{key}.jsonl`.
pub fn append_log(data_dir: &Path, key: &str, entry: &LogEntry) {
    let logs_dir = data_dir.join("logs");
    if std::fs::create_dir_all(&logs_dir).is_err() {
        return;
    }
    let path = logs_dir.join(format!("{key}.jsonl"));
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    if let Ok(json) = serde_json::to_string(entry) {
        use std::io::Write;
        let _ = writeln!(file, "{json}");
    }
}

fn sessions_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("sessions.json")
}

fn read_sessions(data_dir: &std::path::Path) -> HashMap<String, String> {
    let path = sessions_path(data_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return HashMap::new();
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return HashMap::new();
    };
    let Some(obj) = val.as_object() else {
        return HashMap::new();
    };
    obj.iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
        .collect()
}

fn write_sessions(data_dir: &std::path::Path, sessions: &HashMap<String, String>) {
    let map: serde_json::Map<String, serde_json::Value> = sessions
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let json = serde_json::to_string_pretty(&map).unwrap_or_default();
    let path = sessions_path(data_dir);
    let _ = std::fs::write(&path, json);
}

/// Generate a hash-based UUIDv4 (no uuid crate dependency).
pub fn new_uuid() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut h1 = DefaultHasher::new();
    SystemTime::now().hash(&mut h1);
    std::process::id().hash(&mut h1);
    let a = h1.finish();

    let mut h2 = DefaultHasher::new();
    a.hash(&mut h2);
    let b = h2.finish();

    // UUIDv4: version nibble = 4, variant bits = 10xx
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        a as u16 & 0x0FFF,
        (b >> 48) as u16 & 0x3FFF | 0x8000,
        b & 0xFFFF_FFFF_FFFF,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn uuid_format() {
        let id = new_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4"); // version nibble
        let variant = u8::from_str_radix(&id[19..20], 16).unwrap();
        assert!((8..=11).contains(&variant)); // variant 10xx
    }

    #[test]
    fn uuid_uniqueness() {
        let a = new_uuid();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let b = new_uuid();
        assert_ne!(a, b);
    }

    #[test]
    fn store_roundtrip() {
        let dir = std::env::temp_dir().join("llm-sdk-test-store");
        let _ = std::fs::remove_dir_all(&dir);

        let store = SessionStore::load(dir.clone());
        let mut session = store.session("test-key");
        assert!(!session.created);

        // Simulate persist
        session.created = true;
        session.persist();

        // Reload from disk
        let store2 = SessionStore::load(dir.clone());
        let session2 = store2.session("test-key");
        assert!(session2.created);
        assert_eq!(session2.session_id(), session.session_id());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_empty_file() {
        let dir = std::env::temp_dir().join("llm-sdk-test-empty");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("sessions.json"), "").unwrap();
        let sessions = read_sessions(&dir.to_path_buf());
        assert!(sessions.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_new_resolves_path() {
        let store = SessionStore::new("llm-sdk-test-app", "test-project");
        let inner = store.inner.lock().unwrap();
        let path = inner.data_dir.to_string_lossy();
        assert!(path.contains("llm-sdk-test-app"));
        assert!(path.contains("test-project"));
        let _ = std::fs::remove_dir_all(&inner.data_dir);
    }

    #[test]
    fn session_reset() {
        let dir = std::env::temp_dir().join("llm-sdk-test-reset");
        let _ = std::fs::remove_dir_all(&dir);

        let store = SessionStore::load(dir.clone());
        let mut session = store.session("key");
        let original_id = session.session_id().to_string();

        std::thread::sleep(std::time::Duration::from_millis(1));
        session.reset();
        assert_ne!(session.session_id(), original_id);
        assert!(!session.created);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiple_keys() {
        let dir = std::env::temp_dir().join("llm-sdk-test-multi");
        let _ = std::fs::remove_dir_all(&dir);

        let store = SessionStore::load(dir.clone());
        let mut s1 = store.session("dev-0");
        let mut s2 = store.session("architect");
        s1.created = true;
        s1.persist();
        s2.created = true;
        s2.persist();

        // Verify both persisted
        let path = dir.join("sessions.json");
        assert!(Path::new(&path).exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("dev-0"));
        assert!(content.contains("architect"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn now_utc_format() {
        let ts = now_utc();
        // Basic ISO-8601 shape: 2026-03-03T12:00:00Z
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[19..20], "Z");
    }

    #[test]
    fn append_log_creates_file() {
        let dir = std::env::temp_dir().join("llm-sdk-test-log");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        append_log(
            &dir,
            "mykey",
            &LogEntry::User {
                text: "hello".into(),
                timestamp: now_utc(),
            },
        );

        let log_path = dir.join("logs/mykey.jsonl");
        assert!(log_path.exists());
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("\"type\":\"user\""));
        assert!(content.contains("hello"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_entry_session_reset_serializes() {
        let entry = LogEntry::SessionReset;
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(json, r#"{"type":"session_reset"}"#);
    }
}
