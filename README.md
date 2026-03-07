# llm-sdk

Rust library for LLM completions via Claude CLI and OpenRouter API.

## Backends

- **Claude** — wraps the `claude` CLI with builder-pattern config (model, session, system prompt, allowed tools, timeout, sandbox prefix). Parses `stream-json` output for streaming events.
- **OpenRouter** — HTTP client for the OpenRouter API with multi-turn tool-use loops.

Both implement the `Backend` trait:

```rust
#[async_trait]
pub trait Backend: Send + Sync {
    async fn complete(&self, prompt: &str) -> Result<Output, Error>;
}
```

## Tools

Built-in tool implementations for agentic workflows (OpenRouter backend):

| Tool | Description |
|------|-------------|
| Read | Read files |
| Write | Write files |
| Glob | Find files by pattern |
| Grep | Search file contents |
| Bash | Execute shell commands |

`ToolSet::standard()` registers all five. `ToolSet::standard_sandboxed(prefix)` wraps Bash with a command prefix (e.g. bwrap).

## Other modules

- **sandbox** — bubblewrap (bwrap) sandbox helpers for agent isolation
- **session** — persistent session ID store with JSONL logging
- **message_log** — conversation history tracking
- **stream** — Claude CLI `stream-json` event parsing

## Usage

```rust
use llm_sdk::{Backend, claude::Claude};

let claude = Claude::new()
    .model("sonnet")
    .system_prompt("You are a helpful assistant.");

let output = claude.complete("Hello").await?;
println!("{}", output.text);
```

```rust
use llm_sdk::{Backend, openrouter::OpenRouter, tools::ToolSet};

let or = OpenRouter::new("openai/gpt-4.4")
    .api_key(key)
    .tools(ToolSet::standard());

let output = or.complete("List files in src/").await?;
```

## License

MIT
