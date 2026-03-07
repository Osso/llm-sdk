use super::{Tool, ToolDef};
use serde::Deserialize;
use std::path::Path;

pub struct ReadTool;

#[derive(Deserialize)]
struct Args {
    file_path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[async_trait::async_trait]
impl ToolDef for ReadTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "Read".into(),
            description: "Read a file and return its contents with line numbers.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute path to the file" },
                    "offset": { "type": "number", "description": "Line number to start from (1-based)" },
                    "limit": { "type": "number", "description": "Max lines to read" }
                },
                "required": ["file_path"]
            }),
        }
    }

    fn supports_parallel(&self) -> bool { true }

    async fn execute(&self, arguments: &str) -> String {
        let args: Args = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("Invalid arguments: {e}"),
        };
        let path = Path::new(&args.file_path);
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return format!("Error reading {}: {e}", args.file_path),
        };
        let lines: Vec<&str> = content.lines().collect();
        let offset = args.offset.unwrap_or(1).max(1) - 1;
        let limit = args.limit.unwrap_or(2000);
        let end = (offset + limit).min(lines.len());

        let mut out = String::new();
        for (i, line) in lines[offset..end].iter().enumerate() {
            let line_num = offset + i + 1;
            out.push_str(&format!("{line_num:>6}\t{line}\n"));
        }
        out
    }
}
