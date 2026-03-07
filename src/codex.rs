use crate::openai_shared::{
    agent_msg_to_api, agent_output_to_sdk, api_usage_to_token_usage, build_agent_loop,
    parse_chat_response, tools_json_from_tool_set, ApiRequest, ApiResponse, ApiToolDef, Message,
    NoOpExecutor, ToolSetExecutor,
};
use crate::tools::ToolSet;
use crate::{Backend, Error, Output, TokenUsage};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

const API_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_MODEL: &str = "gpt-5.4";

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

// --- HTTP transport ---

impl Codex {
    fn build_request(
        &self,
        messages: &[Message],
        tools: &Option<Vec<ApiToolDef>>,
        tokens: &AuthTokens,
    ) -> reqwest::RequestBuilder {
        let body = ApiRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            tools: tools
                .as_ref()
                .and_then(|t| if t.is_empty() { None } else { Some(t.clone()) }),
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
        messages: &[Message],
        tools: &Option<Vec<ApiToolDef>>,
    ) -> Result<(ApiResponse, TokenUsage), Error> {
        let tokens = self.ensure_tokens().await?;
        let req = self.build_request(messages, tools, &tokens);
        let resp = self.send_request(req).await?;
        let api_resp: ApiResponse = resp
            .json()
            .await
            .map_err(|e| Error::Parse(e.to_string()))?;
        let usage = api_resp
            .usage
            .as_ref()
            .map(api_usage_to_token_usage)
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
        let api_messages: Vec<Message> = messages.iter().map(agent_msg_to_api).collect();
        let api_tools: Option<Vec<ApiToolDef>> =
            tools.and_then(|v| serde_json::from_value(v.clone()).ok());
        let (resp, usage) = self.call_api(&api_messages, &api_tools).await?;
        parse_chat_response(resp, usage)
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
        let c = Codex::new(DEFAULT_MODEL);
        assert_eq!(c.model, DEFAULT_MODEL);
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
}
