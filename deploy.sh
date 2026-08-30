#!/usr/bin/env bash
# Pull-based GitOps deploy, run every 2 minutes by stockspotter-deploy.timer
# on the Pi (see ops/pi/). Mirrors the wavystack fleet's deploy.sh pattern
# but scoped to this one repo/stack, fully decoupled from wavystack's own
# timer. Only rebuilds when the pulled commit actually changed.
set -euo pipefail
cd "$(dirname "$0")"

STATE_FILE=".deployed-commit"

git fetch origin master
BEFORE="$(git rev-parse HEAD)"
git reset --hard origin/master
AFTER="$(git rev-parse HEAD)"

LAST_DEPLOYED="$(cat "$STATE_FILE" 2>/dev/null || echo "")"
if [ "$AFTER" = "$LAST_DEPLOYED" ]; then
  exit 0
fi

echo "[$(date -Is)] deploying $BEFORE -> $AFTER"
docker compose -p stockspotter up -d --build

echo "$AFTER" > "$STATE_FILE"
