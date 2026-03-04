use crate::{Backend, Error, Output, TokenUsage};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Claude CLI backend with builder-pattern configuration.
#[derive(Clone)]
pub struct Claude {
    binary: PathBuf,
    model: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    skip_permissions: bool,
    allowed_tools: Option<Vec<String>>,
    disallowed_tools: Option<Vec<String>>,
    session_id: Option<String>,
    resume_session: Option<String>,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    timeout: Option<Duration>,
    no_session_persistence: bool,
    verbose: bool,
    stdin_prompt: bool,
    extra_args: Vec<String>,
    mcp_config: Option<String>,
    env_removes: Vec<String>,
    command_prefix: Vec<String>,
}

impl Claude {
    /// Create a new Claude backend, resolving the binary via PATH.
    pub fn new() -> Result<Self, Error> {
        let binary = which::which("claude")
            .map_err(|e| Error::Spawn(std::io::Error::new(std::io::ErrorKind::NotFound, e)))?;
        Ok(Self::with_binary(binary))
    }

    /// Create a Claude backend with an explicit binary path.
    pub fn with_binary(binary: PathBuf) -> Self {
        Self {
            binary,
            model: None,
            effort: None,
            permission_mode: None,
            skip_permissions: false,
            allowed_tools: None,
            disallowed_tools: None,
            session_id: None,
            resume_session: None,
            system_prompt: None,
            working_dir: None,
            timeout: None,
            no_session_persistence: false,
            verbose: false,
            stdin_prompt: false,
            extra_args: Vec::new(),
            mcp_config: None,
            env_removes: Vec::new(),
            command_prefix: Vec::new(),
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn effort(mut self, effort: impl Into<String>) -> Self {
        self.effort = Some(effort.into());
        self
    }

    pub fn permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = Some(mode.into());
        self
    }

    pub fn skip_permissions(mut self) -> Self {
        self.skip_permissions = true;
        self
    }

    pub fn allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = Some(tools);
        self
    }

    pub fn disallowed_tools(mut self, tools: Vec<String>) -> Self {
        self.disallowed_tools = Some(tools);
        self
    }

    /// Set session ID for a new session. Mutually exclusive with `resume`.
    pub fn session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self.resume_session = None;
        self
    }

    /// Resume an existing session. Mutually exclusive with `session_id`.
    pub fn resume(mut self, id: impl Into<String>) -> Self {
        self.resume_session = Some(id.into());
        self.session_id = None;
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn no_session_persistence(mut self) -> Self {
        self.no_session_persistence = true;
        self
    }

    pub fn verbose(mut self) -> Self {
        self.verbose = true;
        self
    }

    /// Pass the prompt via stdin instead of as a CLI argument.
    pub fn stdin_prompt(mut self) -> Self {
        self.stdin_prompt = true;
        self
    }

    pub fn extra_arg(mut self, arg: impl Into<String>) -> Self {
        self.extra_args.push(arg.into());
        self
    }

    pub fn mcp_config(mut self, config: impl Into<String>) -> Self {
        self.mcp_config = Some(config.into());
        self
    }

    pub fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.env_removes.push(key.into());
        self
    }

    /// Prepend a command prefix (e.g. bwrap sandbox args) before the claude binary.
    pub fn command_prefix(mut self, prefix: Vec<String>) -> Self {
        self.command_prefix = prefix;
        self
    }

    pub(crate) fn build_command(&self, prompt: &str) -> Command {
        let mut cmd = if self.command_prefix.is_empty() {
            Command::new(&self.binary)
        } else {
            let mut c = Command::new(&self.command_prefix[0]);
            for arg in &self.command_prefix[1..] {
                c.arg(arg);
            }
            c.arg(&self.binary);
            c
        };
        cmd.arg("-p");
        if !self.stdin_prompt {
            cmd.arg(prompt);
        }
        cmd.arg("--output-format").arg("json");
        self.apply_flags(&mut cmd);
        self.apply_io(&mut cmd);
        cmd
    }

    fn apply_flags(&self, cmd: &mut Command) {
        if let Some(ref m) = self.model {
            cmd.arg("--model").arg(m);
        }
        if let Some(ref e) = self.effort {
            cmd.arg("--effort").arg(e);
        }
        if let Some(ref pm) = self.permission_mode {
            cmd.arg("--permission-mode").arg(pm);
        }
        if self.skip_permissions {
            cmd.arg("--dangerously-skip-permissions");
        }
        if let Some(ref t) = self.allowed_tools {
            cmd.arg("--allowedTools").arg(t.join(","));
        }
        if let Some(ref t) = self.disallowed_tools {
            cmd.arg("--disallowedTools").arg(t.join(","));
        }
        if let Some(ref id) = self.session_id {
            cmd.arg("--session-id").arg(id);
        }
        if let Some(ref id) = self.resume_session {
            cmd.arg("--resume").arg(id);
        }
        if let Some(ref sp) = self.system_prompt {
            cmd.arg("--system-prompt").arg(sp);
        }
        if self.no_session_persistence {
            cmd.arg("--no-session-persistence");
        }
        if self.verbose {
            cmd.arg("--verbose");
        }
        if let Some(ref cfg) = self.mcp_config {
            cmd.arg("--mcp-config").arg(cfg);
        }
        for arg in &self.extra_args {
            cmd.arg(arg);
        }
    }

    fn apply_io(&self, cmd: &mut Command) {
        // Skip current_dir when command_prefix is set — the prefix (e.g. bwrap --chdir)
        // handles working directory inside the namespace. Host-side chdir would fail
        // because the mount point doesn't exist on the host.
        if let Some(ref dir) = self.working_dir {
            if self.command_prefix.is_empty() {
                cmd.current_dir(dir);
            }
        }
        for key in &self.env_removes {
            cmd.env_remove(key);
        }
        if self.stdin_prompt {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
}

#[async_trait::async_trait]
impl Backend for Claude {
    async fn complete(&self, prompt: &str) -> Result<Output, Error> {
        let cmd = self.build_command(prompt);
        execute(cmd, self.stdin_prompt, prompt, self.timeout).await
    }
}

async fn execute(
    mut cmd: Command,
    stdin_prompt: bool,
    prompt: &str,
    timeout: Option<Duration>,
) -> Result<Output, Error> {
    tracing::debug!("spawning claude process");
    let mut child = cmd.spawn().map_err(Error::Spawn)?;
    if stdin_prompt {
        write_stdin(&mut child, prompt).await?;
    }
    let output = wait_with_timeout(child, timeout).await?;
    check_exit_status(&output)?;
    Ok(parse_output(&output.stdout))
}

async fn write_stdin(child: &mut tokio::process::Child, prompt: &str) -> Result<(), Error> {
    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(Error::Spawn)?;
    }
    Ok(())
}

