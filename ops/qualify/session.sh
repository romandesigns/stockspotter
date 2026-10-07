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
#   session.sh preflight [YYYY-MM-DD]          before open; verifies and refuses
#   session.sh designate <YYYY-MM-DD> <by> <reason>
#                                              preflight, then designate the
#                                              market day (before its 04:00 ET)
#   session.sh health                          one read-only health dump
#   session.sh settle [ceiling-secs]           blocks until unsettled == 0
#   session.sh export <dest-dir>               preserve + checksum both sides
#   session.sh qualify <session-dir> <date> <out-dir>
#
# Environment:
#   STOCKSPOTTER_BASE     default https://stockspotter.wavystyle.io
#   STOCKSPOTTER_API_TOKEN  required for the health endpoint (never logged)
#   STOCKSPOTTER_CHECKOUT default /opt/apps/stockspotter
#   RESEARCH_DIR          default $STOCKSPOTTER_CHECKOUT/data (holds research/
#                         and discovery-audit/, as the ws container mounts it)
#   MIN_FREE_GB           default 40
#   COMPOSE_PROJECT       default stockspotter-vps
#   CAPTURE_SERVICE       default ws (the process that writes the research capture)

set -euo pipefail

BASE="${STOCKSPOTTER_BASE:-https://stockspotter.wavystyle.io}"
CHECKOUT="${STOCKSPOTTER_CHECKOUT:-/opt/apps/stockspotter}"
# The capture lives in the checkout's data/ (ops/vps/docker-compose.yml mounts
# ../../data at /app/data). The old default, /srv/stockspotter-research, does
# not exist on the VPS, so its disk check read 0 GB and every preflight failed.
RESEARCH_DIR="${RESEARCH_DIR:-$CHECKOUT/data}"
MIN_FREE_GB="${MIN_FREE_GB:-40}"
COMPOSE_PROJECT="${COMPOSE_PROJECT:-stockspotter-vps}"
CAPTURE_SERVICE="${CAPTURE_SERVICE:-ws}"
PREFLIGHT_GATES="$(cd "$(dirname "$0")" && pwd)/preflight_gates.py"

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
#                            outcome-v2 and D3's feature schema 3 together)
#
# Re-bound by the P3 integration (2026-09-25), computed once over the merged
# tree:
#   EXPECTED_OI_CONFIG       oi-cfg-15861d6d0b263f12 -> oi-cfg-73ccdbaf661996ed
#                            (D5: lifecycle "move-v1" + moveInactivitySecs 300
#                            enter the fingerprint)
#   EXPECTED_SPEC_SHA        alpha-qualification-v4 984b8cc3...5d36 ->
#                            alpha-qualification-v5 6dc0fedf...408b
#                            (D13's New York session window and
#                            reference-opportunity-v2; the fingerprint above,
#                            opportunity schema 3 and duplicateIdentityRefused
#                            == 0; the machine gate set with its signal-context,
#                            baseline-policy and lifecycle pins; D7b's
#                            premarket-volume gate. v5, not v4: v4's hash was
#                            already published.)
# The build -- runbook_contract_tests -- fails if this file and the code ever
# disagree.
EXPECTED_OI_CONFIG="oi-cfg-73ccdbaf661996ed"
EXPECTED_OUTCOME_VERSION="opportunity-outcome-v2"
EXPECTED_SPEC_SHA="6dc0fedfc62e7bf7fcfdda67187c7674a8952502c001031c96de9ad7f232408b"
EXPECTED_SPEC_VERSION="alpha-qualification-v5"

# Every other identity the readiness gates compare (P3 §18). Each is pinned to
# the code by runbook_contract_tests, so the build fails the moment the code
# moves and this file does not.
#   EXPECTED_OPPORTUNITY_SCHEMA  3: an id denotes one causal move (D5 move-v1)
#   EXPECTED_LIFECYCLE           backtest_metrics::opportunity::
#                                LIFECYCLE_MOVE_V1_VERSION, the engine's own
#                                constant (preregistration record sha256
#                                0963f174...f849, amendments A1-A3)
EXPECTED_OPPORTUNITY_SCHEMA="3"
EXPECTED_FEATURE_SCHEMA="3"
EXPECTED_SIGNAL_CONTEXT_SCHEMA="2"
EXPECTED_EPISODE_SCHEMA="2"
EXPECTED_BASELINE_POLICY="market-day-0400-ny-v1"
EXPECTED_LIFECYCLE="opportunity-lifecycle-move-v1"

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
  local doc failures=0 day
  need python3
  day="${1:-$(next_market_day)}"
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

  # --- P3 readiness gates (machine-checked, fail closed) --------------------
  #
  # Everything a *designated* session needs beyond the checks above, evaluated
  # by preflight_gates.py from the live health document plus facts gathered
  # here: every schema/lifecycle/baseline pin, retention protection state,
  # writer health, rank capacity, container restarts, and -- the one a
  # deploy-day session fails -- that the capture process started, and began
  # observing, before the market day's 04:00 ET open. An absent field is a
  # FAIL, not a zero. PREFLIGHT_OUT keeps the machine-readable result.
  echo
  echo "READINESS for market day $day"
  local work
  work="$(mktemp -d)"
  printf '%s' "$doc" > "$work/health.json"
  gather_facts "$day" > "$work/facts.json"
  if python3 "$PREFLIGHT_GATES" check --health "$work/health.json" \
       --facts "$work/facts.json" --out "${PREFLIGHT_OUT:-$work/readiness.json}"; then
    ok "readiness gates for $day"
  else
    echo "FAIL readiness gates for $day (see the lines above)"
    failures=$((failures+1))
  fi
  rm -rf "$work"

  echo
  if [ "$failures" -eq 0 ]; then
    echo "PREFLIGHT PASS -- the session may open"
  else
    die "$failures preflight check(s) failed -- do not open the session"
  fi
}

