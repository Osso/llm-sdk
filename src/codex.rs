use crate::openai_shared::{
    agent_output_to_sdk, build_agent_loop, tools_json_from_tool_set, NoOpExecutor, ToolSetExecutor,
};
use crate::tools::ToolSet;
use crate::{Backend, Error, Output, TokenUsage};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

const API_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_INSTRUCTIONS: &str = "You are a helpful coding assistant.";

// --- Responses API types ---

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    instructions: String,
    input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponsesToolDef>>,
    store: bool,
    stream: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
enum InputItem {
    #[serde(rename = "message")]
    Message { role: String, content: String },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput { call_id: String, output: String },
}

#[derive(Serialize, Clone)]
struct ResponsesToolDef {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ResponsesApiResponse {
    #[serde(default)]
    output: Vec<OutputItem>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type")]
enum OutputItem {
    #[serde(rename = "message")]
    Message {
        content: Vec<ContentPart>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type")]
enum ContentPart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

#[derive(Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

// --- Auth types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthFile {
    openai: Option<AuthTokens>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthTokens {
    refresh: String,
    access: String,
    expires: u64,
    #[serde(rename = "accountId")]
    account_id: String,
    #[serde(rename = "type")]
    auth_type: Option<String>,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

pub struct Codex {
    client: reqwest::Client,
    model: String,
    system_prompt: Option<String>,
    tool_set: Option<ToolSet>,
    timeout: Option<Duration>,
    max_turns: u32,
    auth_path: PathBuf,
    tokens: tokio::sync::Mutex<Option<AuthTokens>>,
}

impl Codex {
    pub fn new(model: impl Into<String>) -> Self {
        let auth_path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("opencode/auth.json");
        Self {
            client: reqwest::Client::new(),
            model: model.into(),
            system_prompt: None,
            tool_set: None,
            timeout: None,
            max_turns: 20,
            auth_path,
            tokens: tokio::sync::Mutex::new(None),
        }
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn tools(mut self, tool_set: ToolSet) -> Self {
        self.tool_set = Some(tool_set);
        self
    }

    pub fn timeout(mut self, dur: Duration) -> Self {
        self.timeout = Some(dur);
        self
    }

    pub fn max_turns(mut self, n: u32) -> Self {
        self.max_turns = n;
        self
    }

    pub fn auth_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.auth_path = path.into();
        self
    }
}

// --- Auth ---

impl Codex {
    fn read_auth_file(&self) -> Result<AuthTokens, Error> {
        let data = std::fs::read_to_string(&self.auth_path)
            .map_err(|e| Error::Parse(format!("cannot read auth.json: {e}")))?;
        let auth_file: AuthFile = serde_json::from_str(&data)
            .map_err(|e| Error::Parse(format!("invalid auth.json: {e}")))?;
        auth_file
            .openai
            .ok_or_else(|| Error::Parse("missing 'openai' key in auth.json".into()))
    }

    fn write_auth_file(&self, tokens: &AuthTokens) -> Result<(), Error> {
        let data = std::fs::read_to_string(&self.auth_path).unwrap_or_default();
        let mut auth_file: serde_json::Value =
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}));
        auth_file["openai"] = serde_json::to_value(tokens)
            .map_err(|e| Error::Parse(format!("serialize tokens: {e}")))?;
        let json = serde_json::to_string_pretty(&auth_file)
            .map_err(|e| Error::Parse(format!("serialize auth file: {e}")))?;
        write_file_0600(&self.auth_path, &json)
    }

    async fn ensure_tokens(&self) -> Result<AuthTokens, Error> {
        let mut guard = self.tokens.lock().await;
        if let Some(ref t) = *guard {
            if !is_expired(t) {
                return Ok(t.clone());
            }
        }
        let mut tokens = self.read_auth_file()?;
        if is_expired(&tokens) {
            tokens = self.refresh_tokens(&tokens).await?;
            self.write_auth_file(&tokens)?;
        }
        *guard = Some(tokens.clone());
        Ok(tokens)
    }

    async fn refresh_tokens(&self, tokens: &AuthTokens) -> Result<AuthTokens, Error> {
        let body = format!(
            "grant_type=refresh_token&refresh_token={}&client_id={CLIENT_ID}",
            tokens.refresh
        );
        let resp = self
            .client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Api { status: 0, body: e.to_string() })?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api { status, body });
        }
        let r: RefreshResponse = resp
            .json()
            .await
            .map_err(|e| Error::Parse(format!("refresh response: {e}")))?;
        Ok(AuthTokens {
            refresh: r.refresh_token,
            access: r.access_token,
            expires: now_millis() + r.expires_in * 1000,
            account_id: tokens.account_id.clone(),
            auth_type: tokens.auth_type.clone(),
        })
    }
}

