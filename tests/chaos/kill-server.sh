#!/bin/bash
# Chaos test: kill llama-server, verify canary restarts it
source "$(dirname "$0")/lib.sh"

echo "=== Chaos: Kill Server ==="
require_running

OLD_PID=$(get_server_pid)
info "killing llama-server pid=$OLD_PID"
kill -9 "$OLD_PID"

info "waiting up to 90s for canary to restart server..."
if wait_for "canary restart" 90 5 bash -c "
    NEW_PID=\$(curl -sf $API/api/status | python3 -c \"import sys,json; print(json.load(sys.stdin).get('pid',''))\" 2>/dev/null)
    [ -n \"\$NEW_PID\" ] && [ \"\$NEW_PID\" != \"$OLD_PID\" ] && [ \"\$NEW_PID\" != \"None\" ]
"; then
    NEW_PID=$(get_server_pid)
    pass "server restarted (pid $OLD_PID → $NEW_PID)"
else
    fail "server was not restarted by canary within 90s"
fi

# Verify canary metrics
RESTARTS=$(api_get "/metrics" | grep "^rookery_canary_restarts_total" | awk '{print $2}')
[ "${RESTARTS:-0}" -gt 0 ] && pass "canary_restarts_total=$RESTARTS" || fail "canary restart metric not incremented"

echo "=== Kill Server: PASSED ==="
