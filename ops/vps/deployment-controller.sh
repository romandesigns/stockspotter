#!/usr/bin/env bash
# Fail-closed wrapper for the deployment controller.
#
# This is NOT an entry point for the copy of the controller that lives in the
# checkout being deployed. It runs the controller only from an immutable
# installation outside the production checkout, and only when every installed
# file still matches the digest recorded when it was installed. The reason is
# the trust boundary ops/vps/README.md already documents: a gate whose
# implementation ships inside the candidate commit is not an independent gate.
#
# There is deliberately NO fallback. If the installation is absent, incomplete
# or modified, this refuses and exits; it never runs the checkout's own copies,
# and it never calls deploy.sh. An unprovisioned host does not deploy.
#
# Install (as root, once -- see docs/MASTER-DEPLOYMENT-ADAPTER-2026-10-14.md):
#
#   install -d -m 0755 -o root -g root /opt/stockspotter-controller
#   install -m 0444 -o root -g root \
#       ops/vps/deployment_host.py ops/vps/deployment_transaction.py \
#       ops/vps/deploy_guard.py ops/release_preflight.py \
#       ops/vps/staging_rehearsal.py ops/vps/staging_broker.py \
#       /opt/stockspotter-controller/
#   install -m 0555 -o root -g root ops/vps/deployment-controller.sh \
#       /opt/stockspotter-controller/
#   ( cd /opt/stockspotter-controller && sha256sum deployment_host.py \
#       deployment_transaction.py deploy_guard.py release_preflight.py \
#       deployment-controller.sh staging_rehearsal.py staging_broker.py \
#       > controller-manifest.sha256 )
#   chmod 0444 /opt/stockspotter-controller/controller-manifest.sha256
#
# Updating the controller is therefore an explicit, root-owned act with a new
# manifest -- not something a deployment can do to itself.
#
# IMPORTANT, before any promotion: stop and disable the legacy two-minute
# timer. `deploy.sh` rebuilds whenever HEAD differs from
# ops/vps/.deployed-commit, from the BASE compose file, which would discard the
# live effective configuration this controller preserves. The controller
# refuses to promote while that timer is enabled or active, and it never writes
# ops/vps/.deployed-commit:
#
#   sudo systemctl disable --now stockspotter-deploy.timer
set -euo pipefail

INSTALL="${STOCKSPOTTER_CONTROLLER:-/opt/stockspotter-controller}"
PRODUCTION="${STOCKSPOTTER_PRODUCTION:-/opt/apps/stockspotter}"
MANIFEST="$INSTALL/controller-manifest.sha256"
# Every file the controller needs, each of which must also be listed in the
# manifest. The manifest itself cannot record its own digest, so it is checked
# for existence only -- it is root-owned and read-only.
COVERED=(deployment_host.py deployment_transaction.py deploy_guard.py
         release_preflight.py deployment-controller.sh)
# The staging rehearsal is not needed to deploy, so a controller installed
# without it still works. But if it IS installed, it must be covered by the
# manifest too: an uncovered file in the installation directory is a file
# nobody is verifying.
OPTIONAL=(staging_rehearsal.py staging_broker.py staging_tls.py)

refuse() {
  echo "deployment-controller refused: $1" >&2
  exit 78            # EX_CONFIG: not provisioned, not a transient failure
}

command -v python3 >/dev/null 2>&1 || refuse "python3 is required"
command -v sha256sum >/dev/null 2>&1 || refuse "sha256sum is required"
[ -d "$INSTALL" ] || refuse "$INSTALL does not exist"

# The installation must be outside the checkout it deploys, or it is not
# immutable with respect to the thing being deployed.
real_install="$(readlink -f "$INSTALL")"
real_production="$(readlink -f "$PRODUCTION" 2>/dev/null || echo "$PRODUCTION")"
case "$real_install/" in
  "$real_production"/*) refuse "the controller must not live inside $PRODUCTION" ;;
esac

[ -f "$MANIFEST" ] || refuse "$MANIFEST is not installed"
for file in "${COVERED[@]}"; do
  [ -f "$INSTALL/$file" ] || refuse "$INSTALL/$file is not installed"
  # A full digest line for exactly this file: a loose match would accept a
  # manifest that merely mentions the name.
  grep -qE -- "^[0-9a-f]{64} [ *]${file//./\\.}\$" "$MANIFEST" \
    || refuse "$file is not covered by controller-manifest.sha256"
done
for file in "${OPTIONAL[@]}"; do
  [ -f "$INSTALL/$file" ] || continue
  grep -qE -- "^[0-9a-f]{64} [ *]${file//./\\.}\$" "$MANIFEST" \
    || refuse "$file is installed but not covered by controller-manifest.sha256"
done

# --strict so a malformed manifest line is a refusal, not a skipped check.
( cd "$INSTALL" && sha256sum --check --quiet --strict controller-manifest.sha256 ) \
  || refuse "installed controller files do not match their recorded digests"

exec python3 "$INSTALL/deployment_host.py" --root "$PRODUCTION" \
  --controller-root "$INSTALL" "$@"
