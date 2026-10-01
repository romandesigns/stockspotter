#!/usr/bin/env python3
"""Off-box retention for archived consumer-received observer runs (GPT Phase-B
support acceptance, 2026-10-01).

Lifecycle, after `observation_archive.py retain` + `delete-source`:

  VPS: verified compressed run --attest-source--> source attestation (read-only)
  copy the run's retained evidence to H:\\wavystack\\stockspotter-research\\step4-campaign\\
       sessions\\<session>\\observation\\<runId>\\
  workstation: verify-copy --> independent hashes + decompression --> off-box receipt
  (later, separately authorized) workstation: reverify --> VPS: delete-archive

The VPS cannot see the destination and the workstation cannot see the VPS, so
each side attests only what it can check itself, and every later step is bound
to the exact bytes of the earlier one by SHA-256. `delete-archive` deletes only
the VPS's verified `.gz` artifacts, keeps the receipts as a tombstone, and is
NOT authorized for use yet (GPT: first real run is copied, verified and
reported before any VPS compressed-archive deletion).

Commands:
  attest-source <run_dir>
      VPS, read-only. Verifies the archived run and prints its source attestation.
  verify-copy <dest_run_dir> <source_attestation.json> <session> <implementation_sha> <preregistration_sha256>
      Workstation. Independently verifies the copy and writes the off-box receipt to
      <session_dir>/receipts/<runId>.offbox-export-receipt.json. Idempotent.
  reverify <offbox_receipt.json>
      Workstation, read-only. Re-hashes the destination now; prints a reverify attestation.
  delete-archive <run_dir> <offbox_receipt.json> <offbox_receipt_sha256> <reverify.json>
      VPS, root. NOT AUTHORIZED in the current phase. Deletes exactly the verified
      .gz artifacts after re-verifying everything; writes offbox-deletion.json.

Standard library only; stdout is one JSON object; exit 0 = done, 1 = refused.
"""
import datetime
import hashlib
import importlib.util
import json
import os
import re
import socket
import sys

_spec = importlib.util.spec_from_file_location(
    "observation_archive", os.path.join(os.path.dirname(os.path.abspath(__file__)), "observation_archive.py"))
archive = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(archive)

Refusal = archive.Refusal
ATTESTATION_SCHEMA = "observation-source-attestation-v1"
RECEIPT_SCHEMA = "observation-offbox-export-receipt-v1"
REVERIFY_SCHEMA = "observation-offbox-reverify-v1"
OFFBOX_DELETION = "offbox-deletion.json"
OFFBOX_DELETION_SCHEMA = "observation-offbox-deletion-v1"
# The evidence a fully archived run keeps on the VPS, and therefore copies off.
SIDE_FILES = (archive.RETENTION_RECEIPT, archive.MANIFEST, archive.DELETION_RECORD)
REVERIFY_MAX_AGE = datetime.timedelta(hours=24)
SESSION_RE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")


def _sha(path):
    return archive._sha_and_size(path)


