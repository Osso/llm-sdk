use crate::openai_shared::{
    agent_msg_to_api, agent_output_to_sdk, build_agent_loop,
    parse_chat_response, tools_json_from_tool_set, ApiRequest, ApiResponse, ApiToolDef, Message,
    NoOpExecutor, ToolSetExecutor,
};
use crate::tools::ToolSet;
use crate::{Backend, Error, Output, TokenUsage};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAI {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    system_prompt: Option<String>,
    tool_set: Option<ToolSet>,
    timeout: Option<Duration>,
    max_turns: u32,
}

impl OpenAI {
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
}

// --- HTTP transport ---

impl OpenAI {
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
        self.client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
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
            .map(crate::openai_shared::api_usage_to_token_usage)
            .unwrap_or_default();
        Ok((api_resp, usage))
    }
}

// --- ChatClient impl for llm-agent ---

#[async_trait::async_trait]
impl llm_agent::ChatClient for OpenAI {
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
impl Backend for OpenAI {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_shared::{ApiUsage, ApiToolCall};

    #[test]
    fn builder_defaults() {
        let o = OpenAI::new("gpt-4.4");
        assert_eq!(o.model, "gpt-4.4");
        assert_eq!(o.base_url, DEFAULT_BASE_URL);
        assert_eq!(o.max_turns, 20);
        assert!(o.system_prompt.is_none());
        assert!(o.tool_set.is_none());
    }

    #[test]
    fn builder_chaining() {
        let o = OpenAI::new("gpt-4.4")
            .base_url("https://custom.api/v1")
            .api_key("sk-test")
            .system_prompt("Be helpful")
            .max_turns(10);
        assert_eq!(o.base_url, "https://custom.api/v1");
        assert_eq!(o.api_key, "sk-test");
        assert_eq!(o.system_prompt.as_deref(), Some("Be helpful"));
        assert_eq!(o.max_turns, 10);
    }

    #[test]
    fn api_usage_conversion() {
        let api = ApiUsage { prompt_tokens: 100, completion_tokens: 50 };
        let usage = crate::openai_shared::api_usage_to_token_usage(&api);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
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
}
