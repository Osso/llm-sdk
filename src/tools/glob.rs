use super::{Tool, ToolDef};
use serde::Deserialize;

pub struct GlobTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    path: Option<String>,
}

#[async_trait::async_trait]
impl ToolDef for GlobTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "Glob".into(),
            description: "Find files matching a glob pattern.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern (e.g. '**/*.rs')" },
                    "path": { "type": "string", "description": "Base directory (defaults to cwd)" }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn supports_parallel(&self) -> bool { true }

    async fn execute(&self, arguments: &str) -> String {
        let args: Args = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("Invalid arguments: {e}"),
        };
        let base = args.path.as_deref().unwrap_or(".");
        let full_pattern = if args.pattern.starts_with('/') {
            args.pattern.clone()
        } else {
            format!("{base}/{}", args.pattern)
        };
        let mut matches = Vec::new();
        let entries = match glob::glob(&full_pattern) {
            Ok(e) => e,
            Err(e) => return format!("Invalid pattern: {e}"),
        };
        for entry in entries {
            match entry {
                Ok(path) => {
                    matches.push(path.display().to_string());
                    if matches.len() >= 100 {
                        break;
                    }
                }
                Err(e) => matches.push(format!("Error: {e}")),
            }
        }
        if matches.is_empty() {
            "No matches found".into()
        } else {
            matches.join("\n")
        }
    }
}
