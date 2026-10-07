"""Produce a daily discovery review and attach broker-confirmed trader status."""
import argparse
from collections import Counter
from datetime import datetime
import json
import os
from pathlib import Path
import time
import urllib.request
from zoneinfo import ZoneInfo

from analyze_discovery import analyze, read_files


def review(date, data_root):
    sources, quality = [], Counter()
    paths = sorted((data_root / "discovery-audit").glob(f"{date}-*.jsonl"))
    report = analyze(read_files(paths, sources, quality))
    report["sources"] = sources
    report["quality"].update(quality)
    report["review_date"] = date
    report["generated_at"] = datetime.now(ZoneInfo("UTC")).isoformat()
    report["paper_status"] = None
    try:
        request = urllib.request.Request("http://ws:8788/auto-trader/status?limit=200", headers={
            "Authorization": "Bearer " + os.environ["STOCKSPOTTER_API_TOKEN"]})
        with urllib.request.urlopen(request, timeout=15) as response:
            status = json.load(response)
        if status.get("executionMode") != "alpaca_paper":
            raise ValueError("trader status is not broker paper mode")
        report["paper_status"] = status
    except Exception as error:
        report["paper_status_error"] = str(error)
    report["limitations"] = [
        "Candidate labels use sampled trade prices, not executable quotes.",
        "Missing receipt is unknown; monitor presence is not subscription acknowledgement.",
        "Paper status is cumulative and recent history is capped at 200 entries; it is not a complete per-day trade export.",
        "Whole-market recall remains unmeasured. Review capture gaps before interpreting candidate counts.",
    ]
    directory = data_root / "discovery-reports"
    directory.mkdir(parents=True, exist_ok=True)
    content = json.dumps(report, indent=2, allow_nan=False) + "\n"
    for name in (f"{date}.json", "latest.json"):
        temporary = directory / (name + ".tmp")
        temporary.write_text(content, encoding="utf-8")
        temporary.replace(directory / name)
    print(json.dumps({"date": date, "status": report["status"], "candidates": report["candidate_count"],
                      "paper_status_available": report["paper_status"] is not None}), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--date")
    parser.add_argument("--data", type=Path, default=Path("/app/data"))
    args = parser.parse_args()
    now = datetime.now(ZoneInfo("America/New_York"))
    review(args.date or now.date().isoformat(), args.data)
    if args.once:
        return
    completed = now.date() if (now.hour, now.minute) >= (16, 10) else None
    while True:
        now = datetime.now(ZoneInfo("America/New_York"))
        if (now.hour, now.minute) >= (16, 10) and completed != now.date():
            try:
                review(now.date().isoformat(), args.data)
                completed = now.date()
            except Exception as error:
                print(f"Discovery review failed; will retry: {error}", flush=True)
        time.sleep(30)


if __name__ == "__main__":
    main()