# The next market day whose 04:00 ET open is still ahead.
next_market_day() {
  need python3
  python3 "$PREFLIGHT_GATES" market-day \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["nextMarketDay"])'
}

# The host facts the readiness gates need, as one JSON document on stdout.
# Read-only: git, stat, df and `docker inspect` only. Assembled by
# preflight_gates.py from FACT_* variables so no JSON is built by hand here.
gather_facts() {
  local day="$1" containers ids
  need docker
  need python3
  ids="$(docker ps -aq --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}" 2>/dev/null || true)"
  containers=""
  if [ -n "$ids" ]; then
    # $ids is deliberately unquoted: it is a whitespace-separated id list.
    containers="$(docker inspect --format \
      '{{.Name}}|{{index .Config.Labels "com.docker.compose.service"}}|{{.RestartCount}}|{{.State.StartedAt}}|{{.State.Running}}' \
      $ids 2>/dev/null || true)"
  fi
  FACT_DAY="$day" \
  FACT_HEAD="$(git -C "$CHECKOUT" rev-parse HEAD 2>/dev/null || echo "")" \
  FACT_DEPLOYED="$(cat "$CHECKOUT/ops/vps/.deployed-commit" 2>/dev/null || echo "")" \
  FACT_MARKER_MTIME="$(stat -c %Y "$CHECKOUT/ops/vps/.deployed-commit" 2>/dev/null || echo "")" \
  FACT_DIRTY="$(git -C "$CHECKOUT" status --porcelain 2>/dev/null | wc -l || echo "")" \
  FACT_BEHIND="$(git -C "$CHECKOUT" rev-list --count 'HEAD..@{u}' 2>/dev/null || echo "")" \
  FACT_FREE_GB="$(df -BG --output=avail "$RESEARCH_DIR" 2>/dev/null | tail -1 | tr -dc '0-9' || echo "")" \
  FACT_DIRS="$({ [ -d "$RESEARCH_DIR/research" ] && [ -d "$RESEARCH_DIR/discovery-audit" ]; } && echo true || echo false)" \
  FACT_CONTAINERS="$containers" \
  FACT_MIN_FREE_GB="$MIN_FREE_GB" \
  FACT_CAPTURE_SERVICE="$CAPTURE_SERVICE" \
  FACT_EXPECTED_OI_CONFIG="$EXPECTED_OI_CONFIG" \
  FACT_EXPECTED_OUTCOME_VERSION="$EXPECTED_OUTCOME_VERSION" \
  FACT_EXPECTED_OPPORTUNITY_SCHEMA="$EXPECTED_OPPORTUNITY_SCHEMA" \
  FACT_EXPECTED_FEATURE_SCHEMA="$EXPECTED_FEATURE_SCHEMA" \
  FACT_EXPECTED_SIGNAL_CONTEXT_SCHEMA="$EXPECTED_SIGNAL_CONTEXT_SCHEMA" \
  FACT_EXPECTED_EPISODE_SCHEMA="$EXPECTED_EPISODE_SCHEMA" \
  FACT_EXPECTED_BASELINE_POLICY="$EXPECTED_BASELINE_POLICY" \
  FACT_EXPECTED_LIFECYCLE="$EXPECTED_LIFECYCLE" \
  FACT_EXPECTED_SPEC_SHA="$EXPECTED_SPEC_SHA" \
  FACT_EXPECTED_SPEC_VERSION="$EXPECTED_SPEC_VERSION" \
  python3 "$PREFLIGHT_GATES" facts
}

