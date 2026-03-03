use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single tool call recorded in the message log.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A single chat message stored in the log.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallRecord>>,
    pub tool_call_id: Option<String>,
    pub timestamp: String,
}

/// JSON-persisted ordered list of chat messages for a single conversation.
///
/// Stored at `{data_dir}/message_logs/{key}.json`.
pub struct MessageLog {
    key: String,
    messages: Vec<ChatMessage>,
    data_dir: PathBuf,
}

impl MessageLog {
    /// Load from disk or return an empty log if the file is missing.
    pub fn load(data_dir: &Path, key: &str) -> Self {
        let messages = read_messages(data_dir, key);
        MessageLog {
            key: key.to_string(),
            messages,
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// Write the current messages to disk atomically (temp + rename).
    pub fn save(&self) {
        let logs_dir = self.data_dir.join("message_logs");
        if std::fs::create_dir_all(&logs_dir).is_err() {
            return;
        }
        let path = logs_dir.join(format!("{}.json", self.key));
        let tmp = logs_dir.join(format!("{}.json.tmp", self.key));
        let Ok(json) = serde_json::to_string_pretty(&self.messages) else {
            return;
        };
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Append a message and persist to disk.
    pub fn push(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
        self.save();
    }

    /// All messages in order.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Remove all non-system messages and persist.
    pub fn clear(&mut self) {
        self.messages.retain(|m| m.role == "system");
        self.save();
    }
}

fn read_messages(data_dir: &Path, key: &str) -> Vec<ChatMessage> {
    let path = data_dir.join("message_logs").join(format!("{key}.json"));
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("llm-sdk-msglog-{suffix}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn empty_on_missing_file() {
        let dir = temp_dir("empty");
        let log = MessageLog::load(&dir, "test");
        assert!(log.messages().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn push_and_reload() {
        let dir = temp_dir("push");
        let mut log = MessageLog::load(&dir, "conv");
        log.push(make_msg("user", "hello"));
        log.push(make_msg("assistant", "world"));

        let log2 = MessageLog::load(&dir, "conv");
        assert_eq!(log2.messages().len(), 2);
        assert_eq!(log2.messages()[0].role, "user");
        assert_eq!(log2.messages()[1].content.as_deref(), Some("world"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_keeps_system() {
        let dir = temp_dir("clear");
        let mut log = MessageLog::load(&dir, "conv");
        log.push(make_msg("system", "you are helpful"));
        log.push(make_msg("user", "hello"));
        log.push(make_msg("assistant", "hi"));

        log.clear();
        assert_eq!(log.messages().len(), 1);
        assert_eq!(log.messages()[0].role, "system");

        let log2 = MessageLog::load(&dir, "conv");
        assert_eq!(log2.messages().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_atomic() {
        let dir = temp_dir("atomic");
        let mut log = MessageLog::load(&dir, "key");
        log.push(make_msg("user", "test"));

        // No .tmp file should remain
        let tmp = dir.join("message_logs/key.json.tmp");
        assert!(!tmp.exists());

        let main = dir.join("message_logs/key.json");
        assert!(main.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_call_record_roundtrip() {
        let rec = ToolCallRecord {
            id: "call_1".into(),
            name: "Bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let rec2: ToolCallRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec2.id, "call_1");
        assert_eq!(rec2.name, "Bash");
    }
}
