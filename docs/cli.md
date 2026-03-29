# CLI Reference

The `rookery` CLI communicates with the `rookeryd` daemon over HTTP.

## Global Options

```
--daemon <url>    Daemon address (default: http://127.0.0.1:3000)
--json            Output as JSON (supported on all commands)
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
rookery completions <shell>   # generate shell completions (bash, zsh, fish)
```

## JSON Output

All commands support `--json` for machine-readable output:

```bash
rookery status --json | jq '.state'
rookery agent status --json | jq '.agents[0].uptime_secs'
rookery gpu --json | jq '.gpus[0].vram_used_mb'
```