# ---------------------------------------------------------------------------
# designate -- the explicit designation step, before the market-day open
# ---------------------------------------------------------------------------
#
# A session is designated *before* anything about it can be seen, or it is not
# prospective. This runs the full preflight for that market day (which refuses
# after its 04:00 ET open), and only if it passes writes:
#
#   $RESEARCH_DIR/research/.retention/protected/<day>.json          retention
#   $RESEARCH_DIR/discovery-audit/.retention/protected/<day>.json   protection
#   $RESEARCH_DIR/research/.retention/designations/<day>.json       the record
#
# The record pins commit, OI fingerprint, contract version/SHA, capture start
# and deploy-marker time; `alpha_qualify` refuses a session without it
# (completeness::GATE_TABLE, gate `designation`). Nothing existing is ever
# rewritten: an existing designation is an error, and an existing protection
# record is kept only if it already designates the same day. Each file is
# written beside `protected/` and renamed in, so the retention registry --
# which treats any stray file in `protected/` as "protect everything" -- never
# sees a partial one.
cmd_designate() {
  local day="${1:?usage: session.sh designate <YYYY-MM-DD> <designated-by> <reason>}"
  local by="${2:?usage: session.sh designate <YYYY-MM-DD> <designated-by> <reason>}"
  local reason="${3:?usage: session.sh designate <YYYY-MM-DD> <designated-by> <reason>}"
  need python3

  local record="$RESEARCH_DIR/research/.retention/designations/${day}.json"
  [ -e "$record" ] && die "$record exists -- a designation is evidence and is never rewritten"

  # In a subshell, so its `die` stops the designation rather than this shell
  # before it can report.
  ( cmd_preflight "$day" ) || die "preflight failed -- $day is NOT designated; nothing was written"

  local work
  work="$(mktemp -d)"
  health_json > "$work/health.json"
  gather_facts "$day" > "$work/facts.json"
  # Re-evaluated on this fresh read; `designation` refuses unless it passes.
  python3 "$PREFLIGHT_GATES" check --health "$work/health.json" --facts "$work/facts.json" \
      --out "$work/preflight.json" >/dev/null \
    || die "readiness changed since the preflight -- $day is NOT designated"
  python3 "$PREFLIGHT_GATES" designation --health "$work/health.json" \
      --facts "$work/facts.json" --by "$by" > "$work/designation.json" \
    || die "could not build the designation record -- nothing was written"
  python3 "$PREFLIGHT_GATES" protection --facts "$work/facts.json" --by "$by" \
      --reason "$reason" > "$work/protection.json" \
    || die "could not build the protection record -- nothing was written"

  local dir target
  for dir in research discovery-audit; do
    target="$RESEARCH_DIR/$dir/.retention/protected/${day}.json"
    if [ -e "$target" ]; then
      python3 "$PREFLIGHT_GATES" verify-protection --file "$target" --day "$day" >/dev/null \
        || die "$target exists and does not designate $day -- resolve it by hand; nothing was written"
    fi
  done
  for dir in research discovery-audit; do
    target="$RESEARCH_DIR/$dir/.retention/protected/${day}.json"
    if [ -e "$target" ]; then
      ok "$dir already designates $day (kept, not rewritten)"
      continue
    fi
    mkdir -p "$(dirname "$target")"
    cp "$work/protection.json" "$RESEARCH_DIR/$dir/.retention/.designate-${day}.$$"
    mv "$RESEARCH_DIR/$dir/.retention/.designate-${day}.$$" "$target"
    ok "protected $dir/$day"
  done
  mkdir -p "$(dirname "$record")"
  cp "$work/preflight.json" "$(dirname "$record")/${day}.preflight.json"
  cp "$work/designation.json" "$record.tmp.$$"
  mv "$record.tmp.$$" "$record"
  rm -rf "$work"
  echo "DESIGNATED $day -> $record"
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

  # The expected commit comes from the designation record, written before the
  # open. It used to come from the capture's own health document, which made
  # the commit comparison circular: a session could only ever match itself.
  # No record, no qualification -- the `designation` gate refuses anyway, but
  # this says why before a long run.
  need python3
  local designation="$session/research/.retention/designations/${day}.json"
  [ -f "$designation" ] || die "no designation record at $designation -- an undesignated session cannot qualify"
  local expected_commit
  expected_commit="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["commit"])' \
      "$designation")" || die "unreadable designation record $designation"

  command -v alpha_qualify >/dev/null 2>&1 || die "alpha_qualify is not installed on this host"
  alpha_qualify \
    --session "$session" \
    --session-date "$day" \
    --output "$out" \
    --expected-commit "$expected_commit" \
    --expected-oi-config "$EXPECTED_OI_CONFIG" \
    --expected-spec-sha256 "$EXPECTED_SPEC_SHA"
}

case "${1:-}" in
  preflight) shift; cmd_preflight "$@" ;;
  designate) shift; cmd_designate "$@" ;;
  health)    shift; cmd_health "$@" ;;
  settle)    shift; cmd_settle "$@" ;;
  export)    shift; cmd_export "$@" ;;
  qualify)   shift; cmd_qualify "$@" ;;
  *) sed -n '2,38p' "$0" >&2; exit 1 ;;
esac
