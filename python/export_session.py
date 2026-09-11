"""Package one captured research session for copying to the analysis machine.

Read-only: it never contacts the network, never touches strategy state, and
never submits an order. It reads capture files, compresses them, and writes a
manifest describing exactly what it produced.

Why Python and not Rust: the capture writer lives in `market-data`, a crate on
the strategy path, and adding a compression dependency there would put new code
between tick dispatch and disk for no benefit. Compression belongs at export --
offline, after the session, where a stall costs nothing. Python's stdlib
supplies gzip and sha256, so this adds no dependency anywhere.

Provenance follows the discipline `analyze_discovery.py` already established:
every input is hashed, and this script hashes itself, so a report can be traced
back to the exact bytes and code that produced it.

Usage:

    python python/export_session.py data/discovery-audit --output exports/
    python python/export_session.py data/ --output exports/ --expect episodes

Produces:

    exports/session-YYYY-MM-DD/
        manifest.json
        <name>.ndjson.gz ...
        SHA256SUMS

# Schema 2 (R3/R4)

Two defects in schema 1 are fixed here.

**Directory inputs silently excluded `.ndjson`** -- the glob matched `*.jsonl`
only, while the episode writer emits `.ndjson`. Pointing this script at a
capture directory produced a clean exit code, a valid-looking manifest, and no
episodes at all. Directory inputs now match both suffixes, and `--expect`
turns "the dataset I needed is missing" into a non-zero exit instead of a
plausible artifact.

**`captureStartedAt`/`captureEndedAt` were filesystem mtimes.** Exporting from
copied files therefore described the copy, not the capture. Schema 2 separates
the four clocks that actually exist -- see `TIMESTAMP_SEMANTICS`, which is
embedded in every manifest so a reader never has to guess which one a field
means.
"""
import argparse
import gzip
import hashlib
import json
import os
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path

SCHEMA_VERSION = 2
CHUNK = 1024 * 1024

# Both suffixes the capture writers actually produce. `market-data`'s discovery
# recorder writes `.jsonl`; the measurement collector writes `.ndjson`. Matching
# only one of them is what made a partial export look complete.
CAPTURE_SUFFIXES = ("*.jsonl", "*.ndjson")

# Capture files are grouped by what they contain, so the analysis side can load
# one kind at a time rather than parsing everything to find what it needs.
GROUPS = {
    "discovery": ("discovery-audit", "discovery"),
    "signals": ("live_pending_signals", "live_evaluated_signals"),
    "episodes": ("episodes",),
    "outcomes": ("outcomes", "horizons"),
    "trader": ("auto_trader_journal", "alpaca_paper_ledger"),
}

# What each manifest time field means, carried in the artifact itself. A reader
# that only has the file must be able to tell a capture clock from a filesystem
# clock without consulting this source.
TIMESTAMP_SEMANTICS = {
    "exportedAt": "Wall-clock instant this export ran. Says nothing about the capture.",
    "sourceFileModifiedRange": (
        "Filesystem mtime range of the input files. Metadata only -- it reflects when the "
        "bytes were last written or copied, which for transferred files is the transfer."
    ),
    "eventTimeRange": (
        "Range of market/event timestamps found inside the records (episode open/close, "
        "discovery recorded_at). null when the dataset carries no such field."
    ),
    "captureTimeRange": (
        "Range of receipt timestamps found inside the records -- when Stockspotter observed "
        "the thing, e.g. openingContext.capturedAt. null when the dataset carries no such field."
    ),
}


def classify(path):
    """Which group a capture file belongs to; `other` when nothing matches, so
    an unrecognised file is still exported rather than silently dropped."""
    stem = path.stem.lower()
    for group, prefixes in GROUPS.items():
        if any(prefix in stem for prefix in prefixes):
            return group
    return "other"


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def _iso_bounds(current, value):
    """Folds one ISO-8601 string into a (min, max) pair. Compared as strings
    deliberately: every producer here emits UTC with a `Z` suffix, so lexical
    order is chronological order, and parsing 135k records twice is not free.
    A value that does not look like an ISO date is ignored rather than guessed
    at."""
    if not isinstance(value, str) or len(value) < 20 or value[4] != "-":
        return current
    low, high = current
    return (
        value if low is None or value < low else low,
        value if high is None or value > high else high,
    )


