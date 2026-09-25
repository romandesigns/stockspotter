#!/usr/bin/env python3
"""Designated-session preflight: machine-checkable readiness gates (P3 §18).

Called by `ops/qualify/session.sh preflight|designate`, never on its own in
production. It reads two JSON documents -- the live `/research/completeness`
envelope and a facts document the script gathers from the host (git, deploy
marker, docker, disk, clock) -- and evaluates every check below.

    preflight_gates.py check       --health H --facts F [--out RESULT.json]
    preflight_gates.py designation --health H --facts F --by NAME
    preflight_gates.py protection  --facts F --by NAME --reason TEXT
    preflight_gates.py market-day  [--at RFC3339]      next designatable day
    preflight_gates.py facts                            FACT_* env -> facts JSON
    preflight_gates.py verify-protection --file P --day D

Rules, the same ones the post-session qualifier (`completeness::GATE_TABLE`)
applies:

* **Fail closed.** A field that is absent is a FAIL, never a default of 0.
  Several fields are added by other P3 branches (PROVISIONAL below); until
  they land, this preflight cannot pass, which is intended.
* **No override.** There is no flag or environment variable that turns a
  FAIL into a PASS. The exit status is 0 iff every check passed.
* Stdlib only: this runs on the capture host, where python3 is already a
  dependency and nothing else may be installed.
"""
import argparse
import datetime as dt
import json
import os
import sys

try:
    from zoneinfo import ZoneInfo
except ImportError:  # pragma: no cover - python < 3.9
    ZoneInfo = None

# Health fields other P3 branches add. Read by name and FAIL while absent; the
# build (runbook_contract_tests) requires this set to be exactly the fields the
# current health route does not yet emit, so it cannot outlive its reason.
PROVISIONAL = {
    "report.opportunityEngine.duplicateIdentityRefused",
    "report.opportunityEngine.lifecycle",
    "report.oiVersions.lifecycle",
    "report.premarketVolume.fetchFailures",
    "report.opportunityEngine.marketDayId",
}

# (check, path, predicate, expected-pin-name-or-literal, why)
#
# predicate: zero | false | empty | eq (== pins[expected]) | count
HEALTH_CHECKS = [
    # --- the frozen identities ------------------------------------------------
    ("fingerprint", "report.oiConfigFingerprint", "eq", "oiConfig", "the contract is bound to one configuration"),
    ("fingerprint-versions", "report.oiVersions.configFingerprint", "eq", "oiConfig", "rows self-declare the same configuration"),
    ("opportunity-schema", "report.oiVersions.opportunitySchema", "eq", "opportunitySchema", "an id must mean the unit the contract was written for"),
    ("feature-schema", "report.oiVersions.featureSchema", "eq", "featureSchema", "D3 changed feature meanings without renaming"),
    ("signal-context-schema", "report.signalContextSchema", "eq", "signalContextSchema", "D3: market-day scoped preDetection"),
    ("episode-schema", "report.episodeSchema", "eq", "episodeSchema", "episodeUid is the only collision-free join key"),
    ("outcome-version", "report.outcomeMeasurementVersion", "eq", "outcomeVersion", "D4: v1 dispositions are unknown"),
    ("baseline-policy", "report.oiVersions.baselinePolicy", "eq", "baselinePolicy", "D3: 04:00 ET market-day baselines"),
    ("lifecycle-versions", "report.oiVersions.lifecycle", "eq", "lifecycle", "move-v1: one opportunity is one move"),
    ("lifecycle-engine", "report.opportunityEngine.lifecycle", "eq", "lifecycle", "the running engine, not only the row stamp"),
    # --- zero known loss ----------------------------------------------------------
    ("any-known-loss", "anyKnownLoss", "false", None, "the route's own fast path over every loss counter"),
    ("outcomes-dropped", "report.opportunityOutcomes.dropped", "zero", None, "outcome rows lost before the session"),
    ("outcomes-write-errors", "report.opportunityOutcomes.writeErrors", "zero", None, "outcome rows lost before the session"),
    ("outcomes-loss-spans", "report.opportunityOutcomes.lossSpans", "zero", None, "outcome rows lost before the session"),
    ("outcome-capacity-evictions", "report.opportunityOutcomeEngine.capacityEvictions", "zero", None, "anchors evicted before measurement"),
    ("duplicate-identity", "report.opportunityEngine.duplicateIdentityRefused", "zero", None, "move-v1 refused a colliding opportunity"),
    # --- writer health -------------------------------------------------------------
    ("oi-writer-healthy", "report.opportunityIntelligence.degraded", "false", None, "a degraded writer is already dropping"),
    ("measurement-writer-healthy", "report.measurement.degraded", "false", None, "a degraded writer is already dropping"),
    ("outcomes-writer-healthy", "report.opportunityOutcomes.degraded", "false", None, "a degraded writer is already dropping"),
    ("discovery-writer-healthy", "report.discovery.degraded", "false", None, "a degraded writer is already dropping"),
    # --- retention protection / receipts -------------------------------------------
    ("retention-protected-without-receipt", "retention.protectedWithoutReceipt", "empty", None,
     "protected days awaiting a receipt pin disk; install their receipts first"),
    ("retention-registry-errors", "retention.registryErrors", "empty", None, "a malformed registry protects everything and hides pressure"),
    ("retention-blocked", "retention.blockedByProtection", "false", None, "retention already cannot meet its ceiling"),
    ("discovery-retention-blocked", "discoveryRetention.blockedByProtection", "false", None, "discovery retention already cannot meet its ceiling"),
    ("discovery-retention-registry-errors", "discoveryRetention.registryErrors", "empty", None, "a malformed registry protects everything and hides pressure"),
    # --- premarket volume ------------------------------------------------------------
    ("premarket-volume-fetch", "report.premarketVolume.fetchFailures", "zero", None, "D7b: a failed fetch leaves funnel qualification on a stale bar"),
]

