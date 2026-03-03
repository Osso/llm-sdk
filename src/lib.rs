pub mod claude;
pub mod openrouter;
pub mod session;
pub mod tools;

pub use session::{LogEntry, LogUsage};

/// Backend trait for language model completions.
#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    async fn complete(&self, prompt: &str) -> Result<Output, Error>;
}

/// Result of a model completion.
pub struct Output {
    pub text: String,
    pub usage: Option<TokenUsage>,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl TokenUsage {
    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
    }
}

/// Errors that can occur during model completion.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to spawn process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("timed out")]
    Timeout,
    #[error("process exited with code {code}: {stderr}")]
    ExitStatus { code: i32, stderr: String },
    #[error("session expired")]
    SessionExpired,
    #[error("failed to parse output: {0}")]
    Parse(String),
    #[error("API error (status {status}): {body}")]
    Api { status: u16, body: String },
    #[error("exceeded max turns ({0})")]
    MaxTurns(u32),
}
