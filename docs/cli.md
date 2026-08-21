# CLI Reference

The `rookery` CLI communicates with the `rookeryd` daemon over HTTP.

When `api_key` is set in `~/.config/rookery/config.toml`, the CLI automatically attaches it to daemon requests. `rookery auth generate` prints a strong suggested key for the config file.

## Global Options

```
--daemon <url>    Daemon address (default: http://127.0.0.1:3000)
--json            Output as JSON (supported on most commands)
```

## Commands

### Server

```bash
rookery start [profile]       # start server (default profile if omitted)
rookery stop                  # stop server
rookery sleep                 # unload the model but remember the active profile
rookery wake                  # wake the sleeping profile
rookery swap <profile>        # hot-swap to a different profile
rookery status                # show server state, PID, uptime
rookery bench                 # run PP + gen speed benchmark
rookery profiles              # list available profiles
```

`rookery status` reports `sleeping` when auto-sleep or manual sleep has unloaded the backend. The next chat request also wakes the server automatically.

### GPU

```bash
rookery gpu                   # VRAM, temp, utilization, power, processes
```

### Agents

```bash
rookery agent start <name>    # start an agent
rookery agent stop <name>     # stop an agent
rookery agent update <name>   # stop, update, restart
rookery agent status          # list agents with status
rookery agent describe <name> # detailed health, uptime, restarts, errors
```

### Models

```bash
rookery models search <query>     # search HuggingFace for GGUF repos
rookery models quants <repo>      # list quants with sizes and VRAM fit
rookery models recommend <repo>   # VRAM-aware best quant recommendation
rookery models list               # locally cached models
rookery models pull <repo> [--quant Q4_K_M]  # download a model
rookery models hardware           # show hardware profile
```

### Releases

```bash
rookery releases              # upstream release status (llama.cpp)
rookery releases --json       # JSON output for scripting
```

### Logs

```bash
rookery logs                  # last 50 log lines
rookery logs -n 100           # last 100 lines
rookery logs -f               # follow mode (stream via SSE)
```

### Config

```bash
rookery config                # validate config, show resolved commands
```

### Other

```bash
rookery auth generate        # print a strong rky-... API key
rookery completions <shell>   # generate shell completions (bash, zsh, fish)
```

## JSON Output

Most commands support `--json` for machine-readable output:

```bash
rookery status --json | jq '.state'
rookery agent status --json | jq '.agents[0].uptime_secs'
rookery gpu --json | jq '.gpus[0].vram_used_mb'
```

### Errors in `--json` mode

Every `--json` error path prints a JSON object to **stdout** and exits non-zero, so a pipe into `jq` never sees empty input:

```json
{
  "error": "rookeryd is not running at http://127.0.0.1:3000 (start it with `rookeryd`)",
  "daemon_url": "http://127.0.0.1:3000"
}
```

- **`.error`** is a string on every JSON error body, whatever the command produced it. Test for it with `jq -e .error`.
- **`.daemon_url`** is the daemon address the CLI resolved — from `--daemon`, else `listen` in the config file, else the default. This is what tells a stopped daemon apart from a config pointing at the wrong address.
- `status --json` adds `"state": "daemon_offline"` to that same object rather than using a shape of its own.
- `config --json` reports an invalid config file as `{"valid": false, "error": "..."}` — same `.error` key.

When the *daemon* is the one reporting the failure, stdout is the daemon's own response body printed verbatim, and the exit code comes from its `success` field:

```console
$ rookery start fast --json; echo "exit=$?"
{
  "success": false,
  "message": "profile 'fast' not found"
}
exit=1
```

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | The command did what it reported. |
| `1` | Runtime failure — the daemon was unreachable, or the daemon reported the operation failed. |
| `2` | Usage error — unknown subcommand, missing or invalid argument. Emitted by the argument parser before anything runs. |

`--json` follows exactly the same exit codes as plain output.

**Daemon-reported failures exit `1`.** The daemon returns HTTP 200 with `success: false` for a genuine failure, so the body carries the verdict rather than the status line. These commands read it: `start`, `swap`, `sleep`, `wake`, `agent start`, `agent stop`, `agent update`, `models pull`.

```bash
rookery start && rookery agent start hermes   # the agent is not started against a dead server
rookery wake  && rookery bench                # the bench does not run against a server that never woke
```

**Repeating an operation that already holds is a success, not a failure.** `rookery sleep` against an already-sleeping server, `rookery wake` against an already-running one, and `rookery stop` against an already-stopped one all exit `0` — the requested end state is the actual state, so defensive shutdown and boot scripts can call them unconditionally.

What exits `1` is being in a state the operation cannot reach that end state *from*: `sleep` on a stopped or failed server reports `server is not running`, and `wake` on a server that is not sleeping reports `server is not sleeping`. Neither is a no-op — a stopped server that you `sleep` cannot then be woken — so both are real failures.

**An unreachable daemon exits `1`** on every command that contacts one — which is all of them except `config`, `auth generate` and `completions`, which work purely locally. `config` exits `1` when the config file is missing or fails validation.

`1` deliberately covers both "the daemon is down" and "the daemon says it failed". A script needing to tell the two apart should read the error body, which distinguishes them in more detail than an integer can: `2` is already taken by usage errors, and "I typed the command wrong" is the distinction actually worth an exit code.
