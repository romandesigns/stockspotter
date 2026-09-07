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
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Deployment refused: checkout has local changes" >&2
  exit 1
fi
git merge --ff-only origin/master
AFTER="$(git rev-parse HEAD)"

LAST_DEPLOYED="$(cat "$STATE_FILE" 2>/dev/null || echo "")"
if [ "$AFTER" = "$LAST_DEPLOYED" ]; then
  exit 0
fi

echo "[$(date -Is)] deploying $BEFORE -> $AFTER"
docker compose -p stockspotter build
docker compose -p stockspotter run --rm --no-deps qualify python -c 'import os; assert len(os.environ.get("STOCKSPOTTER_API_TOKEN", "")) >= 32, "Configure STOCKSPOTTER_API_TOKEN before deployment"'
docker compose -p stockspotter up -d --wait --wait-timeout 180
docker compose -p stockspotter exec -T qualify python /app/check_health.py

echo "$AFTER" > "$STATE_FILE"
