#!/usr/bin/env bash
# ------------------------------------------------------------------
# Manual signal-handling smoke test.
#
# Starts tsdl building several parsers with --force, sends SIGTERM
# mid-build, and checks that:
#   1. "Received SIGTERM" appears in the log
#   2. "SHUTDOWN killing pgid=" appears (child killed mid-execution)
#   3. "Command cancelled (shutdown)" appears
#   4. "pipeline shutdown signalled" appears
#   5. Exit code is 0 (graceful)
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
  if grep -q "tree-sitter-macos-arm64" "$log_file" 2>/dev/null; then
    echo "  build underway"
    break
  fi
  sleep 0.5
done

if ! grep -q "tree-sitter-macos-arm64" "$log_file" 2>/dev/null; then
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

wait "$pid" 2>/dev/null
exit_code=$?

echo ""
echo "=== exit code: $exit_code ==="
echo ""

# ------------------------------------------------------------------
# Report
# ------------------------------------------------------------------
if [[ -f "$log_file" ]]; then
  echo "--- log assertions ---"
  grep -q "Received SIGTERM" "$log_file" \
    && echo "  [PASS] signal receipt" \
    || echo "  [FAIL] signal receipt — log: $log_file"
  grep -q "SHUTDOWN killing pgid=" "$log_file" \
    && echo "  [PASS] child killed mid-exec" \
    || echo "  [FAIL] child killed mid-exec — log: $log_file"
  grep -q "Command cancelled (shutdown)" "$log_file" \
    && echo "  [PASS] cancellation error" \
    || echo "  [FAIL] cancellation error — log: $log_file"
  grep -q "pipeline shutdown signalled" "$log_file" \
    && echo "  [PASS] pipeline suppression" \
    || echo "  [FAIL] pipeline suppression — log: $log_file"
  if grep -q "panicked" "$log_file"; then
    echo "  [FAIL] panic found in log — log: $log_file"
  else
    echo "  [PASS] no panics"
  fi
else
  echo "  [FAIL] log file not created: $log_file"
fi

if (( exit_code == 0 )); then
  echo "  [PASS] exit code 0 (graceful)"
else
  echo "  [FAIL] exit code $exit_code (expected 0)"
fi