# Checks computed from facts (and health), listed so the table is complete.
FACT_CHECKS = [
    ("health-read", "the completeness route answered with a JSON document"),
    ("running-commit", "report.commit == git HEAD == ops/vps/.deployed-commit"),
    ("deploy-marker-before-open", "mtime(.deployed-commit) < market_day_open(day)"),
    ("worktree-clean", "git status --porcelain is empty"),
    ("no-pending-deploy", "HEAD..@{u} is empty"),
    ("spec-sha", "the script's EXPECTED_SPEC_SHA is the one being designated"),
    ("containers-present", "the capture service's container exists and is running"),
    ("container-restarts", "every container of the compose project has RestartCount 0"),
    ("capture-started-before-open", "the capture container started before market_day_open(day)"),
    ("observation-started-before-open", "report.opportunityIntelligence.lastWrite is after the capture start and before the open"),
    ("before-observation-boundary", "now < market_day_open(day)"),
    ("disk", "free GB on the research volume >= floor"),
    ("research-dirs", "research/ and discovery-audit/ exist under RESEARCH_DIR"),
    ("rank-capacity", "report.opportunityEngine.rankCohortCapacity >= capacity > 0"),
    ("timezone", "America/New_York is loadable and report.opportunityEngine.marketDayId == market_day(now)"),
]

MARKET_DAY_START_HOUR = 4


def ny():
    if ZoneInfo is None:
        raise RuntimeError("zoneinfo unavailable (python < 3.9)")
    return ZoneInfo("America/New_York")


def market_day(t):
    """`market_data::trading_session::market_day`: (t in NY - 4h).date()."""
    return (t.astimezone(ny()) - dt.timedelta(hours=MARKET_DAY_START_HOUR)).date()


def market_day_open(day):
    """04:00 America/New_York on `day`, as UTC. 04:00 exists exactly once on
    every US calendar day (transitions are at 02:00)."""
    local = dt.datetime(day.year, day.month, day.day, MARKET_DAY_START_HOUR, tzinfo=ny())
    return local.astimezone(dt.timezone.utc)


def parse_time(text):
    if not isinstance(text, str) or not text:
        return None
    text = text.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    # docker prints nanoseconds; fromisoformat takes at most microseconds.
    if "." in text:
        head, _, rest = text.partition(".")
        digits = len(rest) - len(rest.lstrip("0123456789"))
        frac, tz = rest[:digits], rest[digits:]
        text = f"{head}.{frac[:6]}{tz}"
    try:
        t = dt.datetime.fromisoformat(text)
    except ValueError:
        return None
    return t if t.tzinfo else None


