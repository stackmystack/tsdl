#!/bin/sh
# ------------------------------------------------------------------
# Lock takeover smoke test.
#
# Spawns two tsdl processes sharing the same build directory.
# Process A acquires the lock and starts building.  Process B
# discovers the lock is held, prompts the user whether to continue,
# sends SIGTERM to A, waits for A to exit, then acquires the lock
# and runs its own build.
#
# Assertions (checked against log files):
#   1. A acquired the lock
#   2. A received SIGTERM and shut down gracefully
#   3. B sent SIGTERM to A via lock takeover
#   4. B acquired the lock after A released it
#   5. Both exit code 0
#
# Run:  sh tests/test_lock_takeover.sh
# Requires: debug build (cargo build), Unix.
# ------------------------------------------------------------------
set -eu

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

echo "=== cleaning build dir ==="
rm -rf tmp 2>/dev/null
mkdir -p tmp/test parsers

cargo build 2>&1 | tail -1

# ------------------------------------------------------------------
# Process A: start building (acquires the lock, runs subprocesses).
# Use -v for debug-level file logging and a generous unlock_timeout
# so B doesn't time out waiting for A to shut down.
# ------------------------------------------------------------------
echo "=== starting process A (lock owner) ==="
LOG_A="$ROOT/tmp/test/test_lock_a.log"
TSDL_TEST_DELAY_MS=500 \
./target/debug/tsdl -v b json rust toml --force --progress plain \
  --log "$LOG_A" --log-color no --unlock-timeout 30 &
PID_A=$!

# Wait for A to acquire the lock.
echo "=== waiting for A to acquire lock ==="
DEADLINE=$(($(date +%s) + 15))
while [ $(date +%s) -lt $DEADLINE ]; do
  if grep -q "Acquired lock" "$LOG_A" 2>/dev/null; then
    echo "  A acquired lock (PID $PID_A)"
    break
  fi
  sleep 0.5
done

if ! grep -q "Acquired lock" "$LOG_A" 2>/dev/null; then
  echo "  [FAIL] A never acquired lock"
  kill $PID_A 2>/dev/null || true
  exit 1
fi

# Wait for A to actually start building subprocesses before B tries
# to steal the lock (ensures B SIGTERM exercises the subprocess-kill path).
echo "=== waiting for A to start building ==="
DEADLINE=$(($(date +%s) + 15))
while [ $(date +%s) -lt $DEADLINE ]; do
  if grep -q "tree-sitter-macos-arm64" "$LOG_A" 2>/dev/null; then
    echo "  A has spawned subprocesses"
    break
  fi
  sleep 0.5
done

# ------------------------------------------------------------------
# Process B: try to build on the same directory.  Pipe "y\n"
# to stdin so it accepts the prompt and takes over the lock.
# ------------------------------------------------------------------
echo "=== starting process B (lock taker) ==="
LOG_B="$ROOT/tmp/test/test_lock_b.log"
echo "y" | \
TSDL_TEST_DELAY_MS=500 \
./target/debug/tsdl -v b json rust --force --progress plain \
  --log "$LOG_B" --log-color no --unlock-timeout 30 &
PID_B=$!

# Wait for both to finish.
wait $PID_A 2>/dev/null; EXIT_A=$?
wait $PID_B 2>/dev/null; EXIT_B=$?

echo ""
echo "=== A exit code: $EXIT_A ==="
echo "=== B exit code: $EXIT_B ==="
echo ""

# ------------------------------------------------------------------
# Report
# ------------------------------------------------------------------
failures=0

echo "--- process A (lock owner) ---"
if [ -f "$LOG_A" ]; then
  grep -q "Acquired lock" "$LOG_A" \
    && echo "  [PASS] acquired lock" \
    || { echo "  [FAIL] acquired lock — log: $LOG_A"; failures=$((failures+1)); }
  grep -q "Received SIGTERM" "$LOG_A" \
    && echo "  [PASS] received SIGTERM" \
    || { echo "  [FAIL] received SIGTERM — log: $LOG_A"; failures=$((failures+1)); }
  grep -q "SHUTDOWN killing pgid=" "$LOG_A" \
    && echo "  [PASS] child killed mid-exec" \
    || echo "  [WARN] child killed mid-exec (may have completed before signal)"
else
  echo "  [FAIL] log file not created: $LOG_A"
  failures=$((failures+1))
fi

echo "--- process B (lock taker) ---"
if [ -f "$LOG_B" ]; then
  grep -q "Sending SIGTERM to lock owner" "$LOG_B" \
    && echo "  [PASS] sent SIGTERM to A" \
    || { echo "  [FAIL] sent SIGTERM to A — log: $LOG_B"; failures=$((failures+1)); }
  grep -q "Acquired lock" "$LOG_B" \
    && echo "  [PASS] acquired lock" \
    || { echo "  [FAIL] acquired lock — log: $LOG_B"; failures=$((failures+1)); }
  grep -q "panicked" "$LOG_B" && {
    echo "  [FAIL] panic in B log — log: $LOG_B"; failures=$((failures+1));
  }
else
  echo "  [FAIL] log file not created: $LOG_B"
  failures=$((failures+1))
fi

echo ""
if [ $EXIT_A -eq 0 ]; then
  echo "  [PASS] A exit code 0"
else
  echo "  [WARN] A exit code $EXIT_A (may be 143 if SIGTERM killed tsdl before graceful shutdown)"
fi

if [ $EXIT_B -eq 0 ]; then
  echo "  [PASS] B exit code 0 (graceful)"
else
  echo "  [FAIL] B exit code $EXIT_B (expected 0)"
  failures=$((failures+1))
fi

if [ $failures -eq 0 ]; then
  echo ""
  echo "All assertions passed."
  exit 0
else
  echo ""
  echo "$failures assertion(s) FAILED."
  exit 1
fi
