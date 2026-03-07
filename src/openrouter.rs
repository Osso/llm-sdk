use crate::message_log::{ChatMessage, MessageLog, ToolCallRecord};
use crate::openai_shared::{
    agent_msg_to_api, agent_output_to_sdk, api_usage_to_token_usage, build_agent_loop,
    parse_chat_response, tool_defs_to_api, tools_json_from_tool_set, ApiResponse, ApiRequest,
    ApiToolCall, ApiToolDef, ApiFunctionCall, Message, NoOpExecutor, ToolSetExecutor,
};
use crate::session::now_utc;
use crate::tools::ToolSet;
use crate::{Backend, Error, Output, TokenUsage};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    system_prompt: Option<String>,
    tool_set: Option<ToolSet>,
    timeout: Option<Duration>,
    max_turns: u32,
    site_url: Option<String>,
    site_name: Option<String>,
}

impl OpenRouter {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.into(),
            api_key: String::new(),
            model: model.into(),
            system_prompt: None,
            tool_set: None,
            timeout: None,
            max_turns: 20,
            site_url: None,
            site_name: None,
        }
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = key.into();
        self
    }

    pub fn api_key_env(mut self, var: &str) -> Self {
        self.api_key = std::env::var(var).unwrap_or_default();
        self
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

    pub fn site_url(mut self, url: impl Into<String>) -> Self {
        self.site_url = Some(url.into());
        self
    }

    pub fn site_name(mut self, name: impl Into<String>) -> Self {
        self.site_name = Some(name.into());
        self
    }
}

// --- HTTP transport ---

