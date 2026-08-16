#!/usr/bin/env bash
# Install Zeron's private local edge outside TCC-protected project folders and
# register it as a per-user launchd service.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DATA_DIR="${ZERON_DATA_DIR:-$HOME/.zeron}"
APP_DIR="$DATA_DIR/private-edge-app"
BIN_DIR="$DATA_DIR/bin"
PLIST="$HOME/Library/LaunchAgents/com.0xc0ffe.private-edge.plist"
DOMAIN="gui/$(id -u)"
LABEL="com.0xc0ffe.private-edge"

if [[ ! -x "$ROOT/edge/node_modules/.bin/wrangler" ]]; then
  echo "Installing private edge dependencies…"
  npm ci --prefix "$ROOT/edge"
fi

mkdir -p "$APP_DIR" "$BIN_DIR" "$HOME/Library/LaunchAgents"
ditto "$ROOT/edge" "$APP_DIR"
install -m 0755 "$ROOT/scripts/private-edge-supervisor.sh" "$BIN_DIR/private-edge-supervisor.sh"

# Render user-specific absolute paths at install time. launchd does not expand
# $HOME or shell expressions inside plist values.
escaped_home="$(printf '%s' "$HOME" | sed 's/[&|]/\\&/g')"
escaped_data="$(printf '%s' "$DATA_DIR" | sed 's/[&|]/\\&/g')"
sed \
  -e "s|__ZERON_HOME__|$escaped_home|g" \
  -e "s|__ZERON_DATA_DIR__|$escaped_data|g" \
  "$ROOT/scripts/macos/com.0xc0ffe.private-edge.plist" >"$PLIST"
chmod 0644 "$PLIST"

launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
for _ in {1..20}; do
  launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1 || break
  sleep 0.25
done

# launchd can briefly return EIO while a just-removed job is being reaped.
for attempt in {1..5}; do
  if launchctl bootstrap "$DOMAIN" "$PLIST"; then
    break
  fi
  if (( attempt == 5 )); then
    exit 1
  fi
  sleep 1
done

for _ in {1..60}; do
  if curl -fsS --max-time 2 http://127.0.0.1:27640/health >/dev/null 2>&1; then
    echo "Private edge is healthy on 127.0.0.1:27640"
    exit 0
  fi
  sleep 1
done

echo "Private edge did not become healthy; inspect ~/Library/Logs/0xc0ffe-private-edge.log" >&2
exit 1
