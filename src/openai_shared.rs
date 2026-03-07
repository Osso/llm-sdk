//! Shared types and helpers for OpenAI-compatible API backends (OpenAI, OpenRouter).

use crate::tools::ToolSet;
use crate::{Error, Output, TokenUsage};
use serde::{Deserialize, Serialize};

// --- API request/response types ---

#[derive(Serialize)]
pub(crate) struct ApiRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ApiToolDef>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ApiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ApiToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ApiFunction,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct ApiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ApiFunctionCall,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ApiFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct ApiFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Deserialize)]
pub(crate) struct ApiResponse {
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
pub(crate) struct Choice {
    pub message: MessageResponse,
}

#[derive(Deserialize)]
pub(crate) struct MessageResponse {
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Deserialize)]
pub(crate) struct ApiUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
}

// --- Conversions ---

pub(crate) fn tool_defs_to_api(tool_set: &ToolSet) -> Vec<ApiToolDef> {
    tool_set
        .definitions()
        .into_iter()
        .map(|t| ApiToolDef {
            tool_type: "function".into(),
            function: ApiFunction {
                name: t.name,
                description: t.description,
                parameters: t.parameters,
            },
        })
        .collect()
}

pub(crate) fn api_usage_to_token_usage(u: &ApiUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        ..Default::default()
    }
}

pub(crate) fn agent_msg_to_api(msg: &llm_agent::ChatMessage) -> Message {
    Message {
        role: msg.role.clone(),
        content: msg.content.clone(),
        tool_calls: msg.tool_calls.as_ref().map(|tcs| {
            tcs.iter()
                .map(|tc| ApiToolCall {
                    id: tc.id.clone(),
                    call_type: tc.call_type.clone(),
                    function: ApiFunctionCall {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    },
                })
                .collect()
        }),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

pub(crate) fn api_tool_call_to_agent(tc: &ApiToolCall) -> llm_agent::ToolCall {
    llm_agent::ToolCall {
        id: tc.id.clone(),
        call_type: tc.call_type.clone(),
        function: llm_agent::FunctionCall {
            name: tc.function.name.clone(),
            arguments: tc.function.arguments.clone(),
        },
    }
}

pub(crate) fn tools_json_from_tool_set(tool_set: &ToolSet) -> serde_json::Value {
    let api_tools = tool_defs_to_api(tool_set);
    serde_json::to_value(&api_tools).unwrap_or(serde_json::Value::Null)
}

pub(crate) fn parse_chat_response(
    resp: ApiResponse,
    usage: TokenUsage,
) -> Result<(llm_agent::Response, llm_agent::Usage), Box<dyn std::error::Error + Send + Sync>> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| Error::Parse("no choices in response".into()))?;
    let tool_calls = choice.message.tool_calls.unwrap_or_default();
    let agent_usage = llm_agent::Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
    };
    if tool_calls.is_empty() {
        Ok((
            llm_agent::Response::Text(choice.message.content.unwrap_or_default()),
            agent_usage,
        ))
    } else {
        let calls = tool_calls.iter().map(api_tool_call_to_agent).collect();
        Ok((
            llm_agent::Response::ToolCalls {
                text: choice.message.content,
                calls,
            },
            agent_usage,
        ))
    }
}

// --- AgentLoop helpers ---

pub(crate) fn agent_output_to_sdk(output: llm_agent::AgentOutput) -> Output {
    Output {
        text: output.text,
        usage: Some(TokenUsage {
            input_tokens: output.usage.input_tokens,
            output_tokens: output.usage.output_tokens,
            ..Default::default()
        }),
        session_id: None,
        cost_usd: None,
    }
}

pub(crate) fn build_agent_loop<'a, C: llm_agent::ChatClient, T: llm_agent::ToolExecutor>(
    client: &'a C,
    executor: T,
    max_turns: u32,
    system_prompt: Option<&str>,
    tools_json: Option<serde_json::Value>,
) -> llm_agent::AgentLoop<&'a C, T> {
    let mut builder = llm_agent::AgentLoop::new(client, executor).max_turns(max_turns);
    if let Some(sp) = system_prompt {
        builder = builder.system_prompt(sp);
    }
    if let Some(tj) = tools_json {
        builder = builder.tools_json(tj);
    }
    builder
}

// --- ToolExecutor adapters ---

pub(crate) struct ToolSetExecutor<'a> {
    pub tool_set: &'a ToolSet,
}

#[async_trait::async_trait]
impl llm_agent::ToolExecutor for ToolSetExecutor<'_> {
    async fn execute(&self, name: &str, arguments: &str) -> String {
        let call = crate::tools::ToolCall {
            id: String::new(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        };
        self.tool_set.execute(&call).await
    }
}

pub(crate) struct NoOpExecutor;

#[async_trait::async_trait]
impl llm_agent::ToolExecutor for NoOpExecutor {
    async fn execute(&self, _name: &str, _arguments: &str) -> String {
        String::new()
    }
}