fn is_expired(tokens: &AuthTokens) -> bool {
    now_millis() >= tokens.expires
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn write_file_0600(path: &std::path::Path, content: &str) -> Result<(), Error> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Parse(format!("create dir: {e}")))?;
    }
    let mut file = std::fs::File::create(path)
        .map_err(|e| Error::Parse(format!("create auth file: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::Parse(format!("set permissions: {e}")))?;
    }
    file.write_all(content.as_bytes())
        .map_err(|e| Error::Parse(format!("write auth file: {e}")))?;
    Ok(())
}

// --- SSE parsing ---

async fn parse_sse_response(resp: reqwest::Response) -> Result<ResponsesApiResponse, Error> {
    let body = resp.text().await.map_err(|e| Error::Parse(e.to_string()))?;
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with("data: ") {
            continue;
        }
        let data = &line[6..];
        if let Some(parsed) = try_parse_completed(data) {
            return Ok(parsed);
        }
    }
    Err(Error::Parse("no response.completed event in SSE stream".into()))
}

fn try_parse_completed(data: &str) -> Option<ResponsesApiResponse> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    if v.get("type")?.as_str()? != "response.completed" {
        return None;
    }
    let response = v.get("response")?;
    serde_json::from_value(response.clone()).ok()
}

// --- Conversions: ChatMessage <-> Responses API ---

fn extract_instructions(messages: &[llm_agent::ChatMessage]) -> String {
    messages
        .iter()
        .find(|m| m.role == "system")
        .and_then(|m| m.content.clone())
        .unwrap_or_else(|| DEFAULT_INSTRUCTIONS.to_string())
}

fn chat_message_to_input(msg: &llm_agent::ChatMessage) -> Vec<InputItem> {
    match msg.role.as_str() {
        "system" => vec![],
        "user" => {
            let content = msg.content.clone().unwrap_or_default();
            vec![InputItem::Message { role: "user".into(), content }]
        }
        "assistant" => assistant_msg_to_input(msg),
        "tool" => {
            let call_id = msg.tool_call_id.clone().unwrap_or_default();
            let output = msg.content.clone().unwrap_or_default();
            vec![InputItem::FunctionCallOutput { call_id, output }]
        }
        _ => vec![],
    }
}

fn assistant_msg_to_input(msg: &llm_agent::ChatMessage) -> Vec<InputItem> {
    let mut items = Vec::new();
    if let Some(ref text) = msg.content {
        if !text.is_empty() {
            items.push(InputItem::Message {
                role: "assistant".into(),
                content: text.clone(),
            });
        }
    }
    if let Some(ref tcs) = msg.tool_calls {
        for tc in tcs {
            items.push(InputItem::FunctionCall {
                call_id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            });
        }
    }
    items
}

fn tools_from_chat_json(tools: Option<&serde_json::Value>) -> Option<Vec<ResponsesToolDef>> {
    let arr = tools?.as_array()?;
    let defs: Vec<ResponsesToolDef> = arr
        .iter()
        .filter_map(chat_tool_json_to_responses)
        .collect();
    if defs.is_empty() { None } else { Some(defs) }
}

/// Convert a Chat Completions tool JSON to Responses API format.
fn chat_tool_json_to_responses(v: &serde_json::Value) -> Option<ResponsesToolDef> {
    let func = v.get("function")?;
    Some(ResponsesToolDef {
        tool_type: "function".into(),
        name: func.get("name")?.as_str()?.to_string(),
        description: func.get("description")?.as_str()?.to_string(),
        parameters: func.get("parameters").cloned().unwrap_or(serde_json::json!({})),
    })
}

fn responses_usage_to_token_usage(u: &ResponsesUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        ..Default::default()
    }
}

fn output_items_to_parts(items: &[OutputItem]) -> Vec<llm_agent::Part> {
    items.iter().flat_map(output_item_to_parts).collect()
}

fn output_item_to_parts(item: &OutputItem) -> Vec<llm_agent::Part> {
    match item {
        OutputItem::Message { content } => {
            content
                .iter()
                .map(|c| match c {
                    ContentPart::OutputText { text } => llm_agent::Part::Text(text.clone()),
                })
                .collect()
        }
        OutputItem::FunctionCall { call_id, name, arguments } => {
            vec![llm_agent::Part::ToolUse(llm_agent::ToolCall {
                id: call_id.clone(),
                call_type: "function".into(),
                function: llm_agent::FunctionCall {
                    name: name.clone(),
                    arguments: arguments.clone(),
                },
            })]
        }
    }
}

fn has_tool_calls(items: &[OutputItem]) -> bool {
    items.iter().any(|i| matches!(i, OutputItem::FunctionCall { .. }))
}

// --- HTTP transport ---

