use super::{Tool, ToolDef};
use serde::Deserialize;
use std::io::{BufRead, BufReader};

pub struct GrepTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
}

#[async_trait::async_trait]
impl ToolDef for GrepTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "Grep".into(),
            description: "Search file contents with regex. Returns file:line:content matches.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "Directory to search (defaults to cwd)" },
                    "glob": { "type": "string", "description": "File glob filter (e.g. '*.rs')" }
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
        let re = match regex::Regex::new(&args.pattern) {
            Ok(r) => r,
            Err(e) => return format!("Invalid regex: {e}"),
        };
        let base = args.path.as_deref().unwrap_or(".");
        let matches = search_files(&re, base, args.glob.as_deref());
        if matches.is_empty() {
            "No matches found".into()
        } else {
            matches.join("\n")
        }
    }
}

fn search_files(re: &regex::Regex, base: &str, glob_pattern: Option<&str>) -> Vec<String> {
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(base)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !matches_glob_filter(path, glob_pattern) {
            continue;
        }
        search_file(re, path, &mut matches);
        if matches.len() >= 50 {
            break;
        }
    }
    matches
}

fn matches_glob_filter(path: &std::path::Path, glob_pattern: Option<&str>) -> bool {
    let Some(glob_pat) = glob_pattern else {
        return true;
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    glob_to_regex(glob_pat).is_match(name)
}

fn search_file(re: &regex::Regex, path: &std::path::Path, matches: &mut Vec<String>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if let Ok(line) = line {
            if re.is_match(&line) {
                matches.push(format!("{}:{}:{}", path.display(), i + 1, line));
                if matches.len() >= 50 {
                    return;
                }
            }
        }
    }
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|s| s.starts_with('.') || s == "node_modules" || s == "target" || s == "vendor")
}

fn glob_to_regex(pattern: &str) -> regex::Regex {
    let escaped = regex::escape(pattern).replace(r"\*", ".*").replace(r"\?", ".");
    regex::Regex::new(&format!("^{escaped}$")).unwrap_or_else(|_| regex::Regex::new(".*").unwrap())
}
