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

    /// Whether this tool can run in parallel with other parallel-safe tools.
    /// Default: false (serial execution).
    fn supports_parallel(&self) -> bool {
        false
    }
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

    /// Standard set with Bash sandboxed via a command prefix (e.g. bwrap).
    pub fn standard_sandboxed(command_prefix: Vec<String>) -> Self {
        Self::new()
            .add(read::ReadTool)
            .add(write::WriteTool)
            .add(glob::GlobTool)
            .add(grep::GrepTool)
            .add(bash::BashTool::new().with_command_prefix(command_prefix))
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

    /// Merge another ToolSet into this one (consumes both).
    pub fn merge(mut self, other: ToolSet) -> Self {
        self.tools.extend(other.tools);
        self
    }

    pub fn supports_parallel(&self, tool_name: &str) -> bool {
        self.tools
            .iter()
            .find(|t| t.definition().name == tool_name)
            .is_some_and(|t| t.supports_parallel())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names(set: &ToolSet) -> Vec<String> {
        set.definitions().iter().map(|d| d.name.clone()).collect()
    }

    #[test]
    fn standard_has_five_tools() {
        let names = tool_names(&ToolSet::standard());
        assert_eq!(names, vec!["Read", "Write", "Glob", "Grep", "Bash"]);
    }

    #[test]
    fn standard_sandboxed_has_same_tools() {
        let names = tool_names(&ToolSet::standard_sandboxed(vec!["bwrap".into(), "--".into()]));
        assert_eq!(names, vec!["Read", "Write", "Glob", "Grep", "Bash"]);
    }

    #[tokio::test]
    async fn sandboxed_bash_uses_prefix() {
        // "env --" is a transparent wrapper that proves the prefix is wired through
        let set = ToolSet::standard_sandboxed(vec!["env".into(), "--".into()]);
        let call = ToolCall {
            id: "1".into(),
            name: "Bash".into(),
            arguments: r#"{"command": "echo works"}"#.into(),
        };
        let result = set.execute(&call).await;
        assert_eq!(result.trim(), "works");
    }
}
