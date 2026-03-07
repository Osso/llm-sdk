use crate::stream::{EventSink, NullSink, StreamEvent, StreamUsage};
use crate::{Backend, Error, Output, TokenUsage};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
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
        cmd.arg("--output-format").arg("stream-json");
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

impl Claude {
    /// Run a completion with an event sink that receives each streaming event.
    pub async fn complete_streaming(
        &self,
        prompt: &str,
        sink: &mut dyn EventSink,
    ) -> Result<Output, Error> {
        let cmd = self.build_command(prompt);
        execute_streaming(cmd, self.stdin_prompt, prompt, self.timeout, sink).await
    }
}

#[async_trait::async_trait]
impl Backend for Claude {
    async fn complete(&self, prompt: &str) -> Result<Output, Error> {
        self.complete_streaming(prompt, &mut NullSink).await
    }
}

async fn execute_streaming(
    mut cmd: Command,
    stdin_prompt: bool,
    prompt: &str,
    timeout: Option<Duration>,
    sink: &mut dyn EventSink,
) -> Result<Output, Error> {
    tracing::debug!("spawning claude process (stream-json)");
    let mut child = cmd.spawn().map_err(Error::Spawn)?;
    if stdin_prompt {
        write_stdin(&mut child, prompt).await?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Spawn(std::io::Error::other("no stdout")))?;
    let result = read_stream_events(stdout, timeout, sink).await;
    let _ = child.wait().await;
    result
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

type StdoutLines = tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>;

async fn read_stream_events(
    stdout: tokio::process::ChildStdout,
    timeout: Option<Duration>,
    sink: &mut dyn EventSink,
) -> Result<Output, Error> {
    let reader = tokio::io::BufReader::new(stdout);
    let mut lines = reader.lines();
    let deadline = timeout.map(|d| tokio::time::Instant::now() + d);

    loop {
        let line = next_line(&mut lines, deadline).await?;
        if let Some(output) = process_stream_line(line, sink)? {
            return Ok(output);
        }
    }
}

fn process_stream_line(
    line: Option<String>,
    sink: &mut dyn EventSink,
) -> Result<Option<Output>, Error> {
    let Some(line) = line else {
        return Err(Error::Parse("process closed stdout without result".into()));
    };
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Some(event) = parse_stream_line(trimmed) else {
        return Ok(None);
    };
    sink.on_event(trimmed, &event);
    match into_output(event) {
        Some(r) => r.map(Some),
        None => Ok(None),
    }
}

async fn next_line(
    lines: &mut StdoutLines,
    deadline: Option<tokio::time::Instant>,
) -> Result<Option<String>, Error> {
    match deadline {
        Some(dl) => tokio::time::timeout_at(dl, lines.next_line())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(Error::Spawn),
        None => lines.next_line().await.map_err(Error::Spawn),
    }
}

fn parse_stream_line(trimmed: &str) -> Option<StreamEvent> {
    match serde_json::from_str(trimmed) {
        Ok(e) => Some(e),
        Err(_) => {
            tracing::trace!("skipping unparseable line: {}", &trimmed[..trimmed.len().min(200)]);
            None
        }
    }
}

fn into_output(event: StreamEvent) -> Option<Result<Output, Error>> {
    let StreamEvent::Result { result, session_id, is_error, total_cost_usd, usage } = event else {
        return None;
    };
    if is_error {
        return Some(result_error(result));
    }
    Some(Ok(Output {
        text: result.unwrap_or_default(),
        usage: usage.map(convert_usage),
        session_id,
        cost_usd: total_cost_usd,
    }))
}

fn result_error(result: Option<String>) -> Result<Output, Error> {
    let msg = result.unwrap_or_default();
    if msg.contains("No conversation found") {
        return Err(Error::SessionExpired);
    }
    Err(Error::ExitStatus { code: 1, stderr: msg })
}

fn convert_usage(u: StreamUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_input_tokens: u.cache_read_input_tokens,
        cache_creation_input_tokens: u.cache_creation_input_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::StreamEvent;

    #[test]
    fn into_output_from_result_event() {
        let event = StreamEvent::Result {
            result: Some("hello world".into()),
            session_id: Some("abc-123".into()),
            is_error: false,
            total_cost_usd: Some(0.05),
            usage: Some(StreamUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 10,
                cache_creation_input_tokens: 5,
            }),
        };
        let output = into_output(event).unwrap().unwrap();
        assert_eq!(output.text, "hello world");
        assert_eq!(output.session_id.as_deref(), Some("abc-123"));
        assert_eq!(output.cost_usd, Some(0.05));
        let usage = output.usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 10);
        assert_eq!(usage.cache_creation_input_tokens, 5);
    }

    #[test]
    fn into_output_none_for_non_result() {
        let event = StreamEvent::System { session_id: None, subtype: Some("init".into()) };
        assert!(into_output(event).is_none());
    }

    #[test]
    fn into_output_error_result() {
        let event = StreamEvent::Result {
            result: Some("something failed".into()),
            session_id: None,
            is_error: true,
            total_cost_usd: None,
            usage: None,
        };
        let err = into_output(event).unwrap().unwrap_err();
        assert!(err.to_string().contains("something failed"));
    }

    #[test]
    fn into_output_session_expired() {
        let event = StreamEvent::Result {
            result: Some("No conversation found".into()),
            session_id: None,
            is_error: true,
            total_cost_usd: None,
            usage: None,
        };
        let err = into_output(event).unwrap().unwrap_err();
        assert!(matches!(err, Error::SessionExpired));
    }

    #[test]
    fn parse_stream_line_valid() {
        let json = r#"{"type":"system","session_id":"abc","subtype":"init"}"#;
        let event = parse_stream_line(json).unwrap();
        assert!(matches!(event, StreamEvent::System { .. }));
    }

    #[test]
    fn parse_stream_line_invalid() {
        assert!(parse_stream_line("not json").is_none());
    }

    #[test]
    fn parse_stream_line_unknown_type() {
        let json = r#"{"type":"rate_limit_event","data":"something"}"#;
        let event = parse_stream_line(json).unwrap();
        assert!(matches!(event, StreamEvent::Other));
    }

    #[test]
    fn process_stream_line_eof() {
        let err = process_stream_line(None, &mut NullSink).unwrap_err();
        assert!(err.to_string().contains("without result"));
    }

    #[test]
    fn process_stream_line_empty() {
        let result = process_stream_line(Some("  ".into()), &mut NullSink).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn process_stream_line_result() {
        let json = r#"{"type":"result","subtype":"success","result":"done","is_error":false}"#;
        let output = process_stream_line(Some(json.into()), &mut NullSink)
            .unwrap()
            .unwrap();
        assert_eq!(output.text, "done");
    }

    #[test]
    fn build_command_uses_stream_json() {
        let claude = Claude::with_binary(PathBuf::from("/usr/bin/claude"));
        let cmd = claude.build_command("hello");
        let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert!(args.contains(&"stream-json".to_string()));
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
