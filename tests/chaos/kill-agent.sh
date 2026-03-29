#!/bin/bash
# Chaos test: kill agent, verify watchdog restarts it
source "$(dirname "$0")/lib.sh"

echo "=== Chaos: Kill Agent ==="
require_running

AGENT=$(require_agent)
OLD_PID=$(get_agent_pid "$AGENT")
info "killing agent '$AGENT' pid=$OLD_PID"
kill -9 "$OLD_PID"

info "waiting up to 60s for watchdog to restart agent..."
if wait_for "watchdog restart" 60 5 bash -c "
    NEW_PID=\$(curl -sf $API/api/agents/$AGENT/health | python3 -c \"import sys,json; d=json.load(sys.stdin); print(d['pid'] if d.get('status')=='running' else '')\" 2>/dev/null)
    [ -n \"\$NEW_PID\" ] && [ \"\$NEW_PID\" != \"$OLD_PID\" ]
"; then
    NEW_PID=$(get_agent_pid "$AGENT")
    pass "agent '$AGENT' restarted (pid $OLD_PID → $NEW_PID)"
else
    fail "agent '$AGENT' was not restarted by watchdog within 60s"
fi

# Verify restart reason
REASON=$(api_json "/api/agents/$AGENT/health" "print(d.get('last_restart_reason',''))")
[ "$REASON" = "crash" ] && pass "last_restart_reason=crash" || fail "expected reason=crash, got '$REASON'"

# Verify watchdog state
CRASHES=$(api_json "/api/agents/$AGENT/health" "print(d['watchdog']['consecutive_crashes'])")
[ "${CRASHES:-0}" -gt 0 ] && pass "consecutive_crashes=$CRASHES" || fail "crash count not incremented"

echo "=== Kill Agent: PASSED ==="