def resolve(doc, path):
    """Envelope path. A `report.X` absent under report is also tried at the
    top level (the D7b branch may put premarketVolume beside retention).
    null is absent."""
    def walk(node, keys):
        for key in keys:
            if not isinstance(node, dict) or key not in node:
                return None
            node = node[key]
        return node
    if not isinstance(doc, dict):
        return None
    value = walk(doc, path.split("."))
    if value is None and path.startswith("report."):
        value = walk(doc, path[len("report."):].split("."))
    return value


def is_count(v):
    return isinstance(v, int) and not isinstance(v, bool)


def evaluate(health, facts):
    """Every check, in table order. Pure: health + facts in, results out."""
    pins = facts.get("expected", {})
    results = []

    def add(check, ok, observed, expected, absent=False):
        results.append({"check": check, "pass": bool(ok) and not absent, "absent": absent,
                        "observed": observed, "expected": expected})

    add("health-read", isinstance(health, dict), "document" if isinstance(health, dict) else health, "a JSON object")
    doc = health if isinstance(health, dict) else {}

    for check, path, predicate, expected, _why in HEALTH_CHECKS:
        value = resolve(doc, path)
        if value is None:
            add(check, False, None, f"{path} present", absent=True)
            continue
        if predicate == "zero":
            add(check, is_count(value) and value == 0, value, 0)
        elif predicate == "false":
            add(check, value is False, value, False)
        elif predicate == "empty":
            add(check, value == [], value, [])
        elif predicate == "eq":
            want = pins.get(expected)
            add(check, want is not None and value == want, value, want, absent=want is None)
        else:  # a typo in the table must fail, not pass
            add(check, False, value, f"unknown predicate {predicate}")

    # --- provenance -----------------------------------------------------------
    head, deployed = facts.get("head") or "", facts.get("deployedCommit") or ""
    running = resolve(doc, "report.commit")
    add("running-commit", bool(head) and head == deployed == running,
        {"head": head, "deployed": deployed, "running": running}, "all three equal")

    now = parse_time(facts.get("now"))
    try:
        day = dt.date.fromisoformat(facts.get("marketDay", ""))
    except ValueError:
        day = None
    open_ = market_day_open(day) if day else None

    marker = facts.get("deployMarkerMtime")
    marker_t = dt.datetime.fromtimestamp(int(marker), dt.timezone.utc) if str(marker or "").isdigit() else None
    add("deploy-marker-before-open", bool(marker_t and open_ and marker_t < open_),
        marker_t.isoformat() if marker_t else None, f"< {open_.isoformat() if open_ else '?'}",
        absent=marker_t is None or open_ is None)

    add("worktree-clean", facts.get("dirtyEntries") == 0, facts.get("dirtyEntries"), 0)
    add("no-pending-deploy", facts.get("behindUpstream") == 0, facts.get("behindUpstream"), 0)
    add("spec-sha", bool(pins.get("specSha")) and facts.get("specSha") == pins.get("specSha"),
        facts.get("specSha"), pins.get("specSha"))

    # --- containers -------------------------------------------------------------
    containers = facts.get("containers") or []
    service = facts.get("captureService") or "ws"
    capture = [c for c in containers if c.get("service") == service]
    add("containers-present", len(capture) == 1 and capture[0].get("running") is True,
        [c.get("name") for c in capture], f"exactly one running {service} container")
    restarts = {c.get("name"): c.get("restartCount") for c in containers}
    add("container-restarts", bool(containers) and all(is_count(v) and v == 0 for v in restarts.values()),
        restarts, "every RestartCount == 0")
    started = parse_time(capture[0].get("startedAt")) if len(capture) == 1 else None
    add("capture-started-before-open", bool(started and open_ and started < open_),
        started.isoformat() if started else None, f"< {open_.isoformat() if open_ else '?'}",
        absent=started is None)

    # The FeatureCache's baseline is complete only if it folded an event
    # before the open (`baselineTruncated` follows the first *event*, not the
    # process start). A write by this process's OI writer after it started
    # and before the open is positive evidence that it did.
    last_write = parse_time(resolve(doc, "report.opportunityIntelligence.lastWrite"))
    add("observation-started-before-open",
        bool(last_write and started and open_ and started <= last_write < open_),
        last_write.isoformat() if last_write else None,
        "capture start <= OI lastWrite < open", absent=last_write is None)

    add("before-observation-boundary", bool(now and open_ and now < open_),
        now.isoformat() if now else None, f"< {open_.isoformat() if open_ else '?'}")

    # --- host -------------------------------------------------------------------
    free, floor = facts.get("freeGb"), facts.get("minFreeGb")
    add("disk", is_count(free) and is_count(floor) and free >= floor, free, f">= {floor}")
    add("research-dirs", facts.get("researchDirsPresent") is True, facts.get("researchDirsPresent"), True)

    rank = resolve(doc, "report.opportunityEngine.rankCohortCapacity")
    cap = resolve(doc, "report.opportunityEngine.capacity")
    add("rank-capacity", is_count(rank) and is_count(cap) and cap > 0 and rank >= cap,
        {"rankCohortCapacity": rank, "capacity": cap}, "rank >= capacity > 0",
        absent=rank is None or cap is None)

    # --- session timezone ---------------------------------------------------------
    try:
        expected_day = market_day(now).isoformat() if now else None
        tz_ok = True
    except Exception as error:  # noqa: BLE001 - reported, never raised
        expected_day, tz_ok = f"tz error: {error}", False
    reported = resolve(doc, "report.opportunityEngine.marketDayId")
    add("timezone", tz_ok and reported is not None and reported == expected_day,
        reported, expected_day, absent=reported is None)

    return results


