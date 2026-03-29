# Testing

## Unit Tests

```bash
cargo test --workspace    # 337+ tests, no GPU required
```

All tests use mock backends and temp directories — they never touch your real config or running daemon.

## Chaos Tests

Chaos tests verify reliability features under failure conditions. They require a running daemon with a loaded model and a configured agent.

### Prerequisites

- `rookeryd` running via systemd with `auto_start = true`
- At least one agent configured with `restart_on_crash = true` and `depends_on_port`
- An inference server running (any profile)

### Running

```bash
# Run all chaos tests
./tests/chaos/run-all.sh

# Or run individually
./tests/chaos/kill-server.sh      # Kill llama-server, verify canary restarts it
./tests/chaos/kill-agent.sh       # Kill agent, verify watchdog restarts it
./tests/chaos/rapid-swap.sh       # 4 rapid profile swaps
./tests/chaos/sleep-wake.sh       # Sleep/wake cycle
./tests/chaos/server-agent.sh     # Kill server, verify agent bounces on port recovery
```

### What They Test

**Kill Server** (`kill-server.sh`)
- Sends SIGKILL to llama-server
- Waits for the inference canary to detect the failure (60s poll interval)
- Verifies the canary restarts the server with a new PID
- Checks `rookery_canary_restarts_total` incremented in Prometheus metrics

**Kill Agent** (`kill-agent.sh`)
- Sends SIGKILL to the first running agent
- Waits for the watchdog to detect the crash (30s poll interval)
- Verifies the watchdog restarts the agent with exponential backoff
- Checks `last_restart_reason = "crash"` in the health API
- Verifies `consecutive_crashes > 0` in watchdog state

**Rapid Swap** (`rapid-swap.sh`)
- Swaps between two profiles 4 times in quick succession
- Verifies each swap completes cleanly
- Checks the final profile is correct
- Verifies agents restarted on each swap (if `restart_on_swap = true`)

**Sleep/Wake** (`sleep-wake.sh`)
- Puts the server to sleep via CLI
- Verifies state transitions to `sleeping`
- Wakes the server
- Verifies state returns to `running` with the same profile
- Checks agents survive the cycle

**Server + Agent Recovery** (`server-agent.sh`)
- Kills llama-server (the dependency port)
- Waits for canary to restart the server
- Waits for watchdog to detect port recovery (down→up transition)
- Verifies the agent was bounced with `last_restart_reason = "port_recovery"`
- Confirms both server and agent are healthy at the end

### Failure Modes Covered

| Scenario | Detection | Recovery | Timing |
|----------|-----------|----------|--------|
| llama-server SIGKILL | Inference canary (60s) | Auto-restart | ~60-70s |
| Agent SIGKILL | Watchdog poll (30s) | Restart with backoff | ~31-35s |
| CUDA error in stderr | Watch channel (immediate) | Canary triggered | ~2-5s |
| Dependency port down→up | Watchdog port check (30s) | Agent bounce | ~30-60s |
| Rapid swap stress | Op lock serialization | Sequential swap | Immediate |
| Sleep/wake cycle | State machine | Profile restore | ~7-10s |
