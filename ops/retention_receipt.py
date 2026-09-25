"""Build a retention export receipt from an off-box preservation manifest.

A receipt (`<capture dir>/.retention/exports/<date>.json`) is what lets
retention delete a *protected* day. The service accepts it only if every file
of that day on disk matches the receipt's name, byte length and SHA-256 -- so
the hashes written here must come from the **verified copy**, never from
re-hashing the source on the box. A receipt built from the source's own hashes
would prove only that the source equals itself.

This script therefore reads hashes from a preservation MANIFEST.json (the
shape written by the 2026-09-25 evidence preservation: `files[]` entries with
`class`, `src`, `destSize`, `srcSha256`, `destSha256`, `result`, plus an
`alreadyPreservedNotRecopied` map). It refuses any entry whose source and
destination hashes disagree or whose result is not VERIFIED.

It only prints JSON. It never writes into a capture directory and never
touches the VPS; installing the receipt is a separate, deliberate step.

    python ops/retention_receipt.py --manifest MANIFEST.json \
        --store discovery --date 2026-09-21 \
        --destination "H:/wavystack/stockspotter-research/...  (off-box)" \
        --verified-by roman > 2026-09-21.json
"""
import argparse
import datetime
import json
import os
import sys


def fail(message):
    sys.exit(f"retention_receipt: {message}")


def from_files(manifest, store, date):
    entries = []
    for f in manifest.get("files", []):
        if f.get("class") != f"{store}/{date}":
            continue
        name = os.path.basename(f["src"].replace("\\", "/"))
        if f.get("result") != "VERIFIED":
            fail(f"{name}: result is {f.get('result')!r}, not VERIFIED")
        if not f.get("destSha256") or f.get("srcSha256") != f.get("destSha256"):
            fail(f"{name}: source and destination hashes do not agree")
        if int(f["destSize"]) != int(f["expectedSize"]):
            fail(f"{name}: destination size differs from the expected size")
        entries.append({"name": name, "bytes": int(f["destSize"]), "sha256": f["destSha256"]})
    return entries


def from_already_preserved(manifest, store, date):
    """Days preserved by an earlier run carry only per-stream hashes; sizes come
    from the preserved local copy, which is the thing the receipt describes."""
    item = manifest.get("alreadyPreservedNotRecopied", {}).get(f"{store}/{date}")
    if not item:
        return []
    if "VERIFIED" not in item.get("status", ""):
        fail(f"{store}/{date}: already-preserved status is {item.get('status')!r}")
    entries = []
    for stream, sha in item["sha256"].items():
        name = f"{stream}-{date}.ndjson"
        path = os.path.join(item["path"], name)
        try:
            size = os.stat(path).st_size
        except OSError as error:
            fail(f"{name}: cannot stat the preserved copy at {path}: {error}")
        entries.append({"name": name, "bytes": size, "sha256": sha})
    return entries


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--store", required=True, choices=["research", "discovery"])
    ap.add_argument("--date", required=True)
    ap.add_argument("--destination", required=True)
    ap.add_argument("--verified-by", required=True)
    ap.add_argument("--verified-at", help="RFC 3339; defaults to now (UTC)")
    args = ap.parse_args()
    datetime.date.fromisoformat(args.date)

    with open(args.manifest, encoding="utf-8") as fh:
        manifest = json.load(fh)
    files = from_files(manifest, args.store, args.date) + from_already_preserved(
        manifest, args.store, args.date)
    if not files:
        fail(f"the manifest has no {args.store}/{args.date} entries")
    names = [f["name"] for f in files]
    if len(names) != len(set(names)):
        fail("a file appears twice across the manifest's sections")

    verified_at = args.verified_at or datetime.datetime.now(datetime.timezone.utc) \
        .strftime("%Y-%m-%dT%H:%M:%SZ")
    json.dump({
        "schemaVersion": 1,
        "date": args.date,
        "destination": args.destination,
        "verifiedAt": verified_at,
        "verifiedBy": args.verified_by,
        "files": sorted(files, key=lambda f: f["name"]),
    }, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
