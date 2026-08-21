# CLI Reference

The `rookery` CLI communicates with the `rookeryd` daemon over HTTP.

When `api_key` is set in `~/.config/rookery/config.toml`, the CLI automatically attaches it to daemon requests. `rookery auth generate` prints a strong suggested key for the config file.

## Global Options

```
--daemon <url>    Daemon address (default: http://127.0.0.1:3000)
--json            Output as JSON (supported on most commands)
```

The daemon address is resolved in this order: `--daemon`, else `listen` from `~/.config/rookery/config.toml`, else `http://127.0.0.1:3000`. A `listen` bound to an unspecified address (`0.0.0.0`) is dialled as `127.0.0.1` on the same port.

A config file that exists but fails to parse does not fall back silently — the CLI prints a warning naming the file and the address it settled on, then carries on:

```console
$ rookery status
warning: /home/you/.config/rookery/config.toml: expected `=` — using http://127.0.0.1:3000
```

**An unreachable daemon fails in about 2 seconds.** The CLI sets a 2s connect timeout, so a wrong address or a stopped daemon returns promptly instead of sitting in the kernel's SYN retry loop for ~130s. There is deliberately no *total* request timeout — `start` waits on a health check for up to 120s, `bench` up to 60s, and `stop` up to `stop_timeout_secs`, none of which should be cut off mid-flight. The bound on a daemon that is listening but wedged is the same 2s timeout on the health probe that gates every command.

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
rookery reload                # re-read config.toml without restarting the daemon
```

`rookery config` is purely local — it reads and validates the file without contacting the daemon, so it works before the daemon is up.

`rookery reload` (`POST /api/reload`) makes the running daemon re-read `config.toml`. The file is read, parsed and validated into a candidate config *before* anything is swapped in, so **a bad config can never take down a running daemon** — every failure path returns before the live config is written, and the daemon keeps serving exactly what it already had. This is the deliberate inverse of boot behaviour, where an invalid config is a hard exit.

```console
$ rookery reload
config reloaded from /home/you/.config/rookery/config.toml
the running server and all agents were left untouched
```

Reload changes what *future* operations see. It never bounces anything:

| | |
|---|---|
| **Applied immediately** | `api_key`, `idle_timeout`, `default_profile`; `profiles`, `models` and the `llama_server` binary path take effect on the next `start` or `swap`. |
| **Left untouched** | The running backend keeps its profile, port, PID and binary. No agent is started, stopped or bounced. |
| **Needs a daemon restart** | `listen`, `agents`, `auto_start`, `release_check_interval`. |

Anything the reload cannot honour comes back as a warning rather than being silently dropped — a changed `listen`, edited `[agents]`, or a live profile whose port changed or that no longer exists in the file. In plain mode these print to stderr as `warning: ...` while the reload itself still succeeds; `--json` returns them in a `warnings` array alongside `applied_now`, `unchanged` and `needs_daemon_restart`.

Removing the running profile from the config is legal: the backend is owned by the daemon, not by the config entry, so it keeps running and `stop`/`sleep` still work — but `start`/`swap` back to it will fail.

A reload that collides with an in-flight `start`/`stop`/`swap` waits up to 5s for the operation lock and then returns HTTP 409 rather than pinning the connection for the length of a swap. The config is unchanged; retry when the operation finishes.

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

**Daemon-reported failures exit `1`.** The daemon returns HTTP 200 with `success: false` for a genuine failure, so the body carries the verdict rather than the status line. These commands read `success`: `start`, `swap`, `sleep`, `wake`, `agent start`, `agent stop`, `agent update`.

`models pull` exits `1` on the same principle but reads a different field — its body reports `started`, since the command only kicks off a background download and returns.

`stop` is the exception: it does not read the body's verdict at all and exits `0` whenever the daemon was reachable. Do not use its exit code to confirm the server actually stopped — check `rookery status`.

```bash
rookery start && rookery agent start hermes   # the agent is not started against a dead server
rookery wake  && rookery bench                # the bench does not run against a server that never woke
```

**Repeating an operation that already holds is a success, not a failure.** The daemon answers `success: true` when the requested end state is already the actual state, so boot and shutdown scripts can call these unconditionally:

| command | already-satisfied case | daemon message | exit |
|---|---|---|---|
| `rookery sleep` | server is sleeping | `server already sleeping` | `0` |
| `rookery wake` | server is running | `already running with profile '<name>'` | `0` |
| `rookery start <p>` | `<p>` is already running | `already running with profile '<p>'` | `0` |

What exits `1` is being in a state the operation cannot reach that end state *from*:

| command | unreachable-from case | daemon message | exit |
|---|---|---|---|
| `rookery sleep` | server stopped or failed | `server is not running` | `1` |
| `rookery wake` | server not sleeping | `server is not sleeping` | `1` |

Neither is a no-op dressed up as an error — a stopped server that you `sleep` cannot then be woken — so both are real failures.

**An unreachable daemon exits `1`** on every command that contacts one — which is all of them except `config`, `auth generate` and `completions`, which work purely locally. `config` exits `1` when the config file is missing or fails validation.

`1` deliberately covers both "the daemon is down" and "the daemon says it failed". A script needing to tell the two apart should read the error body, which distinguishes them in more detail than an integer can: `2` is already taken by usage errors, and "I typed the command wrong" is the distinction actually worth an exit code.
