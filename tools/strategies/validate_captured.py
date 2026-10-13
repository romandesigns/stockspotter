"""Read-only archived bars -> exact Rust core -> descriptive retrospective results.

Never fetches, edits sources, reads outcome caches, selects winning thresholds,
or claims original receipt/revision provenance. Derived files go to --output.
"""
import argparse
from collections import Counter, defaultdict
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import time

DEVELOPMENT_END = "2026-06-30"
EVALUATION_START = "2026-07-01"
FIRST_DATE = "2026-04-20"
LAST_DATE = "2026-09-04"
PREFIX_CAP = 2 * 1024 * 1024


def sha(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def archived_bars(path):
    """Read only a bounded prefix; trades/quotes are not materialized."""
    with path.open("rb") as file:
        prefix = file.read(PREFIX_CAP).decode("utf-8", errors="strict")
    match = re.search(r'"bars"\s*:\s*', prefix)
    if not match:
        raise ValueError("bars not found within bounded prefix")
    header = json.loads(prefix[:match.start()].rstrip().rstrip(",") + "}")
    bars, _ = json.JSONDecoder().raw_decode(prefix, match.end())
    if not isinstance(bars, list) or len(bars) > 1440:
        raise ValueError("invalid/capped bar array")
    return header["symbol"], bars


def normalize(raw, day):
    time_ms = int(datetime.fromisoformat(raw["t"].replace("Z", "+00:00")).timestamp() * 1000)
    # Receipt and decision are IDEALIZED, not reconstructed or backfilled facts.
    return dict(marketOpenMs=time_ms, receivedAtMs=time_ms + 60_000,
                decisionAtMs=time_ms + 60_000, marketDay=day,
                open=raw["o"], high=raw["h"], low=raw["l"], close=raw["c"],
                volume=raw["v"], isFinal=True, intervalSecs=60)


def outcome(bars, event):
    """Complete contiguous future horizon only, with a fresh denominator for held."""
    anchor = event["eventTimeMs"]
    # Candidate and hold outcomes are separate estimands. No forward bar is
    # supplied to the detector to create either signal.
    price = event["observedPrice"]
    future = [b for b in bars if anchor <= b["marketOpenMs"] < anchor + 300_000]
    expected = [anchor + offset * 60_000 for offset in range(5)]
    if [b["marketOpenMs"] for b in future] != expected:
        return {"classification": "censored", "reason": "incomplete_or_gapped_horizon"}
    if any(not math.isfinite(b["low"]) or not math.isfinite(b["high"]) or b["low"] <= 0 or b["high"] < b["low"] for b in future):
        return {"classification": "censored", "reason": "invalid_future_bar"}
    target = price * 1.02
    return dict(classification="reach" if any(b["high"] >= target for b in future) else "no_reach",
                anchorPrice=price, horizonSeconds=300,
                maxExcursionPct=(max(b["high"] for b in future) / price - 1) * 100,
                minExcursionPct=(min(b["low"] for b in future) / price - 1) * 100)


def split(day):
    return "development" if day <= DEVELOPMENT_END else "retrospective_evaluation"


def run(source, output, binary, repo):
    source = source.resolve(strict=True)
    output = output.resolve()
    if output == source or source in output.parents:
        raise ValueError("output must be outside the source archive")
    output.mkdir(parents=True, exist_ok=False)
    config_raw = subprocess.run([str(binary), "--config"], check=True, capture_output=True).stdout
    config = json.loads(config_raw)
    config_hash = hashlib.sha256(config_raw).hexdigest()
    core_hash = sha(repo / "crates/momentum-strategies/src/lib.rs")
    binary_hash = sha(binary)
    evaluator_hash = sha(Path(__file__))
    contract_hash = sha(repo / "docs/IAC-EXTREME-MOMENTUM-V1.md")
    # This receipt is written before any source bar array or outcome is loaded.
    plan = dict(schema="iac-extreme-retrospective-plan-v1", createdAt=datetime.now(timezone.utc).isoformat(),
                source=str(source), config=config, configSha256=config_hash, coreSha256=core_hash,
                binarySha256=binary_hash, evaluatorSha256=evaluator_hash, contractSha256=contract_hash,
                comparator="same-session contiguous-warmup non-anchor bars per strategy; includes lifecycle/cooldown and zero compression denominators; not detector-equivalent rejected bars",
                minimumRatioCell=20, developmentEnd=DEVELOPMENT_END, evaluationStart=EVALUATION_START,
                firstDate=FIRST_DATE, lastDate=LAST_DATE, thresholdSearch=False,
                availability="idealized_completed_bar_close", receiptProvenance="unavailable",
                revisionProvenance="final_history_only", population="selected_archived_symbols",
                prospective=False, frozenOiTouched=False)
    (output / "plan.json").write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
    inventory = []
    sessions = {}
    exclusions = []
    normalized_path = output / "bars.ndjson"
    names = sorted(source.glob("*-sip-v1.json"))
    if not names:
        raise ValueError("no archived session files")
    with normalized_path.open("w", encoding="utf-8", newline="\n") as target:
        for index, path in enumerate(names):
            match = re.fullmatch(r"([A-Za-z0-9.-]+)-(\d{4}-\d{2}-\d{2})-sip-v1\.json", path.name)
            if not match or not FIRST_DATE <= match[2] <= LAST_DATE:
                exclusions.append(dict(file=path.name, reason="outside_declared_scope"))
                continue
            before = path.stat()
            digest = sha(path)
            symbol, raw = archived_bars(path)
            after = path.stat()
            if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
                raise ValueError(f"source changed while reading: {path.name}")
            if symbol != match[1]:
                raise ValueError(f"symbol mismatch: {path.name}")
            # Input ordering is retained. Sorting corrections into a hindsight
            # sequence would falsely claim original receipt reproduction.
            bars = [normalize(bar, match[2]) for bar in raw]
            if any(bars[i]["marketOpenMs"] <= bars[i-1]["marketOpenMs"] for i in range(1, len(bars))):
                exclusions.append(dict(file=path.name, sha256=digest, reason="nonmonotonic_history"))
                continue
            session_id = f"{symbol}/{match[2]}"
            sessions[session_id] = (bars, match[2])
            inventory.append(dict(path=str(path), sha256=digest, bytes=before.st_size,
                                  mtimeNs=before.st_mtime_ns, sessionId=session_id, split=split(match[2]), bars=len(bars)))
            for bar in bars:
                target.write(json.dumps(dict(sessionId=session_id, symbol=symbol, bar=bar), separators=(",", ":")) + "\n")
            if index % 100 == 0:
                print(f"inventoried {index + 1}/{len(names)} sessions", flush=True)
    (output / "input-manifest.json").write_text(json.dumps(dict(inputs=inventory, exclusions=exclusions), indent=2) + "\n", encoding="utf-8")
    started = time.perf_counter()
    events_path = output / "replay.ndjson"
    with normalized_path.open("rb") as bars_file, events_path.open("wb") as events_file:
        result = subprocess.run([str(binary), "--idealized-close-availability"], stdin=bars_file, stdout=events_file, stderr=subprocess.PIPE)
    if result.returncode:
        raise RuntimeError(f"Rust replay failed closed: {result.stderr.decode(errors='replace')}")
    elapsed = time.perf_counter() - started
    groups = defaultdict(Counter)
    per_session = defaultdict(Counter)
    outcome_rows = []
    counters = Counter()
    summaries = set()
    session_counters = {}
    candidate_times = defaultdict(set)
    with events_path.open(encoding="utf-8") as file:
        for line in file:
            row = json.loads(line)
            if row["type"] == "replay_summary":
                if row["sessionId"] in summaries:
                    raise ValueError("duplicate Rust session summary")
                if row.get("availability") != plan["availability"] or row.get("configSha256") != config_hash:
                    raise ValueError("replay availability/config identity mismatch")
                summaries.add(row["sessionId"])
                session_counters[row["sessionId"]] = row["counters"]
                counters.update(row["counters"])
                continue
            session_id = f'{row["symbol"]}/{row["marketDay"]}'
            bars, day = sessions[session_id]
            key = f'{split(day)}/{row["strategy"]}'
            groups[key][row["kind"]] += 1
            per_session[f'{session_id}/{row["strategy"]}'][row["kind"]] += 1
            if row["kind"] == "candidate":
                candidate_times[(session_id,row["strategy"])].add(row["eventTimeMs"])
            if row["kind"] in ("candidate", "held"):
                observed = outcome(bars, row)
                groups[key][f'{row["kind"]}_{observed["classification"]}'] += 1
                outcome_rows.append(dict(sessionId=session_id, strategy=row["strategy"], stage=row["kind"],
                                         eventId=row["eventId"], split=split(day), **observed))
    expected = {entry["sessionId"] for entry in inventory if entry["bars"]}
    if not expected:
        raise ValueError("no nonempty session bars")
    if summaries != expected:
        raise ValueError("missing, duplicate or unknown Rust replay session summary")
    # A descriptive within-session null, declared before outcomes. It has no
    # time-of-day matching or independence claim. Shared horizon/censoring rule.
    for session_id, (bars,day) in sessions.items():
        contiguous = 0
        previous = None
        for index,bar in enumerate(bars):
            if previous is not None and bar["marketOpenMs"] - previous != 60_000:
                contiguous = 0
            valid = all(math.isfinite(bar[field]) and bar[field] > 0 for field in ("open","high","low","close"))
            valid = valid and bar["high"] >= max(bar["open"],bar["close"]) and bar["low"] <= min(bar["open"],bar["close"])
            if valid:
                for strategy,window in (("iac",config["compressionBars"]),("extreme_momentum",config["extremeBars"]-1)):
                    needed = config["baselineBars"] + window
                    anchor = bar["marketOpenMs"] + 60_000
                    baseline = bars[index-needed:index-window] if contiguous >= needed else []
                    if baseline and sum(b["high"]-b["low"] for b in baseline) > 0 and sum(b["volume"] for b in baseline) > 0:
                        if anchor not in candidate_times[(session_id,strategy)]:
                            key=f"{split(day)}/{strategy}"
                            null=outcome(bars,dict(eventTimeMs=anchor,observedPrice=bar["close"]))
                            groups[key][f'control_{null["classification"]}'] += 1
                contiguous += 1
            else:
                contiguous = 0
            previous = bar["marketOpenMs"]
    # Rehash sources after replay before accepting a result.
    for index, entry in enumerate(inventory):
        if sha(Path(entry["path"])) != entry["sha256"]:
            raise ValueError(f'source bytes changed: {entry["path"]}')
        if index % 100 == 0:
            print(f"reverified {index + 1}/{len(inventory)} source hashes", flush=True)
    if (sha(repo / "crates/momentum-strategies/src/lib.rs") != core_hash or sha(binary) != binary_hash
            or sha(Path(__file__)) != evaluator_hash or sha(repo / "docs/IAC-EXTREME-MOMENTUM-V1.md") != contract_hash):
        raise ValueError("detector/evaluator/contract changed during evaluation")
    accepted = sum(entry["bars"] for entry in inventory)
    report = dict(schema="iac-extreme-retrospective-results-v1", completedAt=datetime.now(timezone.utc).isoformat(),
                  planSha256=sha(output / "plan.json"), configSha256=config_hash, coreSha256=core_hash,
                  normalizedInputSha256=sha(normalized_path), replaySha256=sha(events_path),
                  sessions=len(inventory), bars=accepted, symbols=sorted({key.split('/')[0] for key in sessions}),
                  splitSessions=dict(Counter(entry["split"] for entry in inventory)), exclusions=exclusions,
                  groups={key: dict(value) for key, value in sorted(groups.items())}, counters=dict(counters),
                  ratios={key:{stage:(value[f"{stage}_reach"]/(value[f"{stage}_reach"]+value[f"{stage}_no_reach"])
                            if value[f"{stage}_reach"]+value[f"{stage}_no_reach"]>=20 else None)
                            for stage in ("candidate","held","control")} for key,value in sorted(groups.items())},
                  replayElapsedSeconds=elapsed, averageReplayMicrosecondsPerBar=elapsed / max(1, accepted) * 1_000_000,
                  limitations=["selected archived symbols; no market-wide recall", "final OHLC idealization; no original receipts or revision sequence",
                               "previously studied retrospective sessions, not untouched future confirmation", "bar highs are descriptive reaches, not fills or net returns",
                               "full gapped/short horizons censored even if an earlier high crossed target",
                               "controls include lifecycle/cooldown and zero compression denominators; not detector-equivalent rejected bars",
                               "held-stage horizons extend beyond lifecycle expiry and may overlap; observations not independent",
                               "no parameter search or optimization", "no deployment or activation"])
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    (output / "per-session.json").write_text(json.dumps(dict(eventCounts={key:dict(value) for key,value in sorted(per_session.items())},counters=session_counters), indent=2) + "\n", encoding="utf-8")
    (output / "outcomes.ndjson").write_text("".join(json.dumps(row) + "\n" for row in outcome_rows), encoding="utf-8")
    print(json.dumps(report, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    run(args.source, args.output, args.binary.resolve(strict=True), root)
