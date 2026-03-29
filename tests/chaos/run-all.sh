#!/bin/bash
# Run all chaos tests sequentially
set -euo pipefail

DIR="$(dirname "$0")"
PASSED=0
FAILED=0

run_test() {
    echo ""
    if bash "$DIR/$1"; then
        PASSED=$((PASSED + 1))
    else
        FAILED=$((FAILED + 1))
        echo "  ^^^ FAILED ^^^"
    fi
    # Brief pause between tests for system to stabilize
    sleep 5
}

echo "Rookery Chaos Test Suite"
echo "========================"

run_test "kill-agent.sh"
run_test "kill-server.sh"
run_test "rapid-swap.sh"
run_test "sleep-wake.sh"
run_test "server-agent.sh"

echo ""
echo "========================"
echo "Results: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ] || exit 1