def _file_sha(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def _now():
    return datetime.datetime.now(datetime.timezone.utc)


def _iso(t):
    return t.isoformat(timespec="seconds")


def archived_run(run_dir):
    """Proves a run is fully archived on this host, or raises Refusal. Reads only.

    Fully archived: the archive receipt and a COMPLETE deletion record exist, the
    uncompressed sources are gone, the directory holds exactly the receipt's .gz
    artifacts plus receipt, manifest and deletion record, every .gz hashes to the
    receipt and decompresses to the recorded source bytes.
    """
    run_dir = os.path.normpath(run_dir)
    run_id = os.path.basename(run_dir)
    if not os.path.isdir(run_dir) or not archive.RUN_DIR_RE.match(run_id):
        raise Refusal(f"ambiguous run directory {run_id!r}")
    names = sorted(os.listdir(run_dir))
    rpath = os.path.join(run_dir, archive.RETENTION_RECEIPT)
    if not os.path.isfile(rpath):
        raise Refusal("not archived: no archive receipt (active or unarchived run)")
    with open(rpath, "rb") as f:
        rbytes = f.read()
    receipt = json.loads(rbytes)
    if receipt.get("schema") != "observation-archive-receipt-v1" or receipt.get("runId") != run_id:
        raise Refusal("archive receipt mismatch")
    dpath = os.path.join(run_dir, archive.DELETION_RECORD)
    if not os.path.isfile(dpath):
        raise Refusal("raw sources not removed: no deletion record")
    with open(dpath, "rb") as f:
        dbytes = f.read()
    deletion = json.loads(dbytes)
    receipt_sha = hashlib.sha256(rbytes).hexdigest()
    if (deletion.get("state") != "complete" or deletion.get("receiptSha256") != receipt_sha
            or sorted(deletion.get("removed") or []) != sorted(s["file"] for s in receipt["sources"])):
        raise Refusal("deletion record incomplete or not bound to this archive receipt")
    expected = sorted([c["file"] for c in receipt["compressed"]] + list(SIDE_FILES))
    if names != expected:
        extra = sorted(set(names) - set(expected))
        missing = sorted(set(expected) - set(names))
        raise Refusal(f"run directory is not exactly the archived evidence (extra {extra}, missing {missing})")
    by_source = {s["file"]: s for s in receipt["sources"]}
    artifacts = []
    for c in receipt["compressed"]:
        p = os.path.join(run_dir, c["file"])
        sha, size = _sha(p)
        if (sha, size) != (c["sha256"], c["bytes"]):
            raise Refusal(f"compressed artifact changed: {c['file']}")
        s = by_source.get(c["source"])
        try:
            dsha, dsize = archive._gunzip_sha(p)
        except (OSError, EOFError, ValueError) as e:
            raise Refusal(f"corrupt compressed artifact {c['file']}: {e}")
        if not s or (dsha, dsize) != (s["sha256"], s["bytes"]):
            raise Refusal(f"gzip mismatch: {c['file']} does not reproduce its recorded source")
        artifacts.append({"file": c["file"], "bytes": size, "sha256": sha, "source": c["source"],
                          "sourceSha256": s["sha256"], "sourceBytes": s["bytes"]})
    for n in SIDE_FILES:
        sha, size = _sha(os.path.join(run_dir, n))
        artifacts.append({"file": n, "bytes": size, "sha256": sha})
    artifacts.sort(key=lambda a: a["file"])
    return {"runId": run_id, "receipt": receipt, "receiptSha256": receipt_sha,
            "deletionRecordSha256": hashlib.sha256(dbytes).hexdigest(), "artifacts": artifacts}


def attest_source(run_dir):
    info = archived_run(run_dir)
    r = info["receipt"]
    return {
        "schema": ATTESTATION_SCHEMA,
        "host": socket.gethostname(),
        "sourcePath": os.path.abspath(run_dir),
        "runId": info["runId"],
        "namespace": r["namespace"],
        "implementationSha": r["implementationSha"],
        "preregistrationSha256": r["preregistrationSha256"],
        "runStartedAt": r["runStart"].get("startedAt"),
        "runEndedAt": r["runEnd"].get("endedAt"),
        "archiveReceiptSha256": info["receiptSha256"],
        "deletionRecordSha256": info["deletionRecordSha256"],
        "artifacts": info["artifacts"],
        "verification": {"archivedRun": True, "compressedHashesMatchReceipt": True,
                         "decompressionReproducesSources": True, "rawSourcesRemoved": True},
        "attestedAt": _iso(_now()),
    }


def _verify_destination(dest, attestation):
    """Independently hashes the destination against the attestation. Returns per-artifact rows."""
    names = sorted(os.listdir(dest)) if os.path.isdir(dest) else None
    if names is None:
        raise Refusal("missing destination")
    expected = sorted(a["file"] for a in attestation["artifacts"])
    if names != expected:
        raise Refusal(f"destination is not exactly the attested evidence (extra {sorted(set(names) - set(expected))}, "
                      f"missing {sorted(set(expected) - set(names))})")
    rows = []
    for a in attestation["artifacts"]:
        sha, size = _sha(os.path.join(dest, a["file"]))
        if (sha, size) != (a["sha256"], a["bytes"]):
            raise Refusal(f"destination artifact differs from source: {a['file']}")
        row = {"file": a["file"], "bytes": size, "sourceSha256": a["sha256"], "destinationSha256": sha}
        if "source" in a:
            try:
                dsha, dsize = archive._gunzip_sha(os.path.join(dest, a["file"]))
            except (OSError, EOFError, ValueError) as e:
                raise Refusal(f"corrupt destination artifact {a['file']}: {e}")
            if (dsha, dsize) != (a["sourceSha256"], a["sourceBytes"]):
                raise Refusal(f"gzip mismatch at destination: {a['file']}")
            row.update({"decompressesTo": a["source"], "decompressedSha256": dsha, "decompressedBytes": dsize})
        rows.append(row)
    # The copied archive receipt must be the attested one and must agree with the artifacts.
    with open(os.path.join(dest, archive.RETENTION_RECEIPT), "rb") as f:
        rbytes = f.read()
    if hashlib.sha256(rbytes).hexdigest() != attestation["archiveReceiptSha256"]:
        raise Refusal("archive receipt at destination is not the attested one")
    if _file_sha(os.path.join(dest, archive.DELETION_RECORD)) != attestation["deletionRecordSha256"]:
        raise Refusal("deletion record at destination is not the attested one")
    receipt = json.loads(rbytes)
    for c in receipt["compressed"]:
        row = next((x for x in rows if x["file"] == c["file"]), None)
        if not row or row["destinationSha256"] != c["sha256"]:
            raise Refusal(f"archive receipt disagrees with destination for {c['file']}")
    return rows, receipt


def _session_of(started_at):
    """observation::step4_session_of: the market day whose 20:10 ET rollover ends the run's session."""
    from zoneinfo import ZoneInfo
    ny = ZoneInfo("America/New_York")
    t = datetime.datetime.fromisoformat(re.sub(r"(\.\d{6})\d*", r"\1", started_at).replace("Z", "+00:00"))
    local = t.astimezone(ny)
    day = (local - datetime.timedelta(hours=4)).date()

    def roll(d):
        return datetime.datetime(d.year, d.month, d.day, 20, 10, tzinfo=ny)
    return day if roll(day) > local else day + datetime.timedelta(days=1)


def receipt_path(dest_run_dir, run_id):
    session_dir = os.path.dirname(os.path.dirname(os.path.abspath(dest_run_dir)))
    return os.path.join(session_dir, "receipts", f"{run_id}.offbox-export-receipt.json")


def verify_copy(dest, attestation_path, session, impl, prereg):
    dest = os.path.normpath(os.path.abspath(dest))
    with open(attestation_path, "rb") as f:
        abytes = f.read()
    att = json.loads(abytes)
    if att.get("schema") != ATTESTATION_SCHEMA:
        raise Refusal("not a source attestation")
    run_id = att["runId"]
    if os.path.basename(dest) != run_id:
        raise Refusal(f"wrong run: destination {os.path.basename(dest)!r} is not attested run {run_id!r}")
    if not SESSION_RE.match(session):
        raise Refusal("session is not YYYY-MM-DD")
    parts = dest.replace("\\", "/").split("/")
    if len(parts) < 4 or parts[-2] != "observation" or parts[-3] != session:
        raise Refusal("destination is not <...>/sessions/<session>/observation/<runId>")
    if (att["implementationSha"], att["preregistrationSha256"]) != (impl, prereg):
        raise Refusal("wrong identities: the run was not captured under the expected implementation/preregistration")
    if str(_session_of(att["runStartedAt"])) != session:
        raise Refusal(f"run belongs to session {_session_of(att['runStartedAt'])}, not {session}")
    rp = receipt_path(dest, run_id)
    rows, receipt = _verify_destination(dest, att)
    if (receipt["implementationSha"], receipt["preregistrationSha256"], receipt["runId"]) != (impl, prereg, run_id):
        raise Refusal("wrong identities in the copied archive receipt")
    if os.path.exists(rp):
        # Idempotent: an existing receipt stands if it is for this attestation and still verifies.
        with open(rp, "rb") as f:
            old_bytes = f.read()
        old = json.loads(old_bytes)
        if old.get("sourceAttestationSha256") != hashlib.sha256(abytes).hexdigest():
            raise Refusal("an off-box receipt for this run already exists for a different attestation")
        return old, hashlib.sha256(old_bytes).hexdigest(), True
    os.makedirs(os.path.dirname(rp), exist_ok=True)
    # Keep the attestation beside the receipt: the receipt binds its exact bytes.
    att_copy = os.path.join(os.path.dirname(rp), f"{run_id}.source-attestation.json")
    with open(att_copy, "wb") as f:
        f.write(abytes)
        f.flush()
        os.fsync(f.fileno())
    rec = {
        "schema": RECEIPT_SCHEMA,
        "session": session,
        "runId": run_id,
        "implementationSha": impl,
        "preregistrationSha256": prereg,
        "vpsHost": att["host"],
        "vpsSourcePath": att["sourcePath"],
        "offboxDestinationPath": dest,
        "sourceAttestationSha256": hashlib.sha256(abytes).hexdigest(),
        "archiveReceiptSha256": att["archiveReceiptSha256"],
        "deletionRecordSha256": att["deletionRecordSha256"],
        "artifacts": rows,
        "decompressionVerified": all("decompressedSha256" in r for r in rows if r["file"].endswith(".gz")),
        "exportedAt": _iso(_now()),
        "verification": "PASS",
        "vpsArchiveDeletion": "eligible only under a separate explicit authorization (delete-archive)",
    }
    archive._write_json_durably(rp, rec)
    with open(rp, "rb") as f:
        return rec, hashlib.sha256(f.read()).hexdigest(), False


def reverify(receipt_file):
    with open(receipt_file, "rb") as f:
        rbytes = f.read()
    rec = json.loads(rbytes)
    if rec.get("schema") != RECEIPT_SCHEMA:
        raise Refusal("not an off-box export receipt")
    att = {"artifacts": [], "archiveReceiptSha256": rec["archiveReceiptSha256"], "deletionRecordSha256": rec["deletionRecordSha256"]}
    for r in rec["artifacts"]:
        a = {"file": r["file"], "bytes": r["bytes"], "sha256": r["destinationSha256"]}
        if "decompressedSha256" in r:
            a.update({"source": r["decompressesTo"], "sourceSha256": r["decompressedSha256"], "sourceBytes": r["decompressedBytes"]})
        att["artifacts"].append(a)
    _verify_destination(rec["offboxDestinationPath"], att)
    return {"schema": REVERIFY_SCHEMA, "runId": rec["runId"], "offboxReceiptSha256": hashlib.sha256(rbytes).hexdigest(),
            "offboxDestinationPath": rec["offboxDestinationPath"], "destinationIntact": True,
            "artifacts": len(rec["artifacts"]), "reverifiedAt": _iso(_now())}


def delete_archive(run_dir, receipt_file, receipt_sha, reverify_file, now=None):
    """VPS, root. NOT AUTHORIZED in the current phase. Deletes exactly the verified .gz artifacts."""
    now = now or _now()
    run_dir = os.path.normpath(run_dir)
    with open(receipt_file, "rb") as f:
        rbytes = f.read()
    if hashlib.sha256(rbytes).hexdigest() != receipt_sha:
        raise Refusal("off-box receipt does not match the authorized SHA-256")
    rec = json.loads(rbytes)
    if rec.get("schema") != RECEIPT_SCHEMA or rec.get("verification") != "PASS":
        raise Refusal("incomplete export: not a passing off-box export receipt")
    if os.path.exists(os.path.join(run_dir, OFFBOX_DELETION)):
        raise Refusal("an off-box deletion record already exists; inspect it, do not re-run")
    info = archived_run(run_dir)  # active / unarchived / unknown files / changed .gz all refuse here
    if (rec["runId"], rec["vpsSourcePath"]) != (info["runId"], os.path.abspath(run_dir)):
        raise Refusal("off-box receipt is for a different run or path")
    if (rec["archiveReceiptSha256"], rec["deletionRecordSha256"]) != (info["receiptSha256"], info["deletionRecordSha256"]):
        raise Refusal("archive receipt or deletion record changed since export")
    with open(reverify_file, encoding="utf-8") as f:
        rv = json.load(f)
    if rv.get("schema") != REVERIFY_SCHEMA or rv.get("offboxReceiptSha256") != receipt_sha or rv.get("destinationIntact") is not True:
        raise Refusal("no intact destination reverification bound to this receipt")
    age = now - datetime.datetime.fromisoformat(rv["reverifiedAt"])
    if not datetime.timedelta(0) <= age <= REVERIFY_MAX_AGE:
        raise Refusal("destination reverification is stale; re-run reverify")
    exported = {r["file"]: r for r in rec["artifacts"]}
    deletion = []
    for a in info["artifacts"]:
        r = exported.get(a["file"])
        if not r or (r["sourceSha256"], r["destinationSha256"], r["bytes"]) != (a["sha256"], a["sha256"], a["bytes"]):
            raise Refusal(f"VPS artifact {a['file']} is not the one exported")
        if "source" in a:
            deletion.append(a)
    if sorted(exported) != sorted(a["file"] for a in info["artifacts"]):
        raise Refusal("ambiguous deletion set: export and VPS evidence differ")
    record = {
        "schema": OFFBOX_DELETION_SCHEMA,
        "runId": info["runId"],
        "offboxReceiptSha256": receipt_sha,
        "offboxDestinationPath": rec["offboxDestinationPath"],
        "authorizedSet": [{"file": a["file"], "bytes": a["bytes"], "sha256": a["sha256"]} for a in deletion],
        "state": "in-progress",
        "startedAt": _iso(now),
    }
    dpath = os.path.join(run_dir, OFFBOX_DELETION)
    archive._write_json_durably(dpath, record)
    for a in deletion:
        os.remove(os.path.join(run_dir, a["file"]))
    archive._fsync_dir(run_dir)
    remaining = sorted(os.listdir(run_dir))
    record.update({"state": "complete", "completedAt": _iso(_now()), "removed": [a["file"] for a in deletion],
                   "remaining": remaining,
                   "tombstoneKept": all(n in remaining for n in SIDE_FILES)})
    archive._write_json_durably(dpath, record)
    return record


def main(argv):
    try:
        if len(argv) == 3 and argv[1] == "attest-source":
            out = attest_source(argv[2])
        elif len(argv) == 7 and argv[1] == "verify-copy":
            rec, sha, existed = verify_copy(*argv[2:7])
            out = {"offboxReceiptSha256": sha, "alreadyExported": existed, "receipt": receipt_path(argv[2], rec["runId"]),
                   "verification": rec["verification"]}
        elif len(argv) == 3 and argv[1] == "reverify":
            out = reverify(argv[2])
        elif len(argv) == 6 and argv[1] == "delete-archive":
            out = delete_archive(*argv[2:6])
        else:
            print(__doc__)
            return 2
    except Refusal as r:
        print(json.dumps({"refused": str(r), "deleted": []}))
        return 1
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
