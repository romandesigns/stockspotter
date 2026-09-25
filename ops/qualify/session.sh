#!/usr/bin/env bash
#
# Prospective-session automation (§44, §45).
#
# Automates the repetitive, error-prone parts of running one untouched
# session: preflight verification, health reads, waiting for the settlement
# frontier, export with two-sided checksums, and the qualification run.
#
# WHAT THIS SCRIPT DELIBERATELY CANNOT DO
# ---------------------------------------
# It never tunes a threshold, fits a model, enables a strategy, deploys
# anything, or modifies Auto-Trader. That is not a policy comment -- there
# is no code path here that does any of those things, so there is nothing
# to accidentally enable. Human review sits between the evidence this
# script produces and any production change.
#
# It also never *repairs* production. Every check either passes or aborts.
# A session repaired while it runs is no longer an untouched session.
#
# Usage:
#   session.sh preflight                       before open; verifies and refuses
#   session.sh health                          one read-only health dump
#   session.sh settle [ceiling-secs]           blocks until unsettled == 0
#   session.sh export <dest-dir>               preserve + checksum both sides
#   session.sh qualify <session-dir> <date> <out-dir>
#
# Environment:
#   STOCKSPOTTER_BASE     default https://stockspotter.wavystyle.io
#   STOCKSPOTTER_API_TOKEN  required for the health endpoint (never logged)
#   STOCKSPOTTER_CHECKOUT default /opt/apps/stockspotter
#   RESEARCH_DIR          default /srv/stockspotter-research
#   MIN_FREE_GB           default 40

set -euo pipefail

BASE="${STOCKSPOTTER_BASE:-https://stockspotter.wavystyle.io}"
CHECKOUT="${STOCKSPOTTER_CHECKOUT:-/opt/apps/stockspotter}"
RESEARCH_DIR="${RESEARCH_DIR:-/srv/stockspotter-research}"
MIN_FREE_GB="${MIN_FREE_GB:-40}"

# Frozen by the qualification contract. A capture carrying a different
# fingerprint describes a different engine, and comparing the two would
# attribute one configuration's behaviour to another.
#
# Re-bound 2026-09-25 by the measurement-correctness work, deliberately:
#   EXPECTED_OI_CONFIG       oi-cfg-b4f21c8b311a1b99 -> oi-cfg-15861d6d0b263f12
#                            (D6: maxRankCohort 4,096 -> 16,375)
#   EXPECTED_OUTCOME_VERSION opportunity-outcome-v1 -> opportunity-outcome-v2
#                            (D4: disposition is measured; v1 rows say
#                            still_open for everything, which means "unknown")
#   EXPECTED_SPEC_SHA        alpha-qualification-v3 a4106f3a...c317 ->
#                            alpha-qualification-v4 984b8cc3...5d36
#                            (v3's criteria, re-bound to the D6 fingerprint,
#                            outcome-v2 and D3's feature schema 3 together;
#                            the build -- runbook_contract_tests -- fails if
#                            this file and the code ever disagree)
#
# Re-bound again by P3 D5 (opportunity lifecycle move-v1), PROVISIONALLY --
# the P3 integrator recomputes both after merging the other P3 branches:
#   EXPECTED_OI_CONFIG       oi-cfg-15861d6d0b263f12 -> oi-cfg-73ccdbaf661996ed
#                            (lifecycle "move-v1" + moveInactivitySecs 300 enter
#                            the fingerprint)
#   EXPECTED_SPEC_SHA        984b8cc3...5d36 -> 5bc94f59...dbcf
#                            (the fingerprint above, opportunity schema 3, and
#                            duplicateIdentityRefused == 0 as a gate)
EXPECTED_OI_CONFIG="oi-cfg-73ccdbaf661996ed"
EXPECTED_OUTCOME_VERSION="opportunity-outcome-v2"
EXPECTED_SPEC_SHA="5bc94f59c9e100b2018ada11681f0928ea44c65752643d11bf863cb74ebfdbcf"

die() { echo "FAIL: $*" >&2; exit 1; }
ok()  { echo "  ok   $*"; }

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"; }

# One authenticated read. The token is passed via a header from the
# environment and never appears in a URL, a log line, or the process table
# in a form this script prints.
health_json() {
  need curl
  [ -n "${STOCKSPOTTER_API_TOKEN:-}" ] || die "STOCKSPOTTER_API_TOKEN is not set"
  curl -fsS -H "Authorization: Bearer ${STOCKSPOTTER_API_TOKEN}" \
       "${BASE}/research/completeness" \
    || die "health query failed -- ${BASE}/research/completeness did not answer"
}

