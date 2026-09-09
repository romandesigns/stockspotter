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

Produces:

    exports/session-YYYY-MM-DD/
        manifest.json
        <name>.ndjson.gz ...
        SHA256SUMS
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

SCHEMA_VERSION = 1
CHUNK = 1024 * 1024

# Capture files are grouped by what they contain, so the analysis side can load
# one kind at a time rather than parsing everything to find what it needs.
GROUPS = {
    "discovery": ("discovery-audit", "discovery"),
    "signals": ("live_pending_signals", "live_evaluated_signals"),
    "episodes": ("episodes",),
    "outcomes": ("outcomes", "horizons"),
    "trader": ("auto_trader_journal",),
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


def count_and_compress(source, destination):
    """Streams `source` into gzip, counting records and malformed lines.

    Counts are computed from the bytes actually written, not from a separate
    pass, so a manifest count can never disagree with the file it describes.
    A line that does not parse is counted and still copied verbatim -- dropping
    it would create a silent gap, which is precisely what the capture format's
    own `lost_records` accounting exists to avoid.
    """
    records = 0
    errors = 0
    partial = 0
    with source.open("rb") as raw, gzip.open(destination, "wb", compresslevel=6) as out:
        for line in raw:
            if not line.endswith(b"\n"):
                partial += 1
            stripped = line.strip()
            if not stripped:
                continue
            records += 1
            try:
                json.loads(stripped)
            except json.JSONDecodeError:
                errors += 1
            out.write(line if line.endswith(b"\n") else line + b"\n")
    return records, errors, partial


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
                    stamp = record.get("recorded_at") or record.get("timestamp") or record.get("detectedAt")
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path,
                        help="capture files, or directories of them")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--session-date", default=None)
    args = parser.parse_args()

    paths = sorted({
        p.resolve()
        for item in args.inputs
        for p in (sorted(item.glob("*.jsonl")) if item.is_dir() else [item])
        if p.is_file()
    })
    if not paths:
        raise SystemExit("no capture files found")

    session_date = session_date_of(paths, args.session_date)
    destination = args.output / f"session-{session_date}"
    destination.mkdir(parents=True, exist_ok=True)

    files = []
    totals = {"records": 0, "errors": 0, "partialLines": 0}
    earliest = latest = None

    for path in paths:
        target = destination / f"{path.stem}.ndjson.gz"
        records, errors, partial = count_and_compress(path, target)
        stat = path.stat()
        modified = datetime.fromtimestamp(stat.st_mtime, timezone.utc)
        earliest = min(earliest, modified) if earliest else modified
        latest = max(latest, modified) if latest else modified
        totals["records"] += records
        totals["errors"] += errors
        totals["partialLines"] += partial
        files.append({
            "name": target.name,
            "group": classify(path),
            "sourceName": path.name,
            "records": records,
            "errorRecords": errors,
            "partialLines": partial,
            "sourceBytes": stat.st_size,
            "compressedBytes": target.stat().st_size,
            "sha256": sha256_file(target),
            "sourceSha256": sha256_file(path),
        })

    source_bytes = sum(f["sourceBytes"] for f in files)
    compressed_bytes = sum(f["compressedBytes"] for f in files)
    manifest = {
        "artifact": "stockspotter-research-session",
        "schemaVersion": SCHEMA_VERSION,
        "sessionDate": session_date,
        "captureStartedAt": earliest.isoformat() if earliest else None,
        "captureEndedAt": latest.isoformat() if latest else None,
        "exportedAt": datetime.now(timezone.utc).isoformat(),
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
