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

# Serialize timer/manual deployments. Private release branches stay pinned until
# explicitly replaced; do not publish private work just to operate this VPS.
exec 9>.git/stockspotter-deploy.lock
flock -n 9 || exit 0

BEFORE="$(git rev-parse HEAD)"
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Deployment refused: checkout has local changes" >&2
  exit 1
fi
BRANCH="$(git branch --show-current)"
case "$BRANCH" in
  master) git fetch origin master; git merge --ff-only origin/master ;;
  release/*) ;; # Approved, locally installed source bundle; no public push needed.
  *) echo "Deployment refused: unsupported branch $BRANCH" >&2; exit 1 ;;
esac
AFTER="$(git rev-parse HEAD)"

LAST_DEPLOYED="$(cat "$STATE_FILE" 2>/dev/null || echo "")"
if [ "$AFTER" = "$LAST_DEPLOYED" ]; then
  exit 0
fi

echo "[$(date -Is)] deploying $BEFORE -> $AFTER"
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml build
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml run --rm --no-deps qualify python -c 'import os; assert len(os.environ.get("STOCKSPOTTER_API_TOKEN", "")) >= 32, "Configure STOCKSPOTTER_API_TOKEN before deployment"'
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml up -d --wait --wait-timeout 180

# Verify HTTP and browser entrypoint before recording a successful deployment.
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml exec -T qualify python /app/check_health.py
curl --fail --silent --show-error http://127.0.0.1:3000/ >/dev/null

echo "$AFTER" > "$STATE_FILE"
