"""Read-only prospective-session evidence probe. Never schedules or changes production.

Local: python observe-session.py --date 2026-09-21 --out monday-baseline.json
Repeat with --baseline monday-baseline.json --out monday-smoke.json.
Exit 0 = sampled smoke PASS, 2 = PENDING, 1 = FAIL. A sample is not a full-session verdict.
"""
import argparse
import datetime as dt
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import sys

BASE = "7e365866ce5ae18ea77cce05d6d8f45bc69c7faa"
FINGERPRINT = "oi-cfg-b4f21c8b311a1b99"
HORIZONS = [30, 60, 120, 300, 600, 1200]
ROOT = pathlib.Path("/opt/apps/stockspotter")
DATA = ROOT / "data/research"
SERVICES = ["ws", "auto-trader", "qualify", "discovery-review"]


def command(*args):
    return subprocess.check_output(args, text=True, timeout=40).strip()


def tail_sample(path, byte_limit=256 * 1024):
    """Bounded live read; an unfinished last line is pending, never malformed."""
    if not path.exists():
        return {"bytes": 0, "rows": [], "malformed": 0, "partialTail": False}
    with path.open("rb") as stream:
        size = stream.seek(0, 2)
        offset = max(0, size - byte_limit)
        stream.seek(offset)
        if offset:
            stream.readline()  # discard a potentially partial first row
        raw = stream.read(byte_limit)
    partial = bool(raw) and not raw.endswith(b"\n")
    lines = raw.splitlines()
    if partial:
        lines = lines[:-1]
    rows, malformed = [], 0
    for line in lines[-50:]:
        try:
            value = json.loads(line)
            if not isinstance(value, dict):
                raise ValueError("not an object")
            rows.append(value)
        except (ValueError, UnicodeDecodeError):
            malformed += 1
    return {"bytes": size, "rows": rows, "malformed": malformed, "partialTail": partial}


def probe(date):
    # Credential remains inside qualify's existing environment; only health leaves it.
    request = """import json,os,urllib.request
r=urllib.request.Request('http://ws:8788/research/completeness',headers={'Authorization':'Bearer '+os.environ['STOCKSPOTTER_API_TOKEN']})
print(json.dumps(json.load(urllib.request.urlopen(r,timeout=10))))
"""
    health = json.loads(command("docker", "exec", "stockspotter-vps-qualify-1", "python", "-c", request))
    containers = json.loads(command("docker", "inspect", *[f"stockspotter-vps-{s}-1" for s in SERVICES]))
    identity = {c["Name"]: {"id": c["Id"], "image": c["Image"], "started": c["State"]["StartedAt"],
                            "running": c["State"]["Running"]} for c in containers}
    mounts = next(c["Mounts"] for c in containers if c["Name"].endswith("-ws-1"))
    return {
        "date": date, "observedAt": dt.datetime.now(dt.timezone.utc).isoformat(),
        "head": command("git", "-C", str(ROOT), "rev-parse", "HEAD"),
        "deployed": (ROOT / "ops/vps/.deployed-commit").read_text().strip(),
        "dirty": command("git", "-C", str(ROOT), "status", "--porcelain"),
        "protected": identity,
        "configHash": hashlib.sha256((ROOT / ".env").read_bytes()).hexdigest(),
        "qualificationHash": hashlib.sha256((ROOT / "ops/qualify/session.sh").read_bytes()).hexdigest(),
        "mounts": [{k: m[k] for k in ("Source", "Destination", "RW")} for m in mounts],
        "freeBytes": shutil.disk_usage(DATA).free,
        "health": health,
        "samples": {stem: tail_sample(DATA / f"{stem}-{date}.ndjson") for stem in
                    ("opportunity-outcomes", "opportunity-intelligence", "episodes",
                     "opportunity-outcomes-markers", "opportunity-intelligence-markers", "episodes-markers")},
    }


