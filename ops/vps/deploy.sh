#!/usr/bin/env bash
# Pull-based GitOps deploy for the VPS (srv1170872) -- same real pattern
# as the Pi's own deploy.sh (repo root), just pointed at
# ops/vps/docker-compose.yml instead of the root one (that one's
# caddy-docker-proxy labels don't apply here -- this VPS runs native
# Caddy, see ops/vps/README.md). Only rebuilds when the pulled commit
# actually changed. Lives in ops/vps/ but operates on the repo root
# (git state is repo-root-level, not per-subdirectory).
set -euo pipefail
cd "$(dirname "$0")/../.."

STATE_FILE="ops/vps/.deployed-commit"

git fetch origin master
BEFORE="$(git rev-parse HEAD)"
git reset --hard origin/master
AFTER="$(git rev-parse HEAD)"

LAST_DEPLOYED="$(cat "$STATE_FILE" 2>/dev/null || echo "")"
if [ "$AFTER" = "$LAST_DEPLOYED" ]; then
  exit 0
fi

echo "[$(date -Is)] deploying $BEFORE -> $AFTER"
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml up -d --build

echo "$AFTER" > "$STATE_FILE"
