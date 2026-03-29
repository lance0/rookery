#!/bin/bash
# Shared helpers for chaos tests

set -euo pipefail

ROOKERY="${ROOKERY:-rookery}"
API="${ROOKERY_API:-http://127.0.0.1:3131}"

pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; exit 1; }
info() { echo "  .... $1"; }

api_get() { curl -sf "$API$1" 2>/dev/null; }

api_json() { api_get "$1" | python3 -c "import sys,json; d=json.load(sys.stdin); $2"; }

wait_for() {
    local desc="$1" timeout="$2" interval="${3:-2}"
    shift 3
    local elapsed=0
    while [ "$elapsed" -lt "$timeout" ]; do
        if "$@" 2>/dev/null; then return 0; fi
        sleep "$interval"
        elapsed=$((elapsed + interval))
    done
    return 1
}

get_server_pid() { api_json "/api/status" "print(d.get('pid',''))"; }
get_server_state() { api_json "/api/status" "print(d['state'])"; }
get_server_profile() { api_json "/api/status" "print(d.get('profile',''))"; }

get_agent_pid() {
    local name="$1"
    api_json "/api/agents/$name/health" "print(d['pid'])" 2>/dev/null || echo ""
}

get_agent_status() {
    local name="$1"
    api_json "/api/agents/$name/health" "print(d['status'])" 2>/dev/null || echo "unknown"
}

get_first_agent() {
    api_json "/api/agents" "
agents = [a['name'] for a in d['agents'] if a.get('status') == 'running']
print(agents[0] if agents else '')
"
}

require_running() {
    local state
    state=$(get_server_state) || fail "daemon not reachable at $API"
    [ "$state" = "running" ] || fail "server not running (state=$state)"
}

require_agent() {
    local name
    name=$(get_first_agent)
    [ -n "$name" ] || fail "no running agents found"
    echo "$name"
}