# Reads one field out of the health document. Uses python3 rather than jq
# because python3 is already a dependency of this box and jq is not.
jget() {
  # Same reason as the contract check: unguarded, a missing python3 would abort
  # the whole preflight at exit 127 partway down its list of ok lines.
  need python3
  python3 -c '
import json, sys
doc = json.load(sys.stdin)
path = sys.argv[1].split(".")
node = doc
for key in path:
    if node is None:
        print("")
        sys.exit(0)
    node = node.get(key) if isinstance(node, dict) else None
print("" if node is None else node)
' "$1"
}

# ---------------------------------------------------------------------------
# preflight -- before open
# ---------------------------------------------------------------------------
cmd_preflight() {
  echo "PREFLIGHT $(date -Is)"
  local doc failures=0
  doc="$(health_json)"

  # --- provenance: three independent sources must agree -------------------
  local head deployed running
  head="$(git -C "$CHECKOUT" rev-parse HEAD)"
  deployed="$(cat "$CHECKOUT/ops/vps/.deployed-commit" 2>/dev/null || echo "")"
  running="$(printf '%s' "$doc" | jget report.commit)"

  [ -n "$running" ] || { echo "FAIL live health carries no commit -- provenance unprovable"; failures=$((failures+1)); }
  if [ "$head" = "$deployed" ] && [ "$deployed" = "$running" ]; then
    ok "commit $head (checkout == .deployed-commit == running process)"
  else
    echo "FAIL commit disagreement: HEAD=$head deployed=$deployed running=$running"
    failures=$((failures+1))
  fi

  # --- the frozen identities ----------------------------------------------
  local fingerprint
  fingerprint="$(printf '%s' "$doc" | jget report.oiConfigFingerprint)"
  if [ "$fingerprint" = "$EXPECTED_OI_CONFIG" ]; then
    ok "oi config $fingerprint"
  else
    echo "FAIL oi config fingerprint: expected $EXPECTED_OI_CONFIG, live reports '${fingerprint:-absent}'"
    failures=$((failures+1))
  fi

  # An absent field is a FAIL here, not a pass: a build that predates the
  # version fields cannot prove it writes outcome-v2 rows.
  local outcome_version
  outcome_version="$(printf '%s' "$doc" | jget report.outcomeMeasurementVersion)"
  if [ "$outcome_version" = "$EXPECTED_OUTCOME_VERSION" ]; then
    ok "outcome measurement $outcome_version"
  else
    echo "FAIL outcome measurement version: expected $EXPECTED_OUTCOME_VERSION, live reports '${outcome_version:-absent}'"
    failures=$((failures+1))
  fi

  # --- every loss counter, named individually -----------------------------
  #
  # Named individually on purpose. An aggregate "losses: 0" hides which
  # counter a future non-zero came from, and the three writers fail for
  # different reasons.
  local counters=(
    "report.opportunityIntelligence.dropped"
    "report.opportunityIntelligence.writeErrors"
    "report.opportunityIntelligence.lossSpans"
    "report.measurement.dropped"
    "report.measurement.writeErrors"
    "report.measurement.lossSpans"
    "report.discovery.queueLost"
    "report.discovery.writeErrors"
    "report.discovery.budgetDropped"
  )
  local key value
  for key in "${counters[@]}"; do
    value="$(printf '%s' "$doc" | jget "$key")"
    if [ "${value:-0}" = "0" ]; then
      ok "$key = 0"
    else
      echo "FAIL $key = ${value:-unreadable} -- the capture already lost records before the session began"
      failures=$((failures+1))
    fi
  done

  # --- capacity -----------------------------------------------------------
  #
  # The per-surface truncation counters are structurally zero since D6 bound
  # the ranked cohort to open capacity; a non-zero one means a mis-specified
  # bound or an engine defect, and names the surface.
  for key in "report.opportunityEngine.capacityEvictions" \
             "report.opportunityEngine.cohortTruncations" \
             "report.opportunityEngine.earlyCohortTruncations" \
             "report.opportunityEngine.continuationCohortTruncations" \
             "report.opportunityEngine.truncationMarkersDropped" \
             "report.opportunityEngine.evictionMarkersDropped" \
             "report.opportunityEngine.duplicateIdentityRefused" \
             "measurementPending.capacityEvictions"; do
    value="$(printf '%s' "$doc" | jget "$key")"
    if [ "${value:-0}" = "0" ]; then ok "$key = 0"
    else echo "FAIL $key = $value"; failures=$((failures+1)); fi
  done

  # --- retention must not be waiting to run -------------------------------
  value="$(printf '%s' "$doc" | jget retention.retentionPending)"
  if [ "${value:-False}" = "False" ] || [ -z "${value:-}" ]; then
    ok "retention not pending"
  else
    echo "FAIL retention sweep is pending -- it may delete during the session"
    failures=$((failures+1))
  fi

  # --- disk ---------------------------------------------------------------
  local free_gb
  free_gb="$(df -BG --output=avail "$RESEARCH_DIR" 2>/dev/null | tail -1 | tr -dc '0-9' || echo 0)"
  if [ "${free_gb:-0}" -ge "$MIN_FREE_GB" ]; then
    ok "disk ${free_gb}G free (floor ${MIN_FREE_GB}G)"
  else
    echo "FAIL disk ${free_gb}G free, below the ${MIN_FREE_GB}G floor -- Sept 16 wrote 19.3 GB"
    failures=$((failures+1))
  fi

  # --- no deploy pending --------------------------------------------------
  if [ -n "$(git -C "$CHECKOUT" status --porcelain)" ]; then
    echo "FAIL checkout is dirty -- deploy state is not reproducible"
    failures=$((failures+1))
  else
    ok "checkout clean"
  fi
  git -C "$CHECKOUT" fetch --quiet origin 2>/dev/null || true
  local behind
  behind="$(git -C "$CHECKOUT" rev-list --count HEAD..@{u} 2>/dev/null || echo 0)"
  if [ "${behind:-0}" = "0" ]; then
    ok "no unapproved deploy pending"
  else
    echo "FAIL $behind commit(s) upstream would deploy mid-session"
    failures=$((failures+1))
  fi

  # --- the contract -------------------------------------------------------
  #
  # The contract is a property of the *analysis* build, not of this capture
  # host, so `alpha_qualify` is legitimately absent here -- it runs where the
  # exported artifact is evaluated. That is reported, never silently skipped.
  #
  # It is also not unverified when absent: the contract source lives in the
  # commit this preflight just pinned three ways, so a verified deployed
  # commit already pins the contract text transitively. The direct hash
  # comparison below is the independent confirmation, and it must happen on
  # the host that will actually run the qualification.
  #
  # Guarded with `command -v` because a missing binary under `set -euo
  # pipefail` aborts the whole script at exit 127 -- after printing nothing
  # but `ok` lines. An operator skimming that output would read an abort as a
  # pass, which is the exact failure this preflight exists to prevent.
  local spec_sha
  if command -v alpha_qualify >/dev/null 2>&1; then
    spec_sha="$(alpha_qualify --print-spec 2>&1 >/dev/null | awk '/sha256/ {print $3}' || true)"
    if [ "$spec_sha" = "$EXPECTED_SPEC_SHA" ]; then
      ok "qualification contract $spec_sha"
    else
      echo "FAIL contract hash: expected $EXPECTED_SPEC_SHA, this build carries ${spec_sha:-unreadable}"
      failures=$((failures+1))
    fi
  else
    echo "  DEFER qualification contract $EXPECTED_SPEC_SHA"
    echo "        alpha_qualify is not on this host, which is expected for a capture"
    echo "        host. Confirm the hash on the analysis host before the session:"
    echo "          alpha_qualify --print-spec"
    echo "        The deployed commit verified above already pins the contract source."
  fi

  echo
  if [ "$failures" -eq 0 ]; then
    echo "PREFLIGHT PASS -- the session may open"
  else
    die "$failures preflight check(s) failed -- do not open the session"
  fi
}