def record_times(group, record):
    """(event times, capture times) contributed by one record.

    Returns empty tuples where the dataset genuinely has no such concept. That
    is the point of R4: a missing semantic timestamp yields `null` in the
    manifest rather than being back-filled from a different clock.
    """
    events, captures = [], []
    if group == "episodes":
        for key in ("openedAt", "closedAt"):
            if key in record:
                events.append(record[key])
        context = record.get("openingContext")
        if isinstance(context, dict) and "capturedAt" in context:
            captures.append(context["capturedAt"])
    elif group == "discovery":
        # The discovery recorder's `recorded_at` is a receipt clock: the instant
        # the recorder saw the record, not a market event time.
        if "recorded_at" in record:
            captures.append(record["recorded_at"])
    return events, captures


def count_and_compress(source, destination, group):
    """Streams `source` into gzip, counting records and malformed lines, and
    collecting the record-derived time ranges R4 needs.

    Counts are computed from the bytes actually written, not from a separate
    pass, so a manifest count can never disagree with the file it describes.
    A line that does not parse is counted and still copied verbatim -- dropping
    it would create a silent gap, which is precisely what the capture format's
    own `lost_records` accounting exists to avoid.
    """
    records = 0
    errors = 0
    partial = 0
    event_range = (None, None)
    capture_range = (None, None)
    with source.open("rb") as raw, gzip.open(destination, "wb", compresslevel=6) as out:
        for line in raw:
            if not line.endswith(b"\n"):
                partial += 1
            stripped = line.strip()
            if not stripped:
                continue
            records += 1
            try:
                parsed = json.loads(stripped)
            except json.JSONDecodeError:
                errors += 1
            else:
                if isinstance(parsed, dict):
                    events, captures = record_times(group, parsed)
                    for value in events:
                        event_range = _iso_bounds(event_range, value)
                    for value in captures:
                        capture_range = _iso_bounds(capture_range, value)
            out.write(line if line.endswith(b"\n") else line + b"\n")
    return records, errors, partial, event_range, capture_range


def _as_range(bounds):
    low, high = bounds
    if low is None or high is None:
        return None
    return {"start": low, "end": high}


def _merge(a, b):
    return (
        b[0] if a[0] is None or (b[0] is not None and b[0] < a[0]) else a[0],
        b[1] if a[1] is None or (b[1] is not None and b[1] > a[1]) else a[1],
    )


def session_date_of(paths, override):
    """Session date, preferred from an explicit flag, else from the first
    record's own timestamp, else from the filename. Never from `today` -- an
    export run days later must not relabel the session."""
    if override:
        return override
    for path in paths:
        try:
            with path.open("rb") as source:
                for line in source:
                    record = json.loads(line)
                    stamp = record.get("recorded_at") or record.get("timestamp") or record.get("detectedAt") or record.get("openedAt")
                    if isinstance(stamp, str) and len(stamp) >= 10:
                        return stamp[:10]
                    break
        except (OSError, json.JSONDecodeError):
            continue
    for path in paths:
        stem = path.stem
        if len(stem) >= 10 and stem[4] == "-" and stem[7] == "-":
            return stem[:10]
    raise SystemExit("could not determine the session date; pass --session-date")


