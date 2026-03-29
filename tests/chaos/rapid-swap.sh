#!/bin/bash
# Chaos test: rapid profile swaps
source "$(dirname "$0")/lib.sh"

echo "=== Chaos: Rapid Swap ==="
require_running

CURRENT=$(get_server_profile)
# Pick two profiles to swap between
PROFILES=$(api_json "/api/profiles" "
profiles = [p['name'] for p in d['profiles']]
print(' '.join(profiles[:2]))
")
P1=$(echo "$PROFILES" | awk '{print $1}')
P2=$(echo "$PROFILES" | awk '{print $2}')

[ -n "$P1" ] && [ -n "$P2" ] || fail "need at least 2 profiles (found: $PROFILES)"
info "swapping between '$P1' and '$P2'"

for i in 1 2 3 4; do
    TARGET=$( [ $((i % 2)) -eq 1 ] && echo "$P1" || echo "$P2" )
    $ROOKERY swap "$TARGET" > /dev/null 2>&1 || fail "swap $i to '$TARGET' failed"
    STATE=$(get_server_state)
    [ "$STATE" = "running" ] || fail "not running after swap $i (state=$STATE)"
    pass "swap $i → $TARGET"
done

FINAL=$(get_server_profile)
info "final profile: $FINAL"
pass "all 4 swaps completed cleanly"

# Restore original profile if different
if [ "$FINAL" != "$CURRENT" ]; then
    $ROOKERY swap "$CURRENT" > /dev/null 2>&1
    info "restored original profile '$CURRENT'"
fi

echo "=== Rapid Swap: PASSED ==="