# ---------------------------------------------------------------------------
# health -- one read-only dump, safe to run during the session
# ---------------------------------------------------------------------------
cmd_health() { health_json | python3 -m json.tool; }

# ---------------------------------------------------------------------------
# settle -- wait for the actual settlement frontier, never a guessed delay
# ---------------------------------------------------------------------------
cmd_settle() {
  # The longest horizon is 900s, so an episode opened at the bell is not
  # settled for fifteen minutes after it. The ceiling is generous rather
  # than tight: hitting it is itself a finding.
  local ceiling="${1:-5400}" waited=0 interval=30 pending
  echo "waiting for settlement frontier (ceiling ${ceiling}s)"
  while [ "$waited" -lt "$ceiling" ]; do
    pending="$(health_json | jget measurementPending.pending)"
    if [ "${pending:-1}" = "0" ]; then
      echo "settled after ${waited}s -- unsettled == 0"
      return 0
    fi
    echo "  ${waited}s: ${pending} episode(s) still open"
    sleep "$interval"
    waited=$((waited + interval))
  done
  die "still ${pending} unsettled episode(s) after ${ceiling}s -- the session cannot speak about them"
}

# ---------------------------------------------------------------------------
# export -- preserve, checksum BOTH sides, then make immutable
# ---------------------------------------------------------------------------
cmd_export() {
  local dest="${1:?usage: session.sh export <dest-dir>}"
  [ -e "$dest" ] && die "$dest already exists -- a preserved session is never written over"
  need rsync; need sha256sum

  local day doc
  day="$(date -u +%Y-%m-%d)"
  doc="$(health_json)"

  local unsettled
  unsettled="$(printf '%s' "$doc" | jget measurementPending.pending)"
  [ "${unsettled:-1}" = "0" ] || die "unsettled == ${unsettled} -- run 'settle' first"

  mkdir -p "$dest/research"
  # Verbatim. A transcription step is a place for a session to be described
  # by a document that does not match it.
  printf '%s' "$doc" > "$dest/research/completeness-${day}.json"

  echo "copying capture artifacts"
  rsync -a --info=stats1 "$RESEARCH_DIR/research/" "$dest/research/"
  rsync -a --info=stats1 "$RESEARCH_DIR/discovery-audit/" "$dest/discovery-audit/"

  # Two-sided checksums. Copying is the step most likely to lose a byte
  # silently, so the source is digested too and the two lists must agree.
  echo "checksumming both sides"
  ( cd "$RESEARCH_DIR" && find research discovery-audit -type f -print0 \
      | sort -z | xargs -0 sha256sum ) > "$dest/.source-sums"
  ( cd "$dest" && find research discovery-audit -type f -print0 \
      | sort -z | xargs -0 sha256sum ) > "$dest/.dest-sums-raw"
  # The completeness document exists only on the destination side.
  grep -v "completeness-${day}.json" "$dest/.dest-sums-raw" > "$dest/.dest-sums"
  if diff -q "$dest/.source-sums" "$dest/.dest-sums" >/dev/null; then
    echo "  every artifact matches its source byte for byte"
  else
    diff "$dest/.source-sums" "$dest/.dest-sums" | head -20
    die "checksum mismatch between source and export -- the copy is not trustworthy"
  fi
  mv "$dest/.dest-sums-raw" "$dest/SHA256SUMS"
  rm -f "$dest/.source-sums" "$dest/.dest-sums"

  # Tell the retention sweep to leave it alone, then take write permission
  # away so an accident cannot reach it.
  touch "$dest/.hold"
  chmod -R a-w "$dest"
  echo "preserved and frozen at $dest"
}