impl Codex {
    fn build_request(
        &self,
        instructions: &str,
        input: &[InputItem],
        tools: &Option<Vec<ResponsesToolDef>>,
        tokens: &AuthTokens,
    ) -> reqwest::RequestBuilder {
        let body = ResponsesRequest {
            model: self.model.clone(),
            instructions: instructions.to_string(),
            input: input.to_vec(),
            tools: tools.clone(),
            store: false,
            stream: true,
        };
        self.client
            .post(API_URL)
            .header("Authorization", format!("Bearer {}", tokens.access))
            .header("ChatGPT-Account-Id", &tokens.account_id)
            .header("originator", "opencode")
            .json(&body)
    }

    async fn send_request(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, Error> {
        let future = req.send();
        let resp = match self.timeout {
            Some(dur) => tokio::time::timeout(dur, future)
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(|e| Error::Api { status: 0, body: e.to_string() })?,
            None => future
                .await
                .map_err(|e| Error::Api { status: 0, body: e.to_string() })?,
        };
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api { status, body });
        }
        Ok(resp)
    }

    async fn call_api(
        &self,
        instructions: &str,
        input: &[InputItem],
        tools: &Option<Vec<ResponsesToolDef>>,
    ) -> Result<(ResponsesApiResponse, TokenUsage), Error> {
        let tokens = self.ensure_tokens().await?;
        let req = self.build_request(instructions, input, tools, &tokens);
        let resp = self.send_request(req).await?;
        let api_resp = parse_sse_response(resp).await?;
        let usage = api_resp
            .usage
            .as_ref()
            .map(responses_usage_to_token_usage)
            .unwrap_or_default();
        Ok((api_resp, usage))
    }
}

// --- ChatClient impl for llm-agent ---

#[async_trait::async_trait]
impl llm_agent::ChatClient for Codex {
    async fn chat(
        &self,
        messages: &[llm_agent::ChatMessage],
        tools: Option<&serde_json::Value>,
    ) -> Result<(llm_agent::Response, llm_agent::Usage), Box<dyn std::error::Error + Send + Sync>>
    {
        let instructions = extract_instructions(messages);
        let input: Vec<InputItem> = messages.iter().flat_map(chat_message_to_input).collect();
        let api_tools = tools_from_chat_json(tools);
        let (resp, usage) = self.call_api(&instructions, &input, &api_tools).await?;
        let parts = output_items_to_parts(&resp.output);
        let finish_reason = if has_tool_calls(&resp.output) {
            "tool_calls"
        } else {
            "stop"
        };
        let agent_usage = llm_agent::Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: 0,
        };
        Ok((
            llm_agent::Response { parts, finish_reason: finish_reason.to_string() },
            agent_usage,
        ))
    }
}

// --- Backend impl using AgentLoop ---

