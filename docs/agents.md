# Agent Management

Rookery manages external processes (agents) alongside the inference server. Agents are long-running processes like Hermes (Telegram bot), coding assistants, or any service that depends on the inference API.

## Configuration

```toml
[agents.my_agent]
command = "/path/to/agent"
args = ["run"]
auto_start = true                    # start when daemon starts
restart_on_swap = true               # restart when model is hot-swapped
restart_on_crash = true              # watchdog auto-restarts on crash
depends_on_port = 8081               # bounce when this port recovers (server restart)
version_file = "/path/to/pyproject.toml"  # read version from project file
update_command = "/path/to/agent update"  # run for updates
update_workdir = "/path/to/agent/repo"    # optional working directory for updates
restart_on_error_patterns = [        # immediate restart on these stderr patterns
    "ConnectionError",
    "ReadTimeout"
]
```

## Agent Lifecycle

### Start/Stop

```bash
rookery agent start hermes    # start agent
rookery agent stop hermes     # stop agent (intentional, won't trigger watchdog)
rookery agent update hermes   # stop, update, restart
rookery agent status          # list agents with status
rookery agent status --json   # machine-readable
```

### Auto-Start

When `auto_start = true`, the agent starts automatically when the daemon starts. This is the recommended setting for production agents.

### Health Endpoint

```bash
curl http://localhost:3131/api/agents/hermes/health
```

Returns:
```json
{
    "name": "hermes",
    "pid": 12345,
    "started_at": "2026-03-27T12:00:00Z",
    "status": "running",
    "version": "0.4.0",
    "uptime_secs": 3600,
    "total_restarts": 2,
    "last_restart_reason": "port_recovery",
    "error_count": 0,
    "lifetime_errors": 3
}
```

### Update Flow

`rookery agent update <name>` and `POST /api/agents/{name}/update` run the configured `update_command` under rookery control:

1. stop the agent if it is running
2. run the update command with `[agent:<name>:update]` log prefix
3. restart the agent
4. report the resulting version if `version_file` is configured

If the update command exits non-zero, rookery attempts to restart the previous agent code and returns a failure response instead of leaving the agent down.

> **Note**: The update command is executed via `sh -c` and its full text is logged to journald. Avoid embedding secrets or tokens directly in `update_command` — use environment variables or credential files instead.

## Reliability Features

### Watchdog (restart_on_crash)

Polls every 30 seconds. If an agent's process dies unexpectedly (not from `rookery agent stop`), it restarts automatically with exponential backoff:

- 1s, 2s, 4s, 8s, 16s, 32s, 60s (cap)
- Backoff resets after 5 minutes of healthy uptime
- `intentional_stop` flag prevents watchdog from restarting manually stopped agents

### Dependency Port Health (depends_on_port)

Tracks whether the inference server port is alive. When the port transitions from **down → up** (server restarted), the watchdog bounces the agent to get a fresh connection. This handles:

- llama-server crashes and restarts
- Model hot-swaps (server stops and starts on new port)
- Daemon restarts where the server was already running

A 60-second uptime guard prevents double-bouncing when the swap handler already restarted the agent.

### Error Pattern Restart (restart_on_error_patterns)

Monitors agent stderr for fatal patterns. When a pattern matches (case-insensitive), the watchdog triggers an **immediate** restart instead of waiting for the next 30s poll cycle.

```toml
restart_on_error_patterns = [
    "telegram.error.TimedOut",    # Telegram API timeout
    "ReadTimeout",                # HTTP client timeout
    "deleteWebhook",              # Telegram webhook cleanup failure
]
```

#### Patterns for Common Messaging Platforms

| Platform | Patterns |
|----------|----------|
| Telegram | `telegram.error.TimedOut`, `ReadTimeout`, `deleteWebhook` |
| Discord | `discord.errors.GatewayNotConnected`, `websocket.close`, `HeartbeatTimeout` |
| Slack | `slack_sdk.errors.SlackApiError`, `invalid_auth`, `token_revoked` |
| Signal | `SignalProtocolError`, `WebSocketClosedError` |
| WhatsApp | `ConnectionClosed`, `StreamEndedError` |
| Matrix | `MatrixRequestError`, `M_UNKNOWN_TOKEN` |

These are examples — check your specific agent framework's error messages and add the relevant substrings.

### Swap Restart (restart_on_swap)

When a model is hot-swapped, agents with `restart_on_swap = true` are automatically restarted with a 2-second delay to ensure the old process exits cleanly. If start fails, retries once after 3 seconds.

For agents using `--replace` (like Hermes), the daemon uses `remove_tracking` instead of `stop` to avoid racing with the agent's own process management.

### Daemon Restart Bounce

When the daemon restarts and finds adopted agents from a previous session, it bounces them for fresh connections to the inference server. This prevents stale CLOSE_WAIT sockets.

### Inference Canary

A background task sends a minimal completion request to the inference server every 60 seconds. If the server fails to respond (CUDA zombie state), it auto-restarts. This is separate from agent management but keeps the server healthy for agents to use.

## Observability

### Metrics

Each agent tracks:
- **error_count** — stderr lines containing "error" (resets on restart)
- **lifetime_errors** — accumulated errors across all restarts
- **total_restarts** — number of times the agent has been restarted
- **last_restart_reason** — "crash", "swap", "port_recovery", "daemon_restart", "error_pattern"

### Dashboard

The Agents panel on the Overview tab shows:
- Green/gray dot for running/stopped
- Version badge (from `version_file`)
- Uptime (e.g., "2h 34m")
- Restart count (yellow if > 0)
- Error count (red if > 0)
- Start/Stop button
- Update button

### Logs

Agent stdout/stderr is captured with `[agent:name]` prefix in the log buffer:
```bash
rookery logs | grep hermes
rookery logs -f  # follow mode
```

## Adding a New Agent

1. Add to `~/.config/rookery/config.toml`:
```toml
[agents.myagent]
command = "/usr/local/bin/myagent"
args = ["--port", "8081"]
auto_start = false
restart_on_crash = true
depends_on_port = 8081
restart_on_error_patterns = ["ConnectionRefused", "FatalError"]
```

2. Start it: `rookery agent start myagent`
3. Check health: `curl http://localhost:3131/api/agents/myagent/health`
