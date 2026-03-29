#!/bin/bash
# Chaos test: sleep/wake cycle
source "$(dirname "$0")/lib.sh"

echo "=== Chaos: Sleep/Wake ==="
require_running

PROFILE=$(get_server_profile)
info "current profile: $PROFILE"

# Sleep
$ROOKERY sleep > /dev/null 2>&1 || fail "sleep command failed"
sleep 2
STATE=$(get_server_state)
[ "$STATE" = "sleeping" ] && pass "server sleeping" || fail "expected sleeping, got $STATE"

SLEEP_PROFILE=$(get_server_profile)
[ "$SLEEP_PROFILE" = "$PROFILE" ] && pass "profile preserved while sleeping" || fail "profile changed: $PROFILE → $SLEEP_PROFILE"

# Wake
$ROOKERY wake > /dev/null 2>&1 || fail "wake command failed"
sleep 10
STATE=$(get_server_state)
[ "$STATE" = "running" ] && pass "server running after wake" || fail "expected running, got $STATE"

WAKE_PROFILE=$(get_server_profile)
[ "$WAKE_PROFILE" = "$PROFILE" ] && pass "profile restored: $WAKE_PROFILE" || fail "wrong profile after wake: $WAKE_PROFILE (expected $PROFILE)"

# Check agent survived
AGENT=$(get_first_agent 2>/dev/null || true)
if [ -n "$AGENT" ]; then
    STATUS=$(get_agent_status "$AGENT")
    [ "$STATUS" = "running" ] && pass "agent '$AGENT' survived sleep/wake" || info "agent '$AGENT' status: $STATUS"
fi

echo "=== Sleep/Wake: PASSED ==="