def valid_id(row):
    try:
        opened = dt.datetime.fromisoformat(row["openedAt"].replace("Z", "+00:00")).astimezone(dt.timezone.utc)
        sequence = ((opened.hour * 60 + opened.minute) * 60 + opened.second) * 1000 + opened.microsecond // 1000
        return row["opportunityId"] == f'{row["symbol"]}:{row["sessionDate"]}:{sequence}'
    except (KeyError, ValueError, TypeError):
        return False


def valid_episode(row):
    # Preserve nine-digit source precision: Python datetime alone truncates nanos.
    opened = str(row.get("openedAt", ""))
    match = re.fullmatch(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?Z", opened)
    if not match:
        return False
    timestamp = match[1] + "." + (match[2] or "").ljust(9, "0") + "Z"
    expected = f'epu1|{row.get("id", {}).get("symbol")}|{timestamp}|{row.get("openedBy")}'
    return row.get("schemaVersion") == 2 and row.get("episodeUid") == expected


def assess(snapshot, baseline=None):
    errors, pending = [], []
    health = snapshot.get("health", {})
    report = health.get("report", {})
    if not all(v == BASE for v in (snapshot.get("head"), snapshot.get("deployed"), report.get("commit"))):
        errors.append("frozen revision disagreement")
    if report.get("oiConfigFingerprint") != FINGERPRINT:
        errors.append("OI fingerprint absent or changed")
    if snapshot.get("dirty"):
        errors.append("production checkout dirty")
    services = snapshot.get("protected", {})
    if len(services) != 4 or not all(c.get("running") for c in services.values()):
        errors.append("protected service missing or stopped")
    if baseline:
        for key in ("protected", "configHash", "qualificationHash", "head", "deployed"):
            if snapshot.get(key) != baseline.get(key):
                errors.append(f"frozen state changed: {key}")
    else:
        pending.append("baseline required to prove no service/config change across session")
    if not any(m.get("Source") == "/opt/apps/stockspotter/data" and m.get("Destination") == "/app/data" and m.get("RW")
               for m in snapshot.get("mounts", [])):
        errors.append("expected durable capture mount unavailable")
    if snapshot.get("freeBytes", 0) < 40 * 1024**3:
        errors.append("disk headroom below runbook 40 GiB floor")
    if health.get("anyKnownLoss") is not False:
        errors.append("known loss or absent loss report")
    if health.get("retention", {}).get("retentionPending") is not False:
        errors.append("retention pending or unknown")
    for name in ("measurement", "opportunityIntelligence", "opportunityOutcomes", "discovery"):
        writer = report.get(name)
        if not isinstance(writer, dict):
            errors.append(f"{name} capture unavailable")
            continue
        for key in ("writeErrors", "budgetDropped", "queueLost", "dropped", "lossSpans"):
            if writer.get(key, 0):
                errors.append(f"{name}.{key}={writer[key]}")
        if writer.get("degraded") is not False:
            errors.append(f"{name} degraded or unknown")
    for name in ("opportunityEngine", "opportunityOutcomeEngine", "measurementPending"):
        engine = report.get(name, health.get(name, {}))
        if not engine:
            errors.append(f"{name} health missing")
        for key in ("capacityEvictions", "cohortTruncations", "evictionMarkersDropped"):
            if engine.get(key, 0):
                errors.append(f"{name}.{key}={engine[key]}")
    engine = report.get("opportunityOutcomeEngine", {})
    writer = report.get("opportunityOutcomes", {})
    if engine.get("capacity") != 297000 or writer.get("queueCapacity") != 16384 or writer.get("queueCapacityBytes") != 64 * 1024**2:
        errors.append("outcome capacity contract mismatch")
    if not engine.get("anchorsCreated", 0):
        pending.append("no live anchors yet; weekend idle is not a smoke PASS")
    samples = snapshot.get("samples", {})
    for stem, sample in samples.items():
        if sample.get("malformed"):
            errors.append(f"malformed sampled {stem} rows")
        if sample.get("partialTail"):
            pending.append(f"{stem} write in progress; reread")
    for stem in ("opportunity-outcomes", "opportunity-intelligence", "episodes"):
        rows = samples.get(stem, {}).get("rows", [])
        if not rows:
            pending.append(f"no target-date {stem} sample yet")
        for row in rows:
            time_key = {"opportunity-outcomes": "anchorAt", "opportunity-intelligence": "timestamp", "episodes": "openedAt"}[stem]
            try:
                observed = dt.datetime.fromisoformat(row[time_key].replace("Z", "+00:00"))
                started = dt.datetime.fromisoformat(services["/stockspotter-vps-ws-1"]["started"].replace("Z", "+00:00"))
                if observed <= started or observed.date().isoformat() != snapshot["date"]:
                    errors.append(f"{stem} row is not new target-session evidence")
            except (KeyError, ValueError, TypeError):
                errors.append(f"{stem} timestamp missing or invalid")
            if stem == "opportunity-outcomes":
                p = row.get("provenance", {})
                if p.get("measurementVersion") != "opportunity-outcome-v1" or p.get("opportunitySchema") != 2 or p.get("configFingerprint") != FINGERPRINT:
                    errors.append("outcome provenance invalid")
                if sorted(r.get("horizonSecs", -1) for r in row.get("returns", [])) != HORIZONS:
                    errors.append("outcome horizons mismatch")
                if not valid_id(row):
                    errors.append("outcome time-derived ID invalid")
            elif stem == "opportunity-intelligence":
                if row.get("schemaVersion") != 2 or row.get("versions", {}).get("opportunitySchema") != 2 or not valid_id(row):
                    errors.append("ranking schema or time-derived ID invalid")
                if any(row.get(k) is None for k in ("observedHigh", "observedLow", "maxMovePct", "minMovePct", "openingPrice", "openedAt")):
                    errors.append("RiskQuality causal field missing")
            elif not valid_episode(row):
                errors.append("canonical schema-2 episode identity absent")
    settled = engine.get("anchorsCreated", 0) > 0 and engine.get("anchorsSettled") == engine.get("anchorsCreated") and engine.get("outstanding") == 0
    accounted = writer.get("attempted") == writer.get("written", 0) + writer.get("dropped", 0) + writer.get("writeErrors", 0)
    if not accounted or writer.get("queueDepth", 0):
        pending.append("outcome writer still draining; reread before declaring smoke complete")
    return {"status": "FAIL" if errors else "PENDING" if pending else "PASS_SAMPLED_SMOKE",
            "errors": sorted(set(errors)), "pending": sorted(set(pending)),
            "outcomeSettlementObserved": settled, "writerAccountingBalanced": accounted,
            "fullSessionVerdict": "NOT_EVALUATED: requires stable post-close full export and per-anchor reconciliation",
            "observedAt": snapshot.get("observedAt"), "date": snapshot.get("date")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--date", default="2026-09-21")
    parser.add_argument("--host", default="stockspotter-vps")
    parser.add_argument("--input", type=pathlib.Path)
    parser.add_argument("--baseline", type=pathlib.Path)
    parser.add_argument("--out", type=pathlib.Path)
    parser.add_argument("--probe-date", help=argparse.SUPPRESS)
    args = parser.parse_args()
    dt.date.fromisoformat(args.probe_date or args.date)
    if args.probe_date:
        print(json.dumps(probe(args.probe_date)))
        return 0
    if args.input:
        snapshot = json.loads(args.input.read_text())
    else:
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", args.host,
                                 "python3", "-", "--probe-date", args.date],
                                input=pathlib.Path(__file__).read_text(), capture_output=True, text=True, timeout=75, check=True)
        snapshot = json.loads(result.stdout)
    if args.out:
        with args.out.open("x", encoding="utf-8") as output:
            json.dump(snapshot, output, indent=2)
    baseline = json.loads(args.baseline.read_text()) if args.baseline else None
    result = assess(snapshot, baseline)
    print(json.dumps(result, indent=2))
    return {"FAIL": 1, "PENDING": 2, "PASS_SAMPLED_SMOKE": 0}[result["status"]]


if __name__ == "__main__":
    sys.exit(main())