def git_commit():
    try:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True, text=True, check=True,
            cwd=Path(__file__).resolve().parents[1],
        )
        return result.stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def collect_inputs(items):
    """Every capture file named by `items`. A directory contributes both capture
    suffixes; an explicit file is taken as given regardless of extension, so an
    operator can still name something unusual on purpose."""
    found = set()
    for item in items:
        if item.is_dir():
            for pattern in CAPTURE_SUFFIXES:
                found.update(p.resolve() for p in item.glob(pattern) if p.is_file())
        elif item.is_file():
            found.add(item.resolve())
    return sorted(found)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path,
                        help="capture files, or directories of them")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--session-date", default=None)
    parser.add_argument(
        "--expect", action="append", default=[], metavar="GROUP",
        choices=sorted(GROUPS) + ["other"],
        help=("dataset group that MUST be present; repeatable. Exits non-zero if absent. "
              "A valid-looking partial export is worse than a loud failure."),
    )
    args = parser.parse_args()

    paths = collect_inputs(args.inputs)
    if not paths:
        raise SystemExit("no capture files found")

    present = {classify(p) for p in paths}
    missing = [group for group in args.expect if group not in present]
    if missing:
        raise SystemExit(
            "expected dataset(s) absent from the export: "
            + ", ".join(sorted(missing))
            + f"; found: {', '.join(sorted(present)) or 'nothing'}"
        )

    session_date = session_date_of(paths, args.session_date)
    destination = args.output / f"session-{session_date}"
    destination.mkdir(parents=True, exist_ok=True)

    files = []
    totals = {"records": 0, "errors": 0, "partialLines": 0}
    mtime_low = mtime_high = None
    event_range = (None, None)
    capture_range = (None, None)

    for path in paths:
        group = classify(path)
        target = destination / f"{path.stem}.ndjson.gz"
        records, errors, partial, events, captures = count_and_compress(path, target, group)
        event_range = _merge(event_range, events)
        capture_range = _merge(capture_range, captures)
        stat = path.stat()
        modified = datetime.fromtimestamp(stat.st_mtime, timezone.utc)
        mtime_low = min(mtime_low, modified) if mtime_low else modified
        mtime_high = max(mtime_high, modified) if mtime_high else modified
        totals["records"] += records
        totals["errors"] += errors
        totals["partialLines"] += partial
        files.append({
            "name": target.name,
            "group": group,
            "sourceName": path.name,
            "records": records,
            "errorRecords": errors,
            "partialLines": partial,
            "sourceBytes": stat.st_size,
            "compressedBytes": target.stat().st_size,
            "sha256": sha256_file(target),
            "sourceSha256": sha256_file(path),
            "eventTimeRange": _as_range(events),
            "captureTimeRange": _as_range(captures),
        })

    source_bytes = sum(f["sourceBytes"] for f in files)
    compressed_bytes = sum(f["compressedBytes"] for f in files)
    manifest = {
        "artifact": "stockspotter-research-session",
        "schemaVersion": SCHEMA_VERSION,
        "sessionDate": session_date,
        "exportedAt": datetime.now(timezone.utc).isoformat(),
        # Filesystem metadata, named as such. Never again labelled "capture".
        "sourceFileModifiedRange": {
            "start": mtime_low.isoformat() if mtime_low else None,
            "end": mtime_high.isoformat() if mtime_high else None,
        },
        "eventTimeRange": _as_range(event_range),
        "captureTimeRange": _as_range(capture_range),
        "timestampSemantics": TIMESTAMP_SEMANTICS,
        "expected": sorted(set(args.expect)),
        "groupsPresent": sorted(present),
        "gitCommit": git_commit(),
        "totals": {
            **totals,
            "files": len(files),
            "sourceBytes": source_bytes,
            "compressedBytes": compressed_bytes,
            "compressionRatio": round(source_bytes / compressed_bytes, 2) if compressed_bytes else None,
        },
        "files": files,
        "exporterSha256": sha256_file(Path(__file__).resolve()),
        # Stated rather than implied: a reader must know these limits before
        # drawing conclusions from the contents.
        "limitations": [
            "Record counts describe what the capture contained, not what the market did.",
            "Gaps, dropped records and truncated lines invalidate completeness claims; see errorRecords/partialLines.",
            "Censored observations are not failures and must not be aggregated as such.",
            "No credential, environment variable or secret is included by construction: every record type is derived from market events.",
            "sourceFileModifiedRange is filesystem metadata and does not describe the capture window.",
        ],
    }
    (destination / "manifest.json").write_text(
        json.dumps(manifest, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    (destination / "SHA256SUMS").write_text(
        "".join(f"{f['sha256']}  {f['name']}\n" for f in files), encoding="utf-8")

    print(json.dumps({
        "sessionDate": session_date,
        "output": str(destination),
        "files": len(files),
        "records": totals["records"],
        "errorRecords": totals["errors"],
        "sourceBytes": source_bytes,
        "compressedBytes": compressed_bytes,
        "compressionRatio": manifest["totals"]["compressionRatio"],
    }))


if __name__ == "__main__":
    main()