#[async_trait::async_trait]
impl Backend for Codex {
    async fn complete(&self, prompt: &str) -> Result<Output, Error> {
        let tools_json = self.tool_set.as_ref().map(tools_json_from_tool_set);
        match &self.tool_set {
            Some(ts) => {
                let executor = ToolSetExecutor { tool_set: ts };
                let agent = build_agent_loop(
                    self, executor, self.max_turns,
                    self.system_prompt.as_deref(), tools_json,
                );
                let output = agent
                    .run(prompt)
                    .await
                    .map_err(|e| Error::Parse(e.to_string()))?;
                Ok(agent_output_to_sdk(output))
            }
            None => {
                let agent = build_agent_loop(
                    self, NoOpExecutor, 1,
                    self.system_prompt.as_deref(), None,
                );
                let output = agent
                    .run(prompt)
                    .await
                    .map_err(|e| Error::Parse(e.to_string()))?;
                Ok(agent_output_to_sdk(output))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults() {
        let c = Codex::new("gpt-5.4");
        assert_eq!(c.model, "gpt-5.4");
        assert_eq!(c.max_turns, 20);
        assert!(c.system_prompt.is_none());
        assert!(c.tool_set.is_none());
    }

    #[test]
    fn builder_chaining() {
        let c = Codex::new("gpt-5.4")
            .system_prompt("Be helpful")
            .max_turns(10)
            .auth_path("/tmp/test-auth.json");
        assert_eq!(c.model, "gpt-5.4");
        assert_eq!(c.system_prompt.as_deref(), Some("Be helpful"));
        assert_eq!(c.max_turns, 10);
        assert_eq!(c.auth_path, PathBuf::from("/tmp/test-auth.json"));
    }

    #[test]
    fn auth_tokens_deserialization() {
        let json = r#"{
            "openai": {
                "type": "oauth",
                "refresh": "rt_test",
                "access": "eyJ_test",
                "expires": 1773663691802,
                "accountId": "fe2de890-test"
            }
        }"#;
        let auth: AuthFile = serde_json::from_str(json).unwrap();
        let tokens = auth.openai.unwrap();
        assert_eq!(tokens.refresh, "rt_test");
        assert_eq!(tokens.account_id, "fe2de890-test");
    }

    #[test]
    fn is_expired_check() {
        let tokens = AuthTokens {
            refresh: String::new(),
            access: String::new(),
            expires: 0,
            account_id: String::new(),
            auth_type: None,
        };
        assert!(is_expired(&tokens));

        let future_tokens = AuthTokens {
            expires: now_millis() + 3_600_000,
            ..tokens
        };
        assert!(!is_expired(&future_tokens));
    }

    #[test]
    fn extract_instructions_from_messages() {
        let msgs = vec![
            llm_agent::ChatMessage {
                role: "system".into(),
                content: Some("Custom instructions".into()),
                tool_calls: None,
                tool_call_id: None,
            },
            llm_agent::ChatMessage {
                role: "user".into(),
                content: Some("hello".into()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        assert_eq!(extract_instructions(&msgs), "Custom instructions");
    }

    #[test]
    fn extract_instructions_default() {
        let msgs = vec![llm_agent::ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            tool_calls: None,
            tool_call_id: None,
        }];
        assert_eq!(extract_instructions(&msgs), DEFAULT_INSTRUCTIONS);
    }

    #[test]
    fn chat_message_to_input_user() {
        let msg = llm_agent::ChatMessage {
            role: "user".into(),
            content: Some("hi".into()),
            tool_calls: None,
            tool_call_id: None,
        };
        let items = chat_message_to_input(&msg);
        assert_eq!(items.len(), 1);
        let json = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hi");
    }

    #[test]
    fn chat_message_to_input_tool_result() {
        let msg = llm_agent::ChatMessage {
            role: "tool".into(),
            content: Some("result text".into()),
            tool_calls: None,
            tool_call_id: Some("call_123".into()),
        };
        let items = chat_message_to_input(&msg);
        assert_eq!(items.len(), 1);
        let json = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(json["type"], "function_call_output");
        assert_eq!(json["call_id"], "call_123");
        assert_eq!(json["output"], "result text");
    }

    #[test]
    fn chat_message_to_input_assistant_with_tool_calls() {
        let msg = llm_agent::ChatMessage {
            role: "assistant".into(),
            content: Some("thinking...".into()),
            tool_calls: Some(vec![llm_agent::ToolCall {
                id: "call_abc".into(),
                call_type: "function".into(),
                function: llm_agent::FunctionCall {
                    name: "Bash".into(),
                    arguments: r#"{"command":"ls"}"#.into(),
                },
            }]),
            tool_call_id: None,
        };
        let items = chat_message_to_input(&msg);
        assert_eq!(items.len(), 2);
        let json0 = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(json0["type"], "message");
        assert_eq!(json0["content"], "thinking...");
        let json1 = serde_json::to_value(&items[1]).unwrap();
        assert_eq!(json1["type"], "function_call");
        assert_eq!(json1["call_id"], "call_abc");
        assert_eq!(json1["name"], "Bash");
    }

    #[test]
    fn chat_message_system_skipped() {
        let msg = llm_agent::ChatMessage {
            role: "system".into(),
            content: Some("system prompt".into()),
            tool_calls: None,
            tool_call_id: None,
        };
        let items = chat_message_to_input(&msg);
        assert!(items.is_empty());
    }

    #[test]
    fn tools_from_chat_json_conversion() {
        let chat_tools = serde_json::json!([{
            "type": "function",
            "function": {
                "name": "Bash",
                "description": "Run a command",
                "parameters": {"type": "object"}
            }
        }]);
        let defs = tools_from_chat_json(Some(&chat_tools)).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Bash");
        assert_eq!(defs[0].description, "Run a command");
        assert_eq!(defs[0].tool_type, "function");
    }

    #[test]
    fn output_item_text_to_parts() {
        let items = vec![OutputItem::Message {
            content: vec![ContentPart::OutputText { text: "Hello".into() }],
        }];
        let parts = output_items_to_parts(&items);
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], llm_agent::Part::Text(t) if t == "Hello"));
    }

    #[test]
    fn output_item_function_call_to_parts() {
        let items = vec![OutputItem::FunctionCall {
            call_id: "call_1".into(),
            name: "Bash".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
        }];
        let parts = output_items_to_parts(&items);
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            llm_agent::Part::ToolUse(tc) => {
                assert_eq!(tc.id, "call_1");
                assert_eq!(tc.function.name, "Bash");
            }
            _ => panic!("expected ToolUse"),
        }
        assert!(has_tool_calls(&items));
    }
}
