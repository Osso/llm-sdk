use super::{Tool, ToolDef};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

pub struct BashTool {
    working_dir: Option<PathBuf>,
    command_prefix: Vec<String>,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            working_dir: None,
            command_prefix: Vec::new(),
        }
    }

    pub fn with_working_dir(dir: PathBuf) -> Self {
        Self {
            working_dir: Some(dir),
            command_prefix: Vec::new(),
        }
    }

    /// Prepend a command prefix (e.g. bwrap sandbox args) before bash.
    pub fn with_command_prefix(mut self, prefix: Vec<String>) -> Self {
        self.command_prefix = prefix;
        self
    }

    fn build_command(&self, shell_command: &str) -> Command {
        let mut cmd = if self.command_prefix.is_empty() {
            Command::new("bash")
        } else {
            let mut c = Command::new(&self.command_prefix[0]);
            for arg in &self.command_prefix[1..] {
                c.arg(arg);
            }
            c.arg("bash");
            c
        };
        cmd.arg("-c").arg(shell_command);
        if let Some(ref dir) = self.working_dir {
            cmd.current_dir(dir);
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd
    }
}

#[derive(Deserialize)]
struct Args {
    command: String,
    timeout: Option<u64>,
}

fn format_output(output: std::process::Output) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_without_prefix() {
        let tool = BashTool::new();
        let cmd = tool.build_command("echo hi");
        let prog = cmd.as_std().get_program().to_string_lossy().to_string();
        let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(prog, "bash");
        assert_eq!(args, vec!["-c", "echo hi"]);
    }

    #[test]
    fn build_command_with_prefix() {
        let tool = BashTool::new()
            .with_command_prefix(vec!["bwrap".into(), "--ro-bind".into(), "/".into(), "/".into(), "--".into()]);
        let cmd = tool.build_command("echo hi");
        let prog = cmd.as_std().get_program().to_string_lossy().to_string();
        let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(prog, "bwrap");
        assert_eq!(args, vec!["--ro-bind", "/", "/", "--", "bash", "-c", "echo hi"]);
    }

    #[test]
    fn build_command_with_working_dir() {
        let tool = BashTool::with_working_dir(PathBuf::from("/tmp"));
        let cmd = tool.build_command("ls");
        assert_eq!(cmd.as_std().get_current_dir(), Some(std::path::Path::new("/tmp")));
    }

    #[tokio::test]
    async fn execute_with_env_prefix() {
        // "env --" is a transparent wrapper that proves the prefix is used
        let tool = BashTool::new()
            .with_command_prefix(vec!["env".into(), "--".into()]);
        let result = tool.execute(r#"{"command": "echo sandboxed"}"#).await;
        assert_eq!(result.trim(), "sandboxed");
    }
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
        let child = match self.build_command(&args.command).spawn() {
            Ok(c) => c,
            Err(e) => return format!("Failed to spawn: {e}"),
        };
        let timeout = Duration::from_secs(args.timeout.unwrap_or(120));
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => format_output(output),
            Ok(Err(e)) => format!("Process error: {e}"),
            Err(_) => "Command timed out".into(),
        }
    }
}
