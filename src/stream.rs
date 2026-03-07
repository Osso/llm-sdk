//! Claude Code stream-json event types.

use serde::Deserialize;

/// A parsed event from Claude Code's `--output-format stream-json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    System {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        subtype: Option<String>,
    },
    Assistant {
        #[serde(default)]
        message: Option<AssistantMessage>,
    },
    Result {
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        usage: Option<StreamUsage>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub usage: Option<StreamUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { name: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StreamUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Receiver for streaming events during a completion.
pub trait EventSink: Send {
    /// Called for each streaming event. `raw` is the original JSON line.
    fn on_event(&mut self, raw: &str, event: &StreamEvent);
}

/// No-op sink that discards events.
pub struct NullSink;

impl EventSink for NullSink {
    fn on_event(&mut self, _raw: &str, _event: &StreamEvent) {}
}
