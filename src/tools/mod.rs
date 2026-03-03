mod bash;
mod glob;
mod grep;
mod read;
mod write;

use std::sync::Arc;

/// Definition sent to the API.
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool call returned by the model.
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Trait each tool implements — definition + execution.
#[async_trait::async_trait]
pub trait ToolDef: Send + Sync {
    fn definition(&self) -> Tool;
    async fn execute(&self, arguments: &str) -> String;
}

/// Registry of tools.
pub struct ToolSet {
    tools: Vec<Arc<dyn ToolDef>>,
}

impl ToolSet {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn add(mut self, tool: impl ToolDef + 'static) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Standard set: Read + Write + Glob + Grep + Bash.
    pub fn standard() -> Self {
        Self::new()
            .add(read::ReadTool)
            .add(write::WriteTool)
            .add(glob::GlobTool)
            .add(grep::GrepTool)
            .add(bash::BashTool::new())
    }

    pub fn definitions(&self) -> Vec<Tool> {
        self.tools.iter().map(|t| t.definition()).collect()
    }

    pub async fn execute(&self, call: &ToolCall) -> String {
        for tool in &self.tools {
            let def = tool.definition();
            if def.name == call.name {
                return tool.execute(&call.arguments).await;
            }
        }
        format!("Unknown tool: {}", call.name)
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
