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
REMOTE_SHA=""
if [ -n "$(git status --porcelain --untracked-files=all)" ]; then
  echo "Deployment refused: checkout has local changes" >&2
  exit 1
fi
BRANCH="$(git branch --show-current)"
case "$BRANCH" in
  master)
    git fetch --quiet origin master
    REMOTE_SHA="$(git rev-parse FETCH_HEAD)"
    git merge --ff-only FETCH_HEAD
    ;;
  release/*)
    if ! git fetch --quiet origin "$BRANCH" 2>/dev/null; then
      echo "Deployment refused: $BRANCH has no counterpart on origin (push it first)" >&2
      exit 1
    fi
    REMOTE_SHA="$(git rev-parse FETCH_HEAD)"
    if [ "$(git rev-parse HEAD)" != "$REMOTE_SHA" ]; then
      echo "Deployment refused: checkout is not exactly at origin/$BRANCH tip" >&2
      echo "  local HEAD:      $(git rev-parse HEAD)" >&2
      echo "  origin/$BRANCH: $REMOTE_SHA" >&2
      exit 1
    fi
    ;;
  *) echo "Deployment refused: unsupported branch $BRANCH" >&2; exit 1 ;;
esac
AFTER="$(git rev-parse HEAD)"
if [ "$AFTER" != "$REMOTE_SHA" ]; then
  echo "Deployment refused: HEAD does not equal the freshly fetched deployable branch tip" >&2
  echo "  local HEAD:      $AFTER" >&2
  echo "  origin/$BRANCH: $REMOTE_SHA" >&2
  exit 1
fi

LAST_DEPLOYED="$(cat "$STATE_FILE" 2>/dev/null || echo "")"
if [ "$AFTER" = "$LAST_DEPLOYED" ]; then
  exit 0
fi

# The public check-runs endpoint is read-only and needs no VPS credential.
# This fails closed until both server jobs have completed successfully on the
# exact commit that will be built. Mobile remains a separate mobile-release gate.
if ! command -v python3 >/dev/null 2>&1; then
  echo "Deployment refused: python3 is required to verify exact-commit CI" >&2
  exit 1
fi
python3 ops/vps/deploy_guard.py --commit "$AFTER" --branch "$BRANCH"

# Recheck the build input after the network wait. Do not build from a checkout
# that moved or became dirty while the exact-SHA check was running.
if [ "$(git rev-parse HEAD)" != "$AFTER" ] || [ -n "$(git status --porcelain --untracked-files=all)" ]; then
  echo "Deployment refused: checkout changed after exact-commit validation" >&2
  exit 1
fi

echo "[$(date -Is)] deploying $BEFORE -> $AFTER"

# Build-time commit identity (assignment section 30).
#
# Compiled into the binary so `/research/completeness` can state which commit
# produced it. Without it a captured session has no provable provenance and the
# qualification pipeline returns INDETERMINATE -- refusing the very session it
# exists to evaluate.
#
# Asserted here rather than in the Dockerfile because absence is legitimate for
# a local or CI build and illegitimate only for a *deployment*. This is the one
# place that distinction is known.
export STOCKSPOTTER_COMMIT="$AFTER"
case "$STOCKSPOTTER_COMMIT" in
  [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) ;;
  *) echo "Deployment refused: STOCKSPOTTER_COMMIT is not a full object id ($STOCKSPOTTER_COMMIT)" >&2; exit 1 ;;
esac

docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml build
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml run --rm --no-deps qualify python -c 'import os; assert len(os.environ.get("STOCKSPOTTER_API_TOKEN", "")) >= 32, "Configure STOCKSPOTTER_API_TOKEN before deployment"'
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml up -d --wait --wait-timeout 180

# Verify HTTP and browser entrypoint before recording a successful deployment.
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml exec -T qualify python /app/check_health.py
curl --fail --silent --show-error http://127.0.0.1:3000/ >/dev/null

echo "$AFTER" > "$STATE_FILE"
