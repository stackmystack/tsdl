#!/usr/bin/env bash
# ------------------------------------------------------------------
# Manual signal-handling smoke test.
#
# Starts tsdl building several parsers with --force, sends SIGTERM
# mid-build, and checks that:
#   1. "Received SIGTERM" appears in the log
#   2. "SHUTDOWN sending SIGTERM" appears (signal forwarded to child)
#   3. "Command interrupted by SIGTERM" appears
#   4. "pipeline shutdown signalled" appears
#   5. Exit code is 143 (128 + SIGTERM)
#
# Polls the log for a spawned subprocess (rather than a fixed sleep)
# so the test is not sensitive to network speed or CLI download time.
#
# Run:  bash tests/test_signal.sh
# Requires: debug build (cargo build), Unix.
# ------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")/.."
root="$(pwd)"

echo "=== cleaning build dir ==="
rm -rf tmp 2>/dev/null
mkdir -p tmp/test parsers

cargo build 2>&1 | tail -1

# ------------------------------------------------------------------
# 4 parsers with --force, TSDL_TEST_DELAY_MS=500 gives each spawned
# child a 500ms window where it is definitely running.  We poll the
# log for a spawned line, then send SIGTERM immediately.
# ------------------------------------------------------------------
echo "=== starting tsdl (4 parsers, SIGTERM when builds underway) ==="
log_file="$root/tmp/test/test_signal.log"
TSDL_TEST_DELAY_MS=500 \
./target/debug/tsdl -v b json rust toml php --force --progress plain \
  --log "$log_file" --log-color no &
pid=$!

# Poll until a subprocess has been spawned.
echo "=== waiting for builds to start ==="
deadline=$(( $(date +%s) + 60 ))
while (( $(date +%s) < deadline )); do
  if grep -q "spawned pid=Some(" "$log_file" 2>/dev/null; then
    echo "  build underway"
    break
  fi
  sleep 0.5
done

if ! grep -q "spawned pid=Some(" "$log_file" 2>/dev/null; then
  echo "  [FAIL] build never started"
  kill "$pid" 2>/dev/null || true
  exit 1
fi

echo ""
echo "=== sending SIGTERM to $pid ==="
kill -TERM "$pid"

sleep 5

if kill -0 "$pid" 2>/dev/null; then
  echo "=== sending SIGKILL ==="
  kill -KILL "$pid"
fi

set +e
wait "$pid" 2>/dev/null
exit_code=$?
set -e

echo ""
echo "=== exit code: $exit_code ==="
echo ""

# ------------------------------------------------------------------
# Report
# ------------------------------------------------------------------
failures=0

if [[ -f "$log_file" ]]; then
  echo "--- log assertions ---"
  if grep -q "Received SIGTERM" "$log_file"; then
    echo "  [PASS] signal receipt"
  else
    echo "  [FAIL] signal receipt — log: $log_file"
    failures=$((failures + 1))
  fi

  if grep -q "SHUTDOWN sending SIGTERM" "$log_file"; then
    echo "  [PASS] signal forwarded to child"
  else
    echo "  [FAIL] signal forwarded to child — log: $log_file"
    failures=$((failures + 1))
  fi

  if grep -q "Command interrupted by SIGTERM" "$log_file"; then
    echo "  [PASS] interruption logged"
  else
    echo "  [FAIL] interruption logged — log: $log_file"
    failures=$((failures + 1))
  fi

  if grep -q "pipeline shutdown signalled" "$log_file"; then
    echo "  [PASS] pipeline suppression"
  else
    echo "  [FAIL] pipeline suppression — log: $log_file"
    failures=$((failures + 1))
  fi

  if grep -q "panicked" "$log_file"; then
    echo "  [FAIL] panic found in log — log: $log_file"
    failures=$((failures + 1))
  else
    echo "  [PASS] no panics"
  fi
else
  echo "  [FAIL] log file not created: $log_file"
  failures=$((failures + 1))
fi

if (( exit_code == 143 )); then
  echo "  [PASS] exit code 143 (SIGTERM)"
else
  echo "  [FAIL] exit code $exit_code (expected 143)"
  failures=$((failures + 1))
fi

if (( failures == 0 )); then
  echo ""
  echo "All assertions passed."
  exit 0
else
  echo ""
  echo "$failures assertion(s) FAILED."
  exit 1
fi
