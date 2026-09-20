"""Offline full-file reconciliation of an immutable session export, never a replay.

Checks one outcome per recorded ranking anchor, IDs, provenance, horizons,
duplicates, malformed lines and coverage. It does not select or fit a model.
"""
import argparse
import collections
import hashlib
import importlib.util
import json
import pathlib
import sqlite3
import tempfile

spec = importlib.util.spec_from_file_location("observer", pathlib.Path(__file__).with_name("observe-session.py"))
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)


def reconcile(directory, date):
    errors = collections.Counter()
    counts = collections.Counter()
    censoring = collections.Counter()
    hashes = {}
    with tempfile.TemporaryDirectory(prefix="stockspotter-outcomes-") as temp:
        db = sqlite3.connect(str(pathlib.Path(temp) / "joins.sqlite"))
        db.execute("CREATE TABLE anchors (id TEXT, window TEXT, PRIMARY KEY(id,window))")
        db.execute("CREATE TABLE outcomes (id TEXT, window TEXT, PRIMARY KEY(id,window))")
        db.execute("CREATE TABLE episodes (uid TEXT PRIMARY KEY)")
        for stem in ("opportunity-intelligence", "opportunity-outcomes", "episodes"):
            path = directory / f"{stem}-{date}.ndjson"
            if not path.is_file():
                errors[f"missing_{stem}"] += 1
                continue
            before = path.stat()
            digest = hashlib.sha256()
            with path.open("rb") as source:
                for line in source:
                    digest.update(line)
                    try:
                        row = json.loads(line)
                        if not isinstance(row, dict) or not line.endswith(b"\n"):
                            raise ValueError()
                        counts[stem] += 1
                        if stem == "episodes":
                            if not observer.valid_episode(row):
                                errors["invalid_episode_identity"] += 1
                            db.execute("INSERT INTO episodes VALUES (?)", (row.get("episodeUid"),))
                            continue
                        if not observer.valid_id(row) or row.get("sessionDate") != date:
                            errors["invalid_opportunity_identity_or_date"] += 1
                        key = (row["opportunityId"], row["windowId"])
                        if stem == "opportunity-intelligence":
                            if row.get("schemaVersion") != 2 or row.get("versions", {}).get("configFingerprint") != observer.FINGERPRINT:
                                errors["invalid_ranking_provenance"] += 1
                            for field in ("observedHigh", "observedLow", "maxMovePct", "minMovePct", "openingPrice", "openedAt"):
                                if row.get(field) is None:
                                    errors[f"missing_{field}"] += 1
                            db.execute("INSERT INTO anchors VALUES (?,?)", key)
                        else:
                            p = row.get("provenance", {})
                            if p.get("measurementVersion") != "opportunity-outcome-v1" or p.get("opportunitySchema") != 2 or p.get("configFingerprint") != observer.FINGERPRINT:
                                errors["invalid_outcome_provenance"] += 1
                            if sorted(r["horizonSecs"] for r in row["returns"]) != observer.HORIZONS:
                                errors["invalid_horizons"] += 1
                            if not isinstance(row.get("fullyObserved"), bool):
                                errors["missing_fullyObserved"] += 1
                            counts["fullyObserved" if row.get("fullyObserved") else "censored"] += 1
                            censoring.update(str(v) for v in row.get("censorReasons", []))
                            db.execute("INSERT INTO outcomes VALUES (?,?)", key)
                    except sqlite3.IntegrityError:
                        errors[f"duplicate_{stem}"] += 1
                    except (ValueError, TypeError, KeyError, UnicodeDecodeError):
                        errors[f"malformed_{stem}"] += 1
            after = path.stat()
            if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
                errors[f"changing_file_{stem}"] += 1
            hashes[path.name] = digest.hexdigest()
        db.commit()
        for name, left, right in (("missing_outcomes", "anchors", "outcomes"), ("unmatched_outcomes", "outcomes", "anchors")):
            count = db.execute(f"SELECT count(*) FROM {left} l LEFT JOIN {right} r ON l.id=r.id AND l.window=r.window WHERE r.id IS NULL").fetchone()[0]
            if count:
                errors[name] = count
        db.close()
    return {"status": "FAIL" if errors else "PASS_FILE_RECONCILIATION" if counts["opportunity-outcomes"] else "PENDING",
            "date": date, "counts": dict(counts), "errors": dict(errors), "censoring": dict(censoring), "sha256": hashes,
            "limitations": ["Requires a verified immutable export plus stable zero-outstanding health; file checks alone do not qualify a session.",
                            "Score-decile and membership-stratum analysis needs the existing temporal-containment membership contract; no ID-based episode join is inferred.",
                            "RiskQuality field presence is checked; formula replay and the recorded consumedFromLow/dmfs300-or-dcons300 hypothesis require research review, without tuning."]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=pathlib.Path, required=True)
    parser.add_argument("--date", required=True)
    args = parser.parse_args()
    observer.dt.date.fromisoformat(args.date)
    result = reconcile(args.directory, args.date)
    print(json.dumps(result, indent=2))
    raise SystemExit(1 if result["status"] == "FAIL" else 2 if result["status"] == "PENDING" else 0)