def summary(results, facts):
    ok = bool(results) and all(r["pass"] for r in results)
    return {"preflight": "PASS" if ok else "FAIL", "marketDay": facts.get("marketDay"),
            "evaluatedAt": facts.get("now"), "checks": results}


def load(path):
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, ValueError) as error:
        return {"__unreadable__": str(error)}


def cmd_check(args):
    health, facts = load(args.health), load(args.facts)
    if "__unreadable__" in health:
        health = None
    result = summary(evaluate(health, facts), facts)
    for r in result["checks"]:
        tag = "  ok  " if r["pass"] else ("ABSENT" if r["absent"] else " FAIL ")
        print(f"{tag} {r['check']}: observed {json.dumps(r['observed'])}"
              + ("" if r["pass"] else f", expected {json.dumps(r['expected'])}"))
    if args.out:
        with open(args.out, "w", encoding="utf-8") as fh:
            json.dump(result, fh, indent=2, sort_keys=True)
            fh.write("\n")
    print(f"READINESS {result['preflight']} ({sum(r['pass'] for r in result['checks'])}/{len(result['checks'])})")
    return 0 if result["preflight"] == "PASS" else 1


def cmd_designation(args):
    """The designation record `completeness::DesignationRecord` reads. Refuses
    unless every check passes *now*, so a record can never carry a FAIL."""
    health, facts = load(args.health), load(args.facts)
    result = summary(evaluate(None if "__unreadable__" in health else health, facts), facts)
    if result["preflight"] != "PASS":
        sys.exit("preflight_gates: refusing to write a designation for a failing preflight")
    if not args.by.strip():
        sys.exit("preflight_gates: --by is required")
    capture = [c for c in facts["containers"] if c.get("service") == (facts.get("captureService") or "ws")][0]
    marker = dt.datetime.fromtimestamp(int(facts["deployMarkerMtime"]), dt.timezone.utc)
    record = {
        "schemaVersion": 1,
        "marketDay": facts["marketDay"],
        "designatedAt": facts["now"],
        "designatedBy": args.by,
        "commit": facts["head"],
        "oiConfigFingerprint": resolve(health, "report.oiConfigFingerprint"),
        "specVersion": facts["expected"]["specVersion"],
        "specSha256": facts["expected"]["specSha"],
        "processStartedAt": parse_time(capture["startedAt"]).astimezone(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
        "deployMarkerAt": marker.isoformat().replace("+00:00", "Z"),
        "containerRestartCounts": {c["name"]: c["restartCount"] for c in facts["containers"]},
        "preflight": "PASS",
    }
    json.dump(record, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


def cmd_protection(args):
    """`market_data::retention_registry::ProtectionRecord`, class designated."""
    facts = load(args.facts)
    if not args.by.strip() or not args.reason.strip():
        sys.exit("preflight_gates: --by and --reason are required")
    json.dump({"schemaVersion": 1, "date": facts["marketDay"], "class": "designated",
               "reason": args.reason, "protectedBy": args.by, "protectedAt": facts["now"]},
              sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


def cmd_verify_protection(args):
    """Exit 0 iff FILE is a valid designated record for DAY (an existing one is
    kept, never rewritten)."""
    record = load(args.file)
    ok = (record.get("schemaVersion") == 1 and record.get("date") == args.day
          and record.get("class") == "designated"
          and str(record.get("reason", "")).strip() and str(record.get("protectedBy", "")).strip())
    print("designated" if ok else f"not a valid designated record for {args.day}: {record}")
    return 0 if ok else 1


def cmd_facts(_args):
    """Assembles the facts document from the FACT_* variables session.sh sets,
    so the shell never builds JSON by hand. Unreadable numbers become null,
    which every check treats as a FAIL."""
    env = os.environ

    def num(key):
        text = (env.get(key) or "").strip()
        return int(text) if text.isdigit() else None

    containers = []
    for line in (env.get("FACT_CONTAINERS") or "").splitlines():
        parts = line.strip().split("|")
        if len(parts) == 5:
            containers.append({"name": parts[0].lstrip("/"), "service": parts[1],
                               "restartCount": int(parts[2]) if parts[2].isdigit() else None,
                               "startedAt": parts[3], "running": parts[4] == "true"})
    expected = {
        "oiConfig": env.get("FACT_EXPECTED_OI_CONFIG"),
        "outcomeVersion": env.get("FACT_EXPECTED_OUTCOME_VERSION"),
        "opportunitySchema": num("FACT_EXPECTED_OPPORTUNITY_SCHEMA"),
        "featureSchema": num("FACT_EXPECTED_FEATURE_SCHEMA"),
        "signalContextSchema": num("FACT_EXPECTED_SIGNAL_CONTEXT_SCHEMA"),
        "episodeSchema": num("FACT_EXPECTED_EPISODE_SCHEMA"),
        "baselinePolicy": env.get("FACT_EXPECTED_BASELINE_POLICY"),
        "lifecycle": env.get("FACT_EXPECTED_LIFECYCLE"),
        "specSha": env.get("FACT_EXPECTED_SPEC_SHA"),
        "specVersion": env.get("FACT_EXPECTED_SPEC_VERSION"),
    }
    json.dump({
        "marketDay": env.get("FACT_DAY", ""),
        "now": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "head": (env.get("FACT_HEAD") or "").strip(),
        "deployedCommit": (env.get("FACT_DEPLOYED") or "").strip(),
        "deployMarkerMtime": (env.get("FACT_MARKER_MTIME") or "").strip(),
        "dirtyEntries": num("FACT_DIRTY"),
        "behindUpstream": num("FACT_BEHIND"),
        "freeGb": num("FACT_FREE_GB"),
        "minFreeGb": num("FACT_MIN_FREE_GB"),
        "researchDirsPresent": env.get("FACT_DIRS") == "true",
        "captureService": env.get("FACT_CAPTURE_SERVICE") or "ws",
        "containers": containers,
        "specSha": expected["specSha"],
        "expected": expected,
    }, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


def cmd_market_day(args):
    """The next market day whose 04:00 ET open is still ahead, and its open."""
    now = parse_time(args.at) if args.at else dt.datetime.now(dt.timezone.utc)
    day = market_day(now) + dt.timedelta(days=1)
    print(json.dumps({"now": now.isoformat(), "currentMarketDay": market_day(now).isoformat(),
                      "nextMarketDay": day.isoformat(), "opensAt": market_day_open(day).isoformat()}))
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check")
    c.add_argument("--health", required=True)
    c.add_argument("--facts", required=True)
    c.add_argument("--out")
    d = sub.add_parser("designation")
    d.add_argument("--health", required=True)
    d.add_argument("--facts", required=True)
    d.add_argument("--by", required=True)
    p = sub.add_parser("protection")
    p.add_argument("--facts", required=True)
    p.add_argument("--by", required=True)
    p.add_argument("--reason", required=True)
    v = sub.add_parser("verify-protection")
    v.add_argument("--file", required=True)
    v.add_argument("--day", required=True)
    m = sub.add_parser("market-day")
    m.add_argument("--at")
    sub.add_parser("facts")
    args = ap.parse_args()
    return {"check": cmd_check, "designation": cmd_designation, "protection": cmd_protection,
            "verify-protection": cmd_verify_protection, "market-day": cmd_market_day,
            "facts": cmd_facts}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
