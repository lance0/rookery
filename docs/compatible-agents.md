# Compatible Agents & Tools

Rookery can manage any long-running process as an agent — start, stop, restart on crash, bounce on model swap, and update in place. This page lists tools that work well with Rookery's agent management.

## What Makes a Good Rookery Agent?

A tool is a good fit for Rookery agent management if it:

1. **Connects to a local OpenAI-compatible API** (`http://localhost:8081/v1`)
2. **Runs as a long-lived background process** (server, gateway, or daemon)
3. **Benefits from auto-restart** — crash recovery, model swap awareness

Interactive terminal tools (Aider, OpenCode, Goose) should be configured to point at your local endpoint directly — they don't need Rookery to manage them.

## Managed Agents (Daemon/Server Mode)

### Hermes (Nous Research)

Open-source AI agent framework with tool calling, web browsing, vision, and multi-platform messaging (Telegram, Discord, and more). The primary agent Rookery was built to manage.

- **URL**: https://github.com/NousResearch/hermes-agent
- **Install**: `pip install hermes-agent`
- **Local API**: Connects to any OpenAI-compatible endpoint

```toml
[agents.hermes]
command = "/path/to/hermes"
args = ["gateway", "run", "--replace"]
auto_start = true
restart_on_swap = true
restart_on_crash = true
depends_on_port = 8081
version_file = "/path/to/hermes-agent/pyproject.toml"
update_command = "hermes update"
restart_on_error_patterns = ["ConnectionError", "ReadTimeout"]
```

### OpenClaw

Personal AI assistant platform — multi-platform messaging, tool calling, and extensible skills. Runs as a persistent gateway process.

- **URL**: https://github.com/openclaw/openclaw
- **Install**: `npm install -g openclaw@latest`
- **Local API**: Configure a custom provider with `baseUrl: "http://localhost:8081/v1"`

```toml
[agents.openclaw]
command = "openclaw"
args = ["gateway", "run"]
auto_start = false
restart_on_swap = true
restart_on_crash = true
depends_on_port = 8081
```

### SillyTavern

Feature-rich chat frontend for roleplay, character cards, group chats, and creative writing. Runs as a web server you access from a browser.

- **URL**: https://github.com/SillyTavern/SillyTavern
- **Install**: `git clone https://github.com/SillyTavern/SillyTavern && cd SillyTavern && npm install`
- **Local API**: In the UI, select "Chat Completions" and set endpoint to `http://localhost:8081/v1`

```toml
[agents.sillytavern]
command = "node"
args = ["server.js"]
workdir = "/path/to/SillyTavern"
auto_start = false
restart_on_crash = true
depends_on_port = 8081
```

### AnythingLLM

All-in-one AI app with RAG, document chat, workspaces, and built-in vector database. Adds document ingestion and retrieval that Rookery's chat doesn't have.

- **URL**: https://github.com/Mintplex-Labs/anything-llm
- **Install**: Docker or desktop app
- **Local API**: "Generic OpenAI" provider — set base URL to `http://localhost:8081/v1`

```toml
[agents.anythingllm]
command = "docker"
args = ["run", "--rm", "-p", "3001:3001", "-v", "/path/to/storage:/app/server/storage", "mintplexlabs/anythingllm"]
auto_start = false
restart_on_swap = true
restart_on_crash = true
depends_on_port = 8081
```

### LibreChat

ChatGPT-like web UI with conversation branching, presets, plugins, and multi-user auth. More polished than SillyTavern for general-purpose productivity chat.

- **URL**: https://github.com/danny-avila/LibreChat
- **Install**: Docker Compose (includes MongoDB)
- **Local API**: Add as a custom OpenAI-compatible provider

```toml
[agents.librechat]
command = "docker"
args = ["compose", "up"]
workdir = "/path/to/LibreChat"
auto_start = false
restart_on_crash = true
depends_on_port = 8081
```

### Open WebUI

Self-hosted ChatGPT-style web interface. The most popular local LLM frontend — supports any OpenAI-compatible API, conversation history, RAG, model selection, and multi-user auth.

- **URL**: https://github.com/open-webui/open-webui
- **Install**: `docker run -d -p 3000:8080 ghcr.io/open-webui/open-webui:main`
- **Local API**: Set `OPENAI_API_BASE_URL=http://host.docker.internal:8081/v1` in Docker env

```toml
[agents.openwebui]
command = "docker"
args = ["run", "--rm", "-p", "3000:8080", "-e", "OPENAI_API_BASE_URL=http://host.docker.internal:8081/v1", "-v", "/path/to/data:/app/backend/data", "ghcr.io/open-webui/open-webui:main"]
auto_start = false
restart_on_crash = true
depends_on_port = 8081
```

### Open Interpreter (Server Mode)

Natural language interface for executing code on your machine. Server mode exposes an API that other tools can call.

- **URL**: https://github.com/openinterpreter/open-interpreter
- **Install**: `pip install open-interpreter`
- **Local API**: Set `OPENAI_API_BASE=http://localhost:8081/v1`

```toml
[agents.interpreter]
command = "interpreter"
args = ["--server", "--port", "8000"]
env = { OPENAI_API_BASE = "http://localhost:8081/v1", OPENAI_API_KEY = "none" }
auto_start = false
restart_on_swap = true
restart_on_crash = true
depends_on_port = 8081
```

## Interactive Tools (Configure Directly)

These tools are interactive terminal sessions. Configure them to point at `localhost:8081/v1` directly — they don't need Rookery to manage their lifecycle.

| Tool | Config | Notes |
|------|--------|-------|
| [Aider](https://aider.chat) | `aider --model openai/model --openai-api-base http://localhost:8081/v1` | AI pair programming |
| [OpenCode](https://opencode.ai) | `opencode.json` with `baseURL: "http://localhost:8081/v1"` | Terminal coding agent |
| [Goose](https://github.com/block/goose) | `OPENAI_HOST=http://localhost:8081` in config | Block's coding agent |
| [Cline](https://github.com/cline/cline) | VS Code extension, set provider to OpenAI-compatible with local URL | Autonomous coding agent |
| [Continue.dev](https://continue.dev) | VS Code extension, set provider to `openai` with local base URL | IDE assistant |

## Writing Your Own Agent Config

Any process that talks to an OpenAI-compatible API can be a Rookery agent:

```toml
[agents.my_custom_agent]
command = "/path/to/my-agent"
args = ["--api-url", "http://localhost:8081/v1"]
workdir = "/path/to/agent/dir"           # optional working directory
auto_start = true                         # start on daemon boot
restart_on_swap = true                    # restart when model changes
restart_on_crash = true                   # watchdog auto-restarts
depends_on_port = 8081                    # bounce when server restarts
version_file = "/path/to/pyproject.toml"  # optional version tracking
update_command = "git pull && pip install -e ."  # optional update script
restart_on_error_patterns = ["ConnectionError"]  # optional error-triggered restart
```

Key config fields:
- `restart_on_swap` — restart the agent when you hot-swap models (clears stale connections)
- `depends_on_port` — detect when the inference server restarts and bounce the agent
- `restart_on_error_patterns` — watch stderr for fatal patterns and restart immediately
- `restart_on_crash` — exponential backoff restart (1s → 60s cap, resets after 5min healthy)