impl OpenRouter {
    fn build_request(
        &self,
        messages: &[Message],
        tools: &Option<Vec<ApiToolDef>>,
    ) -> reqwest::RequestBuilder {
        let body = ApiRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            tools: tools
                .as_ref()
                .and_then(|t| if t.is_empty() { None } else { Some(t.clone()) }),
        };
        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body);
        if let Some(ref url) = self.site_url {
            req = req.header("HTTP-Referer", url);
        }
        if let Some(ref name) = self.site_name {
            req = req.header("X-Title", name);
        }
        req
    }

    async fn send_request(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, Error> {
        let future = req.send();
        let resp = match self.timeout {
            Some(dur) => tokio::time::timeout(dur, future)
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(|e| Error::Api { status: 0, body: e.to_string() })?,
            None => future.await.map_err(|e| Error::Api { status: 0, body: e.to_string() })?,
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
        let req = self.build_request(messages, tools);
        let resp = self.send_request(req).await?;
        let api_resp: ApiResponse = resp.json().await.map_err(|e| Error::Parse(e.to_string()))?;
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
impl llm_agent::ChatClient for OpenRouter {
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
impl Backend for OpenRouter {
    async fn complete(&self, prompt: &str) -> Result<Output, Error> {
        let tools_json = self.tool_set.as_ref().map(tools_json_from_tool_set);
        match &self.tool_set {
            Some(ts) => {
                let executor = ToolSetExecutor { tool_set: ts };
                let agent = build_agent_loop(
                    self, executor, self.max_turns,
                    self.system_prompt.as_deref(), tools_json,
                );
                let output = agent.run(prompt).await
                    .map_err(|e| Error::Parse(e.to_string()))?;
                Ok(agent_output_to_sdk(output))
            }
            None => {
                let agent = build_agent_loop(
                    self, NoOpExecutor, 1,
                    self.system_prompt.as_deref(), None,
                );
                let output = agent.run(prompt).await
                    .map_err(|e| Error::Parse(e.to_string()))?;
                Ok(agent_output_to_sdk(output))
            }
        }
    }
}

// --- complete_chat: agentic loop with MessageLog persistence ---

fn chat_message_to_api(msg: &ChatMessage) -> Message {
    Message {
        role: msg.role.clone(),
        content: msg.content.clone(),
        tool_calls: msg.tool_calls.as_ref().map(|tcs| {
            tcs.iter()
                .map(|tc| ApiToolCall {
                    id: tc.id.clone(),
                    call_type: "function".into(),
                    function: ApiFunctionCall {
                        name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    },
                })
                .collect()
        }),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

fn api_tool_call_to_record(tc: &ApiToolCall) -> ToolCallRecord {
    ToolCallRecord {
        id: tc.id.clone(),
        name: tc.function.name.clone(),
        arguments: tc.function.arguments.clone(),
    }
}

fn push_user_message(log: &mut MessageLog, prompt: &str) {
    log.push(ChatMessage {
        role: "user".into(),
        content: Some(prompt.into()),
        tool_calls: None,
        tool_call_id: None,
        timestamp: now_utc(),
    });
}

fn push_final_assistant(log: &mut MessageLog, text: &str) {
    log.push(ChatMessage {
        role: "assistant".into(),
        content: Some(text.into()),
        tool_calls: None,
        tool_call_id: None,
        timestamp: now_utc(),
    });
}

fn push_assistant_tool_calls(
    log: &mut MessageLog,
    tool_calls: &[ApiToolCall],
    content: Option<String>,
) {
    let records: Vec<ToolCallRecord> = tool_calls.iter().map(api_tool_call_to_record).collect();
    log.push(ChatMessage {
        role: "assistant".into(),
        content,
        tool_calls: Some(records),
        tool_call_id: None,
        timestamp: now_utc(),
    });
}

fn push_tool_result(log: &mut MessageLog, id: &str, result: String) {
    log.push(ChatMessage {
        role: "tool".into(),
        content: Some(result),
        tool_calls: None,
        tool_call_id: Some(id.into()),
        timestamp: now_utc(),
    });
}

enum ChatTurnResult {
    Final(Output),
    Continue,
}

impl OpenRouter {
    /// Run the agentic completion loop, persisting every message to `log`.
    pub async fn complete_chat(
        &self,
        log: &mut MessageLog,
        prompt: &str,
    ) -> Result<Output, Error> {
        push_user_message(log, prompt);
        let api_tools = self.tool_set.as_ref().map(tool_defs_to_api);
        let mut total_usage = TokenUsage::default();

        for turn in 0..self.max_turns {
            match self.run_chat_turn(log, &api_tools, &mut total_usage, turn).await? {
                ChatTurnResult::Final(output) => return Ok(output),
                ChatTurnResult::Continue => {}
            }
        }
        Err(Error::MaxTurns(self.max_turns))
    }

    async fn run_chat_turn(
        &self,
        log: &mut MessageLog,
        api_tools: &Option<Vec<ApiToolDef>>,
        total_usage: &mut TokenUsage,
        turn: u32,
    ) -> Result<ChatTurnResult, Error> {
        tracing::info!(turn, "OpenRouter chat API call ({} messages)", log.messages().len());
        let messages: Vec<Message> = log.messages().iter().map(chat_message_to_api).collect();
        let (resp, usage) = self.call_api(&messages, api_tools).await?;
        total_usage.accumulate(&usage);
        let choice = resp.choices.into_iter().next()
            .ok_or_else(|| Error::Parse("no choices in response".into()))?;
        let tool_calls = choice.message.tool_calls.unwrap_or_default();
        if tool_calls.is_empty() {
            let text = choice.message.content.unwrap_or_default();
            push_final_assistant(log, &text);
            return Ok(ChatTurnResult::Final(Output {
                text, usage: Some(total_usage.clone()), session_id: None, cost_usd: None,
            }));
        }
        let tool_names: Vec<&str> = tool_calls.iter().map(|tc| tc.function.name.as_str()).collect();
        tracing::info!(turn, "tool calls: {:?}", tool_names);
        if let Some(ref ts) = self.tool_set {
            self.run_tool_turn(log, tool_calls, choice.message.content, ts).await;
        }
        Ok(ChatTurnResult::Continue)
    }

    async fn run_tool_turn(
        &self,
        log: &mut MessageLog,
        tool_calls: Vec<ApiToolCall>,
        content: Option<String>,
        tool_set: &ToolSet,
    ) {
        push_assistant_tool_calls(log, &tool_calls, content);
        for tc in &tool_calls {
            let call = crate::tools::ToolCall {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            };
            let result = tool_set.execute(&call).await;
            tracing::info!(tool = %call.name, "tool result: {} bytes", result.len());
            push_tool_result(log, &tc.id, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_shared::ApiUsage;

    #[test]
    fn api_usage_conversion() {
        let api = ApiUsage { prompt_tokens: 100, completion_tokens: 50 };
        let usage = api_usage_to_token_usage(&api);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn tool_call_deserialization() {
        let json = r#"{
            "id": "call_123",
            "type": "function",
            "function": { "name": "Read", "arguments": "{\"file_path\": \"/tmp/test\"}" }
        }"#;
        let tc: ApiToolCall = serde_json::from_str(json).unwrap();
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.function.name, "Read");
    }

    #[test]
    fn api_response_deserialization() {
        let json = r#"{
            "choices": [{ "message": { "content": "Hello!", "tool_calls": null } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        }"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.usage.unwrap().prompt_tokens, 10);
    }

    #[test]
    fn api_response_with_tool_calls() {
        let json = r#"{
            "choices": [{ "message": { "content": null, "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": { "name": "Bash", "arguments": "{\"command\": \"ls\"}" }
            }] } }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 10 }
        }"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        let tool_calls = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].function.name, "Bash");
    }

    #[test]
    fn builder_defaults() {
        let or = OpenRouter::new("test-model");
        assert_eq!(or.model, "test-model");
        assert_eq!(or.base_url, DEFAULT_BASE_URL);
        assert_eq!(or.max_turns, 20);
        assert!(or.system_prompt.is_none());
        assert!(or.tool_set.is_none());
    }

    #[test]
    fn builder_chaining() {
        let or = OpenRouter::new("model")
            .base_url("https://custom.api/v1")
            .api_key("sk-test")
            .system_prompt("Be helpful")
            .max_turns(10)
            .site_url("https://example.com")
            .site_name("MyApp");
        assert_eq!(or.base_url, "https://custom.api/v1");
        assert_eq!(or.api_key, "sk-test");
        assert_eq!(or.system_prompt.as_deref(), Some("Be helpful"));
        assert_eq!(or.max_turns, 10);
        assert_eq!(or.site_url.as_deref(), Some("https://example.com"));
        assert_eq!(or.site_name.as_deref(), Some("MyApp"));
    }
}
