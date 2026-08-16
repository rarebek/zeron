#!/usr/bin/env bash
# Keep the private local edge healthy. launchd/systemd owns this supervisor;
# the supervisor owns Wrangler and exits when Wrangler dies or stops serving.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EDGE_DIR="${ZERON_PRIVATE_EDGE_DIR:-$ROOT/edge}"
PORT="${ZERON_PRIVATE_EDGE_PORT:-27640}"
STATE_DIR="${ZERON_PRIVATE_EDGE_STATE_DIR:-$HOME/.zeron/private-edge-state}"
HEALTH_URL="http://127.0.0.1:${PORT}/health"
WRANGLER="$EDGE_DIR/node_modules/.bin/wrangler"

if [[ ! -x "$WRANGLER" ]]; then
  echo "private edge dependencies are missing; run: npm ci --prefix '$EDGE_DIR'" >&2
  exit 78
fi

# launchd has a deliberately small PATH. Resolve the newest mise Node runtime
# without relying on shell initialization, then let Wrangler's env shebang use it.
if ! command -v node >/dev/null 2>&1; then
  node_bin="$(find "$HOME/.local/share/mise/installs/node" -mindepth 3 -maxdepth 3 -type f -path '*/bin/node' 2>/dev/null | sort -V | tail -n 1)"
  if [[ -z "$node_bin" ]]; then
    echo "private edge requires Node.js" >&2
    exit 78
  fi
  export PATH="$(dirname "$node_bin"):$PATH"
fi

mkdir -p "$STATE_DIR"
cd "$EDGE_DIR"

"$WRANGLER" dev \
  --ip 127.0.0.1 \
  --port "$PORT" \
  --var AUTH_MODE:dev \
  --persist-to "$STATE_DIR" &
child=$!

cleanup() {
  kill "$child" 2>/dev/null || true
  wait "$child" 2>/dev/null || true
}
trap cleanup EXIT INT TERM HUP

startup_deadline=$(( $(date +%s) + 60 ))
healthy=0
failures=0

while kill -0 "$child" 2>/dev/null; do
  if curl -fsS --max-time 3 "$HEALTH_URL" >/dev/null 2>&1; then
    healthy=1
    failures=0
  elif (( healthy )); then
    failures=$((failures + 1))
    if (( failures >= 3 )); then
      echo "private edge failed three health checks; restarting" >&2
      exit 1
    fi
  elif (( $(date +%s) >= startup_deadline )); then
    echo "private edge did not become healthy within 60 seconds; restarting" >&2
    exit 1
  fi
  sleep 5
done

wait "$child"
status=$?
echo "private edge exited with status $status" >&2
exit 1
