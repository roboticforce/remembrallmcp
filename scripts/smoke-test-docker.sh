#!/usr/bin/env bash
# Smoke test for the Docker Compose setup path - the same flow new users follow
# in the README quick start.
#
# Catches the class of regression from issue #6:
#   - `remembrall init` is one-shot, so the container used to exit immediately
#     and `docker compose exec remembrall remembrall status` failed with
#     "service is not running".
#   - init's human-readable banner used to go to stdout, which would corrupt
#     the MCP JSON-RPC stream when an MCP client launched the container.
#
# What this verifies:
#   1. `docker compose up -d` brings the stack up and the remembrall container
#      stays running (does not exit after init).
#   2. `docker compose exec remembrall remembrall status` reports a connected
#      database - the exact verification command from the README.
#   3. An MCP `initialize` request sent over stdio to
#      `docker compose run --rm -T remembrall` returns a JSON-RPC response on
#      stdout, and stdout contains no init banner text (stdout stays clean for
#      MCP JSON-RPC).
#
# Usage: scripts/smoke-test-docker.sh [working-dir]
# Requires Docker and the docker compose plugin.

set -euo pipefail

# Move to the repo root (or the directory passed as $1).
if [ $# -ge 1 ]; then
  cd "$1"
else
  cd "$(dirname "$0")/.."
fi

# Compose v2 is invoked as `docker compose`. Allow an override for CI.
COMPOSE="${DOCKER_COMPOSE:-docker compose}"

# Protocol version the server advertises (rmcp LATEST).
PROTOCOL_VERSION="2025-06-18"

# Portable timeout: use GNU `timeout` if present (Linux/CI), `gtimeout` if
# Homebrew coreutils is installed, otherwise run without a timeout. The MCP
# stdio test terminates on its own when stdin closes, so the timeout is only a
# safety net.
run_with_timeout() {
  local secs="$1"; shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$secs" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$secs" "$@"
  else
    "$@"
  fi
}

# Tear down containers but keep named volumes (db_data, remembrall_home) so the
# embedding model cache persists across local re-runs. Removing the volume every
# run forces an 86MB HuggingFace download each time.
cleanup() {
  echo "[smoke] tearing down..."
  $COMPOSE down --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "[smoke] building and starting the stack..."
$COMPOSE up -d --build >/dev/null

# Wait for the remembrall container to be running, then for init to finish.
# Readiness signal: `remembrall status` reports "Database: connected". This
# both confirms the container is up AND that init created the schema and saved
# the config (the config file is written at the very end of init, after the
# embedding model download).
echo "[smoke] waiting for remembrall to be ready (init + schema + model)..."
ready=0
# 600s headroom: on a slow network the first-run embedding model download
# (~86 MB from HuggingFace) can take several minutes. The cache persists in
# the remembrall_home volume, so subsequent runs skip the download.
#
# Capture `status` output into a variable before grepping. With `pipefail`,
# piping a multi-line producer straight into `grep -q` is unreliable: grep
# exits 0 on the first match and closes the pipe, the producer then hits
# SIGPIPE (exit 141), and pipefail makes the whole pipeline non-zero - so the
# readiness check would never succeed even when the database is connected.
for _ in $(seq 1 600); do
  status_out="$($COMPOSE exec -T remembrall remembrall status 2>/dev/null || true)"
  if printf '%s\n' "$status_out" | grep -q "Database: connected"; then
    ready=1
    break
  fi
  sleep 1
done

if [ "$ready" -ne 1 ]; then
  echo "[smoke] FAIL: remembrall did not become ready"
  echo "--- remembrall logs (last 30 lines, escapes stripped) ---"
  $COMPOSE logs remembrall 2>&1 | tr -d '\r' | sed 's/\x1b\[[0-9;]*[mK]//g' | tail -30 || true
  echo "--- compose ps ---"
  $COMPOSE ps || true
  echo "--- exec status (diagnostic) ---"
  $COMPOSE exec -T remembrall remembrall status 2>&1 || true
  exit 1
fi
echo "[smoke] remembrall is ready (database connected)"

# Assertion 1: the remembrall container must still be running. If init were the
# only command, the container would have exited here.
state="$($COMPOSE ps remembrall --format '{{.State}}' 2>/dev/null | tr -d '[:space:]')"
if [ "$state" != "running" ]; then
  echo "[smoke] FAIL: remembrall container state is '$state', expected 'running'"
  exit 1
fi
echo "[smoke] remembrall container is still running (did not exit after init)"

# Assertion 2: the README verification command reports a connected database.
status="$($COMPOSE exec -T remembrall remembrall status)"
echo "$status" | sed 's/^/  /'
if ! echo "$status" | grep -q "Database: connected"; then
  echo "[smoke] FAIL: status did not report 'Database: connected'"
  exit 1
fi
echo "[smoke] status verification passed"

# Assertion 3: MCP initialize over stdio returns a JSON-RPC response, and
# stdout is clean of init's banner text.
echo "[smoke] sending MCP initialize over stdio..."
INIT=$(printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"%s","capabilities":{},"clientInfo":{"name":"smoke","version":"1.0"}}}' "$PROTOCOL_VERSION")

# `run --rm -T` starts a fresh container: entrypoint runs init (output to
# stderr, discarded) then `remembrall serve` over stdio. The server reads the
# initialize request, writes the response, and exits when stdin closes (EOF).
out="$(printf '%s\n' "$INIT" | run_with_timeout 120 $COMPOSE run --rm -T remembrall 2>/dev/null || true)"

if ! printf '%s' "$out" | grep -q '"jsonrpc":"2.0"'; then
  echo "[smoke] FAIL: no JSON-RPC response on stdout"
  echo "--- stdout (first 20 lines) ---"
  printf '%s\n' "$out" | sed -n '1,20p' | sed 's/^/  /'
  exit 1
fi

if printf '%s' "$out" | grep -q "Setting up RemembrallMCP"; then
  echo "[smoke] FAIL: init banner leaked into stdout (would corrupt MCP stream)"
  echo "--- stdout (first 20 lines) ---"
  printf '%s\n' "$out" | sed -n '1,20p' | sed 's/^/  /'
  exit 1
fi
echo "[smoke] MCP initialize returned a JSON-RPC response; stdout is clean"

echo "[smoke] PASS - Docker setup path works"