async fn wait_with_timeout(
    child: tokio::process::Child,
    timeout: Option<Duration>,
) -> Result<std::process::Output, Error> {
    match timeout {
        Some(d) => tokio::time::timeout(d, child.wait_with_output())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(Error::Spawn),
        None => child.wait_with_output().await.map_err(Error::Spawn),
    }
}

fn check_exit_status(output: &std::process::Output) -> Result<(), Error> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if stderr.contains("No conversation found") {
        return Err(Error::SessionExpired);
    }
    Err(Error::ExitStatus {
        code: output.status.code().unwrap_or(-1),
        stderr,
    })
}

pub(crate) fn parse_output(stdout: &[u8]) -> Output {
    let raw = String::from_utf8_lossy(stdout);
    if let Some(output) = try_parse_array(&raw) {
        return output;
    }
    if let Some(output) = try_parse_object(&raw) {
        return output;
    }
    Output {
        text: raw.into_owned(),
        usage: None,
        session_id: None,
        cost_usd: None,
    }
}

fn try_parse_array(raw: &str) -> Option<Output> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(raw).ok()?;
    let entry = arr
        .iter()
        .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("result"))?;
    Some(extract_output(entry))
}

fn try_parse_object(raw: &str) -> Option<Output> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    val.get("result")?;
    Some(extract_output(&val))
}

fn extract_output(val: &serde_json::Value) -> Output {
    let text = val
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    let usage = val.get("usage").map(extract_usage);
    let session_id = val
        .get("session_id")
        .and_then(|s| s.as_str())
        .map(String::from);
    let cost_usd = val.get("cost_usd").and_then(|c| c.as_f64());
    Output {
        text,
        usage,
        session_id,
        cost_usd,
    }
}

fn extract_usage(u: &serde_json::Value) -> TokenUsage {
    TokenUsage {
        input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        cache_read_input_tokens: u
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_creation_input_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_from_array_format() {
        let json = r#"[
            {"type": "system", "data": "init"},
            {"type": "result", "result": "hello world", "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 10,
                "cache_creation_input_tokens": 5
            }, "session_id": "abc-123"}
        ]"#;
        let output = parse_output(json.as_bytes());
        assert_eq!(output.text, "hello world");
        let usage = output.usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 10);
        assert_eq!(usage.cache_creation_input_tokens, 5);
        assert_eq!(output.session_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn output_from_single_object() {
        let json = r#"{"result": "response text", "usage": {
            "input_tokens": 200, "output_tokens": 100,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
        }}"#;
        let output = parse_output(json.as_bytes());
        assert_eq!(output.text, "response text");
        let usage = output.usage.unwrap();
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 100);
    }

    #[test]
    fn output_from_raw_text() {
        let output = parse_output(b"not json at all");
        assert_eq!(output.text, "not json at all");
        assert!(output.usage.is_none());
        assert!(output.session_id.is_none());
    }

    #[test]
    fn output_array_without_result_entry() {
        let json = r#"[{"type": "system"}, {"type": "assistant"}]"#;
        let output = parse_output(json.as_bytes());
        assert!(output.text.contains("system"));
    }

    #[test]
    fn output_object_without_result_field() {
        let json = r#"{"error": "something went wrong"}"#;
        let output = parse_output(json.as_bytes());
        assert!(output.text.contains("something went wrong"));
    }

    #[test]
    fn output_missing_usage() {
        let json = r#"{"result": "no usage"}"#;
        let output = parse_output(json.as_bytes());
        assert_eq!(output.text, "no usage");
        assert!(output.usage.is_none());
    }

    #[test]
    fn output_with_cost() {
        let json = r#"{"result": "ok", "cost_usd": 0.05}"#;
        let output = parse_output(json.as_bytes());
        assert_eq!(output.cost_usd, Some(0.05));
    }

    #[test]
    fn build_command_without_prefix() {
        let claude = Claude::with_binary(PathBuf::from("/usr/bin/claude"));
        let cmd = claude.build_command("hello");
        let prog = cmd.as_std().get_program().to_string_lossy().to_string();
        assert_eq!(prog, "/usr/bin/claude");
        let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert!(args.contains(&"hello".to_string()));
    }

    #[test]
    fn build_command_with_prefix() {
        let claude = Claude::with_binary(PathBuf::from("/usr/bin/claude"))
            .command_prefix(vec!["bwrap".into(), "--ro-bind".into(), "/".into(), "/".into(), "--".into()]);
        let cmd = claude.build_command("hello");
        let prog = cmd.as_std().get_program().to_string_lossy().to_string();
        assert_eq!(prog, "bwrap");
        let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args[0], "--ro-bind");
        assert_eq!(args[3], "--");
        assert_eq!(args[4], "/usr/bin/claude");
    }
}
