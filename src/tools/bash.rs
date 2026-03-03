use super::{Tool, ToolDef};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

pub struct BashTool {
    working_dir: Option<PathBuf>,
}

impl BashTool {
    pub fn new() -> Self {
        Self { working_dir: None }
    }

    pub fn with_working_dir(dir: PathBuf) -> Self {
        Self {
            working_dir: Some(dir),
        }
    }
}

#[derive(Deserialize)]
struct Args {
    command: String,
    timeout: Option<u64>,
}

#[async_trait::async_trait]
impl ToolDef for BashTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "Bash".into(),
            description: "Execute a bash command and return its output.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The bash command to execute" },
                    "timeout": { "type": "number", "description": "Timeout in seconds (default 120)" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> String {
        let args: Args = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("Invalid arguments: {e}"),
        };
        let timeout_secs = args.timeout.unwrap_or(120);
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&args.command);
        if let Some(ref dir) = self.working_dir {
            cmd.current_dir(dir);
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return format!("Failed to spawn: {e}"),
        };

        match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await
        {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str(&stderr);
                }
                if result.is_empty() {
                    format!("Exit code: {}", output.status.code().unwrap_or(-1))
                } else {
                    result
                }
            }
            Ok(Err(e)) => format!("Process error: {e}"),
            Err(_) => "Command timed out".into(),
        }
    }
}