# ---------------------------------------------------------------------------
# qualify -- exactly once, against the immutable artifact
# ---------------------------------------------------------------------------
cmd_qualify() {
  local session="${1:?usage: session.sh qualify <session-dir> <date> <out-dir>}"
  local day="${2:?session date YYYY-MM-DD}"
  local out="${3:?output directory (must not exist)}"
  [ -e "$out" ] && die "$out exists -- a qualification result is evidence and is never overwritten"

  local running
  running="$(python3 -c '
import json, sys
doc = json.load(open(sys.argv[1]))
# Accept both the route response and a bare report, exactly as the
# qualifier does -- an operator may legitimately have saved either.
print((doc.get("report") or doc).get("commit", ""))
' "$session/research/completeness-${day}.json" 2>/dev/null || echo "")"

  alpha_qualify \
    --session "$session" \
    --session-date "$day" \
    --output "$out" \
    ${running:+--expected-commit "$running"} \
    --expected-oi-config "$EXPECTED_OI_CONFIG" \
    --expected-spec-sha256 "$EXPECTED_SPEC_SHA"
}

case "${1:-}" in
  preflight) shift; cmd_preflight "$@" ;;
  health)    shift; cmd_health "$@" ;;
  settle)    shift; cmd_settle "$@" ;;
  export)    shift; cmd_export "$@" ;;
  qualify)   shift; cmd_qualify "$@" ;;
  *) sed -n '2,30p' "$0" >&2; exit 1 ;;
esac
