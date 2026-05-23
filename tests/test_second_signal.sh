#!/bin/sh
# ------------------------------------------------------------------
# Second-signal escalation smoke test.
#
# Verifies that when tsdl receives two signals in succession:
#   1. First SIGTERM → graceful shutdown: cancel() + cooperative exit
#   2. Second SIGTERM → escalation: libc::kill(-pgid, SIGKILL) + exit(143)
#
# Design:
#   TSDL_TEST_DELAY_MS=500 guarantees every spawned child has a 500ms
#   window mid-execution.  The first SIGTERM triggers graceful shutdown
#   but the delay cascade keeps the process alive for 1-2 seconds.
#   We send the second signal 500ms later — reliably during the graceful
#   shutdown window — forcing the escalation path.
#
#   process::exit() kills the process with code 143 and flushes stderr,
#   but may not flush the tracing log file.  We capture stderr for the
#   escalation message and use the tracing log file for the first-signal
#   assertions.
#
# Assertions:
#   1. Exit code is 143 (SIGTERM escalation, not 0 = graceful)
#   2. stderr contains "Received SIGTERM" (first signal)
#   3. stderr contains "Received second SIGTERM; forcing exit" (escalation)
#   4. File log contains "SHUTDOWN killing pgid=" (optional, best-effort)
#
# Run:  sh tests/test_second_signal.sh
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
# Start tsdl.  Capture stderr to a separate file because process::exit
# flushes C stdio but not the tracing non-blocking writer.
# The tracing log file (--log) is also written for first-signal assertions.
# ------------------------------------------------------------------
echo "=== starting tsdl (will receive 2 signals) ==="
LOG="$ROOT/tmp/test/test_2sig.log"
STDERR="$ROOT/tmp/test/test_2sig.stderr"
TSDL_TEST_DELAY_MS=500 \
./target/debug/tsdl -v b json rust toml php --force --progress plain \
  --log "$LOG" --log-color no \
  2>"$STDERR" &
PID=$!

# ------------------------------------------------------------------
# Wait for builds to be underway (spawned pid line in log).
# ------------------------------------------------------------------
echo "=== waiting for builds to start ==="
DEADLINE=$(($(date +%s) + 30))
while [ $(date +%s) -lt $DEADLINE ]; do
  if grep -q "spawned pid=Some(" "$LOG" 2>/dev/null; then
    echo "  build underway"
    break
  fi
  sleep 0.5
done

if ! grep -q "spawned pid=Some(" "$LOG" 2>/dev/null; then
  echo "  [FAIL] build never started"
  kill $PID 2>/dev/null || true
  exit 1
fi

# ------------------------------------------------------------------
# First SIGTERM: graceful shutdown begins.  The test_delay cascade
# keeps the process alive for 1-2 seconds.
# ------------------------------------------------------------------
echo "=== sending first SIGTERM ==="
kill -TERM $PID 2>/dev/null || true

# 500ms is shorter than any single test_delay, so the second signal
# will reliably arrive during the graceful shutdown cascade.
sleep 0.5

# ------------------------------------------------------------------
# Second SIGTERM: escalation path.  process::exit(143) kills the
# process immediately after SIGKILLing child process groups.
# ------------------------------------------------------------------
echo "=== sending second SIGTERM ==="
kill -TERM $PID 2>/dev/null || true

# Wait for the process to die (escalation is immediate).
sleep 2
if kill -0 $PID 2>/dev/null; then
  echo "=== still alive, sending SIGKILL ==="
  kill -KILL $PID
fi

# process::exit(143) means wait returns 143.  set +e so the script
# doesn't abort on the non-zero exit.
set +e
wait $PID 2>/dev/null
EXIT=$?
set -e

echo ""
echo "=== exit code: $EXIT ==="
echo ""

# ------------------------------------------------------------------
# Assertions
# ------------------------------------------------------------------
failures=0

echo "--- first signal (tracing log) ---"
if [ -f "$LOG" ]; then
  grep -q "Received SIGTERM" "$LOG" \
    && echo "  [PASS] first signal received" \
    || { echo "  [FAIL] first signal not in log — $LOG"; failures=$((failures+1)); }
  # Best-effort: child kill evidence (may not appear if escalation fires first)
  grep -q "SHUTDOWN killing pgid=" "$LOG" \
    && echo "  [PASS] child killed mid-exec" \
    || echo "  [WARN] no child kill in log (escalation may have preempted)"
else
  echo "  [FAIL] tracing log not created: $LOG"
  failures=$((failures+1))
fi

echo "--- second signal (stderr capture) ---"
if [ -f "$STDERR" ]; then
  grep -q "Received SIGTERM" "$STDERR" \
    && echo "  [PASS] first signal on stderr" \
    || { echo "  [WARN] first signal not on stderr (may race with exit)"; }
  grep -q "Received second SIGTERM" "$STDERR" \
    && echo "  [PASS] second signal escalation message" \
    || { echo "  [FAIL] escalation message not on stderr — $STDERR"; failures=$((failures+1)); }
else
  echo "  [FAIL] stderr capture not created: $STDERR"
  failures=$((failures+1))
fi

echo ""
if [ $EXIT -eq 143 ]; then
  echo "  [PASS] exit code 143 (SIGTERM escalation)"
else
  echo "  [FAIL] exit code $EXIT (expected 143)"
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
