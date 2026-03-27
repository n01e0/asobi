# asobi

A minimal coding agent with a TUI interface, powered by AWS Bedrock.

## Features

- Interactive chat UI built with [ratatui](https://ratatui.rs/)
- Non-interactive mode for CI/CD and scripting
- Streaming response from AWS Bedrock (Converse Stream API)
- Tool use: `read_file`, `write_file`, `run_command`, `list_files`
- Session-based conversation history (`~/.asobi/sessions/<ID>.jsonl`)
- Session restore with `--restore <ID>` or `-c` (continue last)
- Configurable model via CLI args or environment variables
- Model ID is automatically resolved to a region-specific ARN from `AWS_REGION`

## Requirements

- Rust 1.85+
- AWS credentials configured (`~/.aws/credentials`, environment variables, or IAM role)
- Access to models on AWS Bedrock

## Installation

```sh
cargo install --path .
```

## Usage

### Interactive Mode

```sh
asobi
```

Each session gets a unique ID. On exit, a restore hint is printed:

```
To resume this session:
  asobi --restore 9f3a1b2c-...
```

### Session Management

```sh
# Continue the most recent session
asobi -c

# Restore a specific session by ID
asobi --restore 9f3a1b2c-4d5e-6f7a-8b9c-0d1e2f3a4b5c
```

### Non-interactive Mode

```sh
# Pass prompt directly
asobi --prompt "Explain the code in src/main.rs"

# Read prompt from a file
asobi --prompt-file prompt.txt

# Read prompt from stdin
echo "Fix the build errors" | asobi --prompt-file -

# Combine with other options
asobi -m us.anthropic.claude-haiku-4-5-20251001-v1:0 -p "List all TODO comments"
```

In non-interactive mode:
- Agent text output goes to **stdout**
- Tool calls and results are logged to **stderr**
- Exit code is **1** if the agent encountered an error, **0** otherwise

### GitHub Actions Example

```yaml
- name: Run coding agent
  run: |
    asobi --prompt-file .github/prompts/review.txt > result.txt 2> agent.log
  env:
    AWS_REGION: us-west-2
```

### Options

```
-m, --model <MODEL_ID>            Bedrock model ID or ARN [env: ASOBI_MODEL]
                                   [default: openai.gpt-oss-120b-1:0]
    --system-prompt <PROMPT>       System prompt [env: ASOBI_SYSTEM_PROMPT]
-p, --prompt <PROMPT>              Run non-interactively with the given prompt
-f, --prompt-file <PATH>           Run non-interactively with prompt from file ("-" for stdin)
-c, --continue                     Continue the most recent session
    --restore <SESSION_ID>         Restore a specific session by ID
-h, --help                         Print help
-V, --version                      Print version
```

When a model ID (not an ARN) is given, it is automatically expanded to
`arn:aws:bedrock:<REGION>::foundation-model/<MODEL_ID>` using the configured AWS region.
You can also pass a full ARN directly to skip this resolution.

### Examples

```sh
# Use a different model (ARN is built automatically from AWS_REGION)
asobi --model us.anthropic.claude-haiku-4-5-20251001-v1:0

# Pass an explicit ARN
asobi --model arn:aws:bedrock:eu-west-1::foundation-model/openai.gpt-oss-120b-1:0

# Set model via environment variable
export ASOBI_MODEL=openai.gpt-oss-120b-1:0
asobi

# Custom system prompt
asobi --system-prompt "You are a Rust expert. Always prefer idiomatic Rust."
```

## Key Bindings (Interactive Mode)

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Ctrl+C` (x2) | Quit |
| `Ctrl+D` (x2) | Quit |
| `Ctrl+H` | Delete character (same as Backspace) |
| `Ctrl+U` | Clear input line |
| `Up/Down` | Scroll chat history |
| `Left/Right` | Move cursor in input |
| `Backspace` | Delete character |

Type `/quit` and press Enter to exit.

## Configuration

| Environment Variable | Description |
|---------------------|-------------|
| `ASOBI_MODEL` | Bedrock model ID or ARN |
| `ASOBI_SYSTEM_PROMPT` | System prompt |
| `ASOBI_HISTORY_DIR` | Directory for session files (default: `~/.asobi`) |
| `AWS_REGION` | AWS region (used for ARN resolution) |
| `AWS_PROFILE` | AWS profile name |

## Architecture

```
main.rs    -- Entry point, TUI event loop, clap CLI, non-interactive mode
agent.rs   -- Bedrock Converse Stream API, tool execution loop, ARN resolution
tools.rs   -- Tool definitions (JSON Schema) and execution
app.rs     -- TUI state management and rendering
history.rs -- Session-based JSONL conversation persistence
```

## License

MIT
