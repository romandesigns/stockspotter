"""Analyze prospective evidence; no network, strategy changes, or order submission."""
import argparse
from bisect import bisect_left, bisect_right
from collections import Counter, defaultdict
from datetime import datetime
import hashlib
import json
import math
from pathlib import Path
from zoneinfo import ZoneInfo

ET = ZoneInfo("America/New_York")


def epoch(value):
    dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if dt.tzinfo is None:
        raise ValueError("audit timestamps must have a timezone")
    return dt.timestamp()


def session(at):
    dt = datetime.fromtimestamp(at, ET)
    minute = dt.hour * 60 + dt.minute
    if 240 <= minute < 960:
        return dt.date().isoformat(), "premarket" if minute < 570 else "regular"
    return None


def continuous(times, start, end):
    """Require both endpoints and bounded gaps, without filling missing prices."""
    return (bool(times) and 0 <= times[0] - start <= 30
            and 0 <= end - times[-1] <= 30
            and all(b - a <= 60 for a, b in zip(times, times[1:])))


def analyze(records):
    points, receipts, alerts = defaultdict(list), defaultdict(list), defaultdict(list)
    coverage, decisions, scans = [], [], {}
    quality = Counter()
    feeds = set()
    for record in records:
        if record.get("schema") != 1:
            raise ValueError("unsupported discovery audit schema")
        at, kind, data = epoch(record["recorded_at"]), record["kind"], record["data"]
        quality["records"] += 1
        quality["max_reported_lost_records"] = max(
            quality["max_reported_lost_records"], record["lost_records"])
        if kind == "scan_started":
            if data["scan_id"] in scans:
                raise ValueError("duplicate scan ID: supply each capture file once")
            scans[data["scan_id"]] = {"expected": set(data["universe"]),
                                       "requested": set(), "complete": False}
            feeds.add(data["feed"])
        elif kind == "snapshot_batch":
            scan = scans.get(data["scan_id"])
            if scan is None:
                quality["orphan_batches"] += 1
            else:
                requested = set(data["requested"])
                quality["duplicate_requested_symbols"] += len(requested & scan["requested"])
                scan["requested"].update(requested)
            quality["requested_snapshots"] += len(data["requested"])
            quality["missing_snapshot_responses"] += len(set(data["requested"]) - data["snapshots"].keys())
            for symbol, raw in data["snapshots"].items():
                trade = raw.get("latestTrade") or {}
                price, market_at = trade.get("p"), trade.get("t")
                if not isinstance(price, (int, float)) or not math.isfinite(price) or price <= 0 or not market_at:
                    quality["invalid_trade_snapshots"] += 1
                    continue
                if not -1 <= at - epoch(market_at) <= 60:
                    quality["stale_trade_snapshots"] += 1
                    continue
                group = session(at)
                if group is None:
                    quality["outside_session_snapshots"] += 1
                    continue
                points[(symbol, group[0])].append((at, float(price)))
        elif kind == "snapshot_complete":
            if data["scan_id"] in scans:
                scans[data["scan_id"]]["complete"] = True
            else:
                quality["orphan_completions"] += 1
        elif kind == "scan_completed":
            decisions.append((at, set(data["quiet_selected"])))
        elif kind == "coverage":
            coverage.append((at, set(data["full_ignition"]) | set(data["universe_ignition"])))
            for symbol, receipt in data["receipts"].items():
                received = epoch(receipt["received_at"])
                if receipt["ignition_monitored"] and -1 <= received - epoch(receipt["market_at"]) <= 60:
                    receipts[symbol].append(received)
        elif kind == "ignition" and data["stage"] == "confirmed":
            # Receipt time, not backdated market time, determines useful lead.
            if -1 <= at - epoch(data["market_at"]) <= 60:
                alerts[data["symbol"]].append(at)
    quality["scans_started"] = len(scans)
    quality["incomplete_snapshot_scans"] = sum(not s["complete"] for s in scans.values())
    quality["unrequested_universe_symbols"] = sum(len(s["expected"] - s["requested"]) for s in scans.values())
    for series in (receipts, alerts):
        for values in series.values():
            values.sort()
    coverage.sort(key=lambda row: row[0])
    decisions.sort(key=lambda row: row[0])
    coverage_times = [t for t, _ in coverage]
    decision_times = [t for t, _ in decisions]
    candidates, seen = [], set()
    base_counts = Counter()
    for (symbol, date), samples in sorted(points.items()):
        samples = sorted(set(samples))
        times = [t for t, _ in samples]
        for i, (at, price) in enumerate(samples):
            if not .25 <= price <= 3:
                continue
            if at - times[0] < 300:
                continue
            left = bisect_left(times, at - 300)
            base = samples[left:i + 1]
            if not continuous(times[left:i + 1], at - 300, at):
                continue
            prices = [p for _, p in base]
            if max(prices) / min(prices) > 1.02:
                continue
            base_counts["flat_base_anchors"] += 1
            right = bisect_right(times, at + 1200)
            forward = samples[i:right]
            if times[-1] < at + 1200 or not continuous(times[i:right], at, at + 1200):
                base_counts["censored_base_anchors"] += 1
                continue
            base_counts["fully_observed_base_anchors"] += 1
            crossing = next((t for t, p in forward if p / price >= 1.1 - 1e-12), None)
            if crossing is None or (symbol, date) in seen:
                continue
            seen.add((symbol, date))
            receipt_times = receipts[symbol]
            before = bisect_right(receipt_times, at)
            observed = before > 0 and receipt_times[before - 1] >= at - 60
            ci = bisect_right(coverage_times, at) - 1
            configured = (symbol in coverage[ci][1]) if ci >= 0 and at - coverage[ci][0] <= 30 else None
            di = bisect_right(decision_times, at) - 1
            selected = (symbol in decisions[di][1]) if di >= 0 and at - decisions[di][0] <= 60 else None
            first = bisect_left(alerts[symbol], at)
            alert = alerts[symbol][first] if first < len(alerts[symbol]) and alerts[symbol][first] <= crossing else None
            candidates.append({"symbol": symbol, "date": date, "session": session(at)[1],
                "base_endpoint_epoch": at, "base_price": price, "crossing_epoch": crossing,
                "monitored_receipt_before_base": True if observed else None,
                "configured_monitor_before_base": configured, "quiet_selected_before_base": selected,
                "first_confirmed_alert_epoch": alert,
                "alert_lead_seconds": crossing - alert if alert is not None else None})
    return {"protocol": "discovery-coverage-v1", "status": "observed_sample" if candidates else "insufficient_evidence",
        "feeds": sorted(feeds), "quality": dict(quality), "bases": dict(base_counts),
        "candidate_count": len(candidates), "observed_monitored_candidates": sum(c["monitored_receipt_before_base"] is True for c in candidates),
        "candidates_with_confirmed_alert_before_crossing": sum(c["first_confirmed_alert_epoch"] is not None for c in candidates),
        "whole_market_recall": None, "candidates": candidates}


def read_files(paths, provenance, quality):
    for path in paths:
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for raw in source:
                digest.update(raw)
                if not raw.endswith(b"\n"):
                    quality["partial_lines"] += 1
                    continue
                yield json.loads(raw)
        provenance.append({"path": str(path.resolve()), "sha256": digest.hexdigest()})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    paths = sorted({p.resolve() for item in args.inputs for p in (item.glob("*.jsonl") if item.is_dir() else [item])})
    provenance, quality = [], Counter()
    report = analyze(read_files(paths, provenance, quality))
    report["sources"] = provenance
    repo = Path(__file__).resolve().parents[1]
    report["analysis_sources"] = [{"path": str(p), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
        for p in [Path(__file__).resolve(), repo / "docs/discovery-coverage-protocol-2026-09-07.md"]]
    report["quality"].update(quality)
    report["limitations"] = ["Sampled price-pattern labels are not executable opportunities or fills.",
        "No receipt means unknown; configured monitors do not prove provider subscription.",
        "Current provider universe is not historical common-stock or float eligibility.",
        "Capture gaps, stale prices, missing symbols and censored paths prevent whole-market recall claims."]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({k: report[k] for k in ("status", "candidate_count", "quality", "bases")}))


if __name__ == "__main__":
    main()
