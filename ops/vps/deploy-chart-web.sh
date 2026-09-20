#!/usr/bin/env bash
# Scoped chart release: run from a clean, pushed release worktree on the VPS.
# Deliberately leaves the research checkout and all non-web containers pinned.
set -euo pipefail
SOURCE="$(git rev-parse --show-toplevel)"
PRODUCTION=/opt/apps/stockspotter
BASE=7e365866ce5ae18ea77cce05d6d8f45bc69c7faa
REVISION="$(git rev-parse HEAD)"
BRANCH="$(git branch --show-current)"
test "$BRANCH" = release/chart-web-20260920
test "$SOURCE" != "$PRODUCTION"
test -z "$(git status --porcelain)"
git fetch --quiet origin "$BRANCH"
test "$REVISION" = "$(git rev-parse FETCH_HEAD)"
git merge-base --is-ancestor "$BASE" "$REVISION"
# Refuse any change outside the reviewed web-only release surface.
while IFS= read -r file; do
  case "$file" in
    apps/client/src/lib/seriesWriter.ts|apps/client/src/lib/seriesWriter.test.ts|apps/client/src/lib/superChartEngine.ts|apps/client/src/components/SuperChart.tsx|apps/client/src/components/ReplayChart.tsx|docs/chart-web-release-2026-09-20.md|ops/vps/deploy-chart-web.sh) ;;
    *) echo "Unexpected release change: $file" >&2; exit 1 ;;
  esac
done < <(git diff --name-only "$BASE" "$REVISION")

cd "$PRODUCTION"
exec 9>.git/stockspotter-deploy.lock
flock -w 30 9
test "$(git rev-parse HEAD)" = "$BASE"
test "$(cat ops/vps/.deployed-commit)" = "$BASE"
test -z "$(git status --porcelain)"
EVIDENCE="/opt/apps/stockspotter-web-releases/evidence-$REVISION"
mkdir "$EVIDENCE"
compose() { docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml "$@"; }
protected_state() {
  git rev-parse HEAD
  sha256sum .env ops/vps/.deployed-commit ops/vps/deploy.sh ops/qualify/session.sh OI-V2-PREREGISTRATION-2026-09-19.md
  docker inspect --format '{{.Name}} {{.Id}} {{.Image}} {{.State.StartedAt}}' \
    stockspotter-vps-ws-1 stockspotter-vps-auto-trader-1 \
    stockspotter-vps-qualify-1 stockspotter-vps-discovery-review-1
}
protected_state > "$EVIDENCE/protected-before.txt"
OLD_IMAGE="$(docker inspect --format '{{.Image}}' stockspotter-vps-web-1)"
docker tag "$OLD_IMAGE" "stockspotter-chart-web:rollback-$REVISION"
printf '%s\n' "$OLD_IMAGE" > "$EVIDENCE/rollback-image.txt"
docker build --label "org.opencontainers.image.revision=$REVISION" \
  -f "$SOURCE/apps/client/Dockerfile" -t "stockspotter-chart-web:$REVISION" "$SOURCE"

rollback() {
  local status=$?
  trap - ERR
  echo "Web verification failed; restoring $OLD_IMAGE" >&2
  docker tag "$OLD_IMAGE" stockspotter-vps-web:latest
  compose up -d --no-deps --no-build --wait --wait-timeout 60 web
  exit "$status"
}
trap rollback ERR
docker tag "stockspotter-chart-web:$REVISION" stockspotter-vps-web:latest
compose up -d --no-deps --no-build --wait --wait-timeout 60 web
compose exec -T qualify python /app/check_health.py
curl --fail --silent --show-error https://stockspotter.wavystyle.io/ > "$EVIDENCE/served-index.html"
docker exec stockspotter-vps-web-1 cat /app/dist/index.html > "$EVIDENCE/image-index.html"
cmp "$EVIDENCE/served-index.html" "$EVIDENCE/image-index.html"
for asset in $(grep -oE '/assets/[^" ]+\.(js|css)' "$EVIDENCE/served-index.html"); do
  expected="$(docker exec stockspotter-vps-web-1 sha256sum "/app/dist$asset" | cut -d ' ' -f 1)"
  actual="$(curl --fail --silent --show-error "https://stockspotter.wavystyle.io$asset" | sha256sum | cut -d ' ' -f 1)"
  test "$expected" = "$actual"
  printf '%s %s\n' "$actual" "$asset" >> "$EVIDENCE/served-assets.sha256"
done
test "$(docker inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' stockspotter-vps-web-1)" = "$REVISION"
protected_state > "$EVIDENCE/protected-after.txt"
cmp "$EVIDENCE/protected-before.txt" "$EVIDENCE/protected-after.txt"
test -z "$(git status --porcelain)"
printf '%s\n' "$REVISION" > "$EVIDENCE/web-deployed-commit"
trap - ERR
echo "Web deployed and verified: $REVISION"
echo "Protected backend/configuration unchanged. Evidence and rollback image: $EVIDENCE"
