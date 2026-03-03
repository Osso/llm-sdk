use super::{Tool, ToolDef};
use serde::Deserialize;
use std::path::Path;

pub struct WriteTool;

#[derive(Deserialize)]
struct Args {
    file_path: String,
    content: String,
}

#[async_trait::async_trait]
impl ToolDef for WriteTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "Write".into(),
            description: "Write content to a file, creating parent directories if needed.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute path to write to" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["file_path", "content"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> String {
        let args: Args = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("Invalid arguments: {e}"),
        };
        let path = Path::new(&args.file_path);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return format!("Error creating directories: {e}");
            }
        }
        match std::fs::write(path, &args.content) {
            Ok(()) => format!("Wrote {} bytes to {}", args.content.len(), args.file_path),
            Err(e) => format!("Error writing {}: {e}", args.file_path),
        }
    }
}
