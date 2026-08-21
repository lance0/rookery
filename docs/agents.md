# Agent Management

Rookery manages external processes (agents) alongside the inference server. Agents are long-running processes like [Hermes](https://github.com/NousResearch/hermes-agent) (Nous Research's multi-platform AI agent), coding assistants, or any service that depends on the inference API.

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
stop_timeout_secs = 30               # SIGTERM grace before SIGKILL (default 30)
restart_on_error_patterns = [        # restart after 3 matches in 10 minutes
    "ConnectionError",
    "ReadTimeout"
]
data_dir = "/path/to/agent/data"     # SQLite root: nightly check + pre-change backups
```

`data_dir` is what turns on both database-safety features below. With neither it
nor `workdir` set, the nightly sweep and the pre-change backups have no root to
resolve and do nothing.

## Agent Lifecycle

### Start/Stop

```bash
rookery agent start hermes    # start agent
rookery agent stop hermes     # stop agent (intentional, won't trigger watchdog)
rookery agent update hermes   # stop, update, restart
rookery agent status          # list agents with status
rookery agent status --json   # machine-readable
```

`stop` and `update` take a [pre-change database backup](#pre-change-database-backups)
first when `data_dir` (or `workdir`) is set, which adds a few seconds. `stop` also
waits up to `stop_timeout_secs` (default 30) after `SIGTERM` before escalating to
`SIGKILL`; taking the SIGKILL path is logged at `error` and should be treated as
an incident, since hard-killing an agent mid-WAL-checkpoint is how torn pages
happen.

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

1. stop the agent if it is running, taking a pre-change database backup
2. if the agent was *already* stopped, take the backup anyway — the update
   migrates state in place regardless of whether the process is up
3. run the update command with `[agent:<name>:update]` log prefix
4. restart the agent
5. report the resulting version if `version_file` is configured

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

Monitors agent stderr for fatal patterns. When **three** lines match (case-insensitive) within a **ten-minute** window, the watchdog triggers an **immediate** restart instead of waiting for the next 30s poll cycle.

A single match is ignored on purpose — `ReadTimeout` and its relatives are transient network conditions, and restarting a process over one interrupts writes for no benefit. A wedged gateway re-emits its error every poll cycle, so it still trips the threshold in seconds; failures as slow as one every 5 minutes still restart. The counter is per process and resets once the agent restarts.

The trade-off to be aware of: **a pattern that a wedged agent prints exactly once no longer restarts it.** If your agent signals a terminal state with a single line, this path will not catch it — `restart_on_crash` covers an agent that actually exits, but one that wedges while still running does not. Below-threshold matches are logged (`error pattern matched, below restart threshold`) so you can see them accumulating.

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

### Database Integrity (data_dir)

Agents that keep state in SQLite can have it silently corrupted and carry on
serving for weeks, because the damage only surfaces when a write happens to
traverse a bad page. Set `data_dir` and the watchdog runs a read-only
`PRAGMA quick_check` over that agent's databases once a day, shortly after 04:00
local time.

```toml
data_dir = "/home/lance/.hermes"
```

The directory and its immediate subdirectories are scanned for `*.db`, which
picks up both `state.db` and a nested `cron/executions.db` while leaving deeper
backup trees alone. `-wal`/`-shm` sidecars are covered by checking the database
they belong to. If `data_dir` is unset it falls back to `workdir`; with neither
set the agent is not checked.

The check is deliberately conservative:

- **Read-only.** The database is opened `SQLITE_OPEN_READONLY`, in a `sqlite3`
  subprocess. It cannot write, and it cannot lock out the agent — WAL readers do
  not block the writer. Opening read-write would checkpoint the WAL into the main
  file and delete the sidecars out from under a live agent, which is exactly the
  write this must never perform.
- **Reports, never acts.** A corrupt database does *not* stop or restart the
  agent. Restarting is the worst possible response: the first thing a restart
  does is reopen and write to the damaged file. Findings go to `tracing` at
  `error` level and to the log buffer under the agent's own `[agent:name]`
  prefix, so they appear in `rookery logs` alongside a crash.
- **Degrades loudly.** If `sqlite3` is not installed, or a file cannot be read,
  that is reported as *unchecked* — never as healthy, and never as corrupt.

`quick_check` rather than a cheaper query, because **no ordinary query is an
integrity signal**. On a real corrupted `state.db`, `count(*)` returned 25,654
rows while `max(id)` failed outright — the count came out of an index without
ever touching the damaged leaf pages. The reverse happens just as easily. Only a
full page traversal is evidence. `quick_check` skips `integrity_check`'s
index-versus-table cross-checks and measures ~2s on a 392 MB database.

### Pre-change Database Backups

The integrity sweep detects damage. This is the other half: making sure there is
something to restore when it does. Before rookery takes an agent down for a
change that is about to mutate state the agent owns, it copies that agent's
databases with `VACUUM INTO`.

It runs on the same `data_dir` (falling back to `workdir`), so the same one
setting enables both features.

**When a backup is taken:**

| Flow | Backup? |
|---|---|
| `rookery agent update <name>` — running agent | yes |
| `rookery agent update <name>` — already-stopped agent | yes |
| Profile swap that bounces the agent (`restart_on_swap`) | yes |
| `rookery agent stop <name>` (manual) | **yes** |
| Watchdog crash restart / dependency-port bounce | no |
| Daemon shutdown (`stop_all`) | no |

A manual `rookery agent stop` taking a copy is worth knowing about: it adds a few
seconds to a command that used to be instant. It is deliberate — that is the path
`POST /api/agents/{name}/update` stops the agent through, and `hermes update`
applies config migrations in place. The two excluded flows are excluded for
concrete reasons: daemon shutdown would add minutes to `systemctl stop` for a
copy nothing is about to change, and an agent in a crash-restart loop would churn
out gigabytes and — worse — age the pre-update backup out of retention exactly
when it is needed.

**`VACUUM INTO`, read-only, not `cp`.** `cp` of a live database reads pages while
the writer mutates them and can capture a torn page — producing exactly the file
this exists to avoid creating. `VACUUM INTO` runs inside a read transaction, so
the copy is a consistent snapshot. The source is opened `-readonly` for the same
reason the integrity check is: a read-write open checkpoints the WAL on close and
**deletes** the `-wal`/`-shm` sidecars. Measured on a database with 2.8 MB of
uncheckpointed WAL, the read-write open removed both sidecars; the read-only open
left them byte-for-byte intact, and the copy still contained every row, because
the reader reads *through* the WAL.

**Layout and retention.** Copies land in `<data_dir>/db-backups/<UTC timestamp>/`,
one directory per generation, each file suffixed `.bak` (a nested
`cron/executions.db` is flattened to `cron_executions.db.bak` so it cannot
collide with a root-level file of the same name). **Three generations** are kept;
older ones are pruned on every run. The `.bak` suffix and the extra directory
level are not cosmetic — the integrity sweep walks only one level deep and
filters on `*.db`, so backups are excluded twice over, and neither a deeper walk
nor a change to the extension filter can silently rope them in and multiply the
nightly sweep's runtime by the retention count.

**Disk cost:** worst case is three times the total size of the agent's live
databases. For Hermes' ~400 MB that is ~1.2 GB.

**Fail-open.** A failed backup does not block the change. Failures are logged
loudly — `tracing` at `error` level, plus a `[agent:<name>] db backup FAILED`
line in the log buffer — and the update or stop proceeds. A missing `sqlite3`, a
full disk, or a read-only data directory must not wedge every update.

A partial copy is never left behind: `VACUUM INTO` creates its destination
immediately and fills it as it goes, so a killed copy, a corrupt source, or a
full disk all leave a plausible-looking `.bak` on disk. Each copy is therefore
read back and verified before it counts as a success, and discarded on any
failure. Both halves are needed: a 0-byte file passes `PRAGMA quick_check`
(SQLite reads it as a valid empty database), so verification alone would not
catch it.

## Observability

### Metrics

Each agent tracks:
- **error_count** — stderr lines containing "error" (resets on restart)
- **lifetime_errors** — accumulated errors across all restarts
- **total_restarts** — number of times the agent has been restarted
- **last_restart_reason** — "crash", "swap", "port_recovery", "daemon_restart", "error_pattern"

Exported per agent on `/metrics`:
- **rookery_agent_db_corrupt** — databases found corrupt by `quick_check`
- **rookery_agent_db_unchecked** — databases the check could not read
- **rookery_agent_db_last_check_timestamp** — when the sweep last completed.
  Alert on staleness as well as on failures: a check that stopped running looks
  exactly like a clean bill of health.

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
