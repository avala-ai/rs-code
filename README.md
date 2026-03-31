# rust-code

An AI-powered coding agent for the terminal, written in pure Rust.

## Features

- **Interactive REPL** with streaming responses and markdown rendering
- **Tool system** for file operations, shell commands, and code search
- **Permission system** with configurable rules (allow/deny/ask per tool)
- **Agent loop** that autonomously executes multi-step coding tasks
- **Extensible architecture** with traits for tools, LLM clients, and hooks

## Architecture

```
┌──────────────────────────────────────────────────────┐
│              ENTRYPOINT (main.rs / cli)               │
│         CLI parsing, initialization, bootstrap        │
└────────────────────────┬─────────────────────────────┘
                         │
┌────────────────────────▼─────────────────────────────┐
│           CONFIG & BOOTSTRAP (config/)                │
│     Settings loading, environment, initialization     │
└────────────────────────┬─────────────────────────────┘
                         │
┌────────────────────────▼─────────────────────────────┐
│            QUERY ENGINE (query/)                      │
│   Agent loop: prompt → stream → tools → loop          │
└────────┬───────────┬──────────────┬──────────────────┘
         │           │              │
┌────────▼──┐  ┌─────▼────┐  ┌─────▼──────┐
│ TOOL LAYER│  │PERMISSION │  │ HOOKS      │
│           │  │  SYSTEM   │  │            │
│ Registry  │  │ Rules     │  │ Pre/Post   │
│ Executor  │  │ Check/Ask │  │ Lifecycle  │
│ Streaming │  │ Modes     │  │            │
└───────────┘  └───────────┘  └────────────┘
         │           │              │
┌────────▼───────────▼──────────────▼──────────────────┐
│              SERVICES & STATE                         │
│  LLM Client  │  App State  │  Compact  │  Memory     │
└──────────────────────────────────────────────────────┘
```

## Quick Start

```bash
# Build
cargo build --release

# Set your API key
export RC_API_KEY="your-api-key"

# Run
./target/release/rc
```

## Configuration

Configuration is loaded from (highest to lowest priority):
1. CLI flags and environment variables
2. Project-local `.rc/settings.toml`
3. User config `~/.config/rust-code/config.toml`

```toml
[api]
base_url = "https://api.example.com/v1"
model = "default-model"

[permissions]
default_mode = "ask"  # "ask" | "allow" | "deny"

[[permissions.rules]]
tool = "Bash"
pattern = "git *"
action = "allow"
```

## Tool System

Built-in tools:

| Tool | Description |
|------|-------------|
| `Bash` | Execute shell commands |
| `FileRead` | Read file contents with line ranges |
| `FileWrite` | Create or overwrite files |
| `FileEdit` | Targeted search-and-replace editing |
| `Grep` | Regex content search (ripgrep-powered) |
| `Glob` | File pattern matching |

Tools implement the `Tool` trait:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> serde_json::Value;
    async fn call(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult>;
    fn is_read_only(&self) -> bool;
    fn is_concurrency_safe(&self) -> bool;
}
```

## License

MIT
