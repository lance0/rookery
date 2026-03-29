#!/bin/bash
# Chaos test: kill server, verify canary restart AND agent port bounce
source "$(dirname "$0")/lib.sh"

echo "=== Chaos: Server + Agent Recovery ==="
require_running

AGENT=$(require_agent)
OLD_SERVER_PID=$(get_server_pid)
OLD_AGENT_PID=$(get_agent_pid "$AGENT")
info "server pid=$OLD_SERVER_PID, agent '$AGENT' pid=$OLD_AGENT_PID"

info "killing llama-server..."
kill -9 "$OLD_SERVER_PID"

info "waiting up to 90s for server canary restart..."
if wait_for "server restart" 90 5 bash -c "
    PID=\$(curl -sf $API/api/status | python3 -c \"import sys,json; print(json.load(sys.stdin).get('pid',''))\" 2>/dev/null)
    [ -n \"\$PID\" ] && [ \"\$PID\" != \"$OLD_SERVER_PID\" ] && [ \"\$PID\" != \"None\" ]
"; then
    NEW_SERVER_PID=$(get_server_pid)
    pass "server restarted (pid $OLD_SERVER_PID → $NEW_SERVER_PID)"
else
    fail "server not restarted within 90s"
fi

info "waiting up to 60s for agent port bounce..."
if wait_for "agent bounce" 60 5 bash -c "
    PID=\$(curl -sf $API/api/agents/$AGENT/health | python3 -c \"import sys,json; d=json.load(sys.stdin); print(d['pid'] if d.get('status')=='running' else '')\" 2>/dev/null)
    [ -n \"\$PID\" ] && [ \"\$PID\" != \"$OLD_AGENT_PID\" ]
"; then
    NEW_AGENT_PID=$(get_agent_pid "$AGENT")
    pass "agent bounced (pid $OLD_AGENT_PID → $NEW_AGENT_PID)"
else
    fail "agent not bounced within 60s"
fi

REASON=$(api_json "/api/agents/$AGENT/health" "print(d.get('last_restart_reason',''))")
[ "$REASON" = "port_recovery" ] && pass "reason=port_recovery" || info "reason=$REASON (may be 'crash' if agent died before port bounce)"

echo "=== Server + Agent Recovery: PASSED ==="
