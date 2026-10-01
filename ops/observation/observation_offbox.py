#!/usr/bin/env python3
"""Off-box copy verification for archived consumer-received observer runs.

An archived run (`observation_archive.py retain`) is copied to a second
machine by whatever means the operator uses. This tool does not copy, move,
delete or modify evidence on either machine; it proves the copy is the archive:

  capture host    attest-source  read-only: verifies the archived run and
                                 prints its source attestation
  (the operator copies the attested files to
   <campaign_root>/sessions/<session>/observation/<runId>/)
  second machine  verify-copy    hashes and decompresses the copy independently
                                 and writes the off-box receipt beside (not
                                 inside) the copied evidence
  second machine  reverify       read-only: re-hashes the copy at a later time

Neither machine can see the other, so each side attests only what it can check
itself, and every later step is bound to the exact bytes of the earlier one by
SHA-256.

What is written: `verify-copy` creates two new files under
<session_dir>/receipts/ -- a copy of the attestation and the receipt. Both are
created exclusively, so an existing file is never replaced. Nothing else is
written anywhere, and no command removes anything.

Names taken from a receipt or an attestation are untrusted: each must be a bare
file name naming a regular file, not a link, directly inside the directory it
is looked up in. Decompression is streamed and capped (see
`observation_archive.py`).

Commands:
  attest-source <run_dir>
      Capture host, read-only. Verifies the archived run and prints its source
      attestation. The uncompressed sources may still be beside the artifacts
      (then they must match the receipt byte for byte) or may all be absent.
  verify-copy <dest_run_dir> <source_attestation.json> <session> <implementation_sha> <preregistration_sha256>
      Second machine. Independently verifies the copy and writes the off-box receipt to
      <session_dir>/receipts/<runId>.offbox-export-receipt.json. A re-run returns the standing receipt.
  reverify <offbox_receipt.json>
      Second machine, read-only. Re-hashes the destination now; prints a reverify attestation.

Standard library only; stdout is one JSON object; exit 0 = done, 1 = refused, 2 = usage.
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
SESSION_RE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")


def _sha(path):
    return archive._sha_and_size(path)


def _digest(b):
    return hashlib.sha256(b).hexdigest()


def _now():
    return datetime.datetime.now(datetime.timezone.utc)


def _iso(t):
    return t.isoformat(timespec="seconds")


def _need(obj, key, what):
    if not isinstance(obj, dict) or key not in obj:
        raise Refusal(f"{what} has no {key}")
    return obj[key]


def _run_id(value, what):
    """A run id read from a document, before it is used to build a path."""
    if not isinstance(value, str) or not archive.RUN_DIR_RE.match(value):
        raise Refusal(f"{what} names no valid run")
    return value


def archived_run(run_dir):
    """Proves a run is archived on this host, or raises Refusal. Reads only.

    Archived: an unambiguous run directory that is not a link, holding the
    archive receipt, the manifest and exactly the receipt's `.gz` artifacts,
    every one a regular file that hashes to the receipt and decompresses to the
    recorded source bytes. The uncompressed sources are either all present and
    byte-identical to the receipt, or all absent. A source-removal record
    (`archive-deletion.json`, which nothing in this repository writes) may be
    present only with the sources absent, and must then be complete and bound
    to this receipt; it is attested like the other side files.
    """
    run_dir = os.path.normpath(run_dir)
    run_id = os.path.basename(run_dir)
    if not os.path.isdir(run_dir) or archive._is_link(run_dir) or not archive.RUN_DIR_RE.match(run_id):
        raise Refusal(f"ambiguous run directory {run_id!r}")
    names = sorted(os.listdir(run_dir))
    if archive.RETENTION_RECEIPT not in names:
        raise Refusal("not archived: no archive receipt (active or unarchived run)")
    rbytes = archive._read_small(archive._evidence_file(run_dir, archive.RETENTION_RECEIPT), "archive receipt")
    receipt = archive._parse_object(rbytes, "archive receipt")
    if receipt.get("schema") != "observation-archive-receipt-v1" or receipt.get("runId") != run_id:
        raise Refusal("archive receipt mismatch")
    pairs = archive.receipt_pairs(receipt)
    receipt_sha = _digest(rbytes)
    source_names = sorted(s["file"] for s, _ in pairs)
    present = [n for n in names if archive.SOURCE_RE.match(n)]
    side = [archive.RETENTION_RECEIPT, archive.MANIFEST]
    removal_sha = None
    if archive.DELETION_RECORD in names:
        dbytes = archive._read_small(archive._evidence_file(run_dir, archive.DELETION_RECORD), "source-removal record")
        record = archive._parse_object(dbytes, "source-removal record")
        removed = record.get("removed")
        if (record.get("state") != "complete" or record.get("receiptSha256") != receipt_sha
                or not isinstance(removed, list) or sorted(map(str, removed)) != source_names):
            raise Refusal("source-removal record incomplete or not bound to this archive receipt")
        if present:
            raise Refusal("source-removal record present but uncompressed sources exist")
        side.append(archive.DELETION_RECORD)
        removal_sha = _digest(dbytes)
    if present and present != source_names:
        raise Refusal(f"uncompressed sources present are not exactly the receipt's sources: {present}")
    expected = sorted([c["file"] for _, c in pairs] + side + present)
    if names != expected:
        extra = sorted(set(names) - set(expected))
        missing = sorted(set(expected) - set(names))
        raise Refusal(f"run directory is not exactly the archived evidence (extra {extra}, missing {missing})")
    artifacts = []
    for s, c in pairs:
        p = archive._evidence_file(run_dir, c["file"])
        sha, size = _sha(p)
        if (sha, size) != (c.get("sha256"), c.get("bytes")):
            raise Refusal(f"compressed artifact changed: {c['file']}")
        try:
            dsha, dsize = archive._gunzip_sha(p)
        except archive.GUNZIP_ERRORS as e:
            raise Refusal(f"corrupt compressed artifact {c['file']}: {e}")
        if (dsha, dsize) != (s.get("sha256"), s.get("bytes")):
            raise Refusal(f"gzip mismatch: {c['file']} does not reproduce its recorded source")
        if present and _sha(archive._evidence_file(run_dir, s["file"])) != (dsha, dsize):
            raise Refusal(f"uncompressed source differs from the receipt: {s['file']}")
        artifacts.append({"file": c["file"], "bytes": size, "sha256": sha, "source": s["file"],
                          "sourceSha256": dsha, "sourceBytes": dsize})
    for n in side:
        sha, size = _sha(archive._evidence_file(run_dir, n))
        artifacts.append({"file": n, "bytes": size, "sha256": sha})
    artifacts.sort(key=lambda a: a["file"])
    return {"runId": run_id, "receipt": receipt, "receiptSha256": receipt_sha, "deletionRecordSha256": removal_sha,
            "rawSourcesPresent": bool(present), "artifacts": artifacts}


def attest_source(run_dir):
    info = archived_run(run_dir)
    r = info["receipt"]
    return {
        "schema": ATTESTATION_SCHEMA,
        "host": socket.gethostname(),
        "sourcePath": os.path.abspath(run_dir),
        "runId": info["runId"],
        "namespace": r.get("namespace"),
        "implementationSha": r.get("implementationSha"),
        "preregistrationSha256": r.get("preregistrationSha256"),
        "runStartedAt": (r.get("runStart") or {}).get("startedAt"),
        "runEndedAt": (r.get("runEnd") or {}).get("endedAt"),
        "archiveReceiptSha256": info["receiptSha256"],
        # null unless the run carries a source-removal record.
        "deletionRecordSha256": info["deletionRecordSha256"],
        "artifacts": info["artifacts"],
        "verification": {"archivedRun": True, "compressedHashesMatchReceipt": True,
                         "decompressionReproducesSources": True,
                         "rawSourcesPresent": info["rawSourcesPresent"],
                         "rawSourcesMatchReceipt": True if info["rawSourcesPresent"] else None},
        "attestedAt": _iso(_now()),
    }


def _verify_destination(dest, attestation):
    """Independently hashes the destination against the attestation. Returns per-artifact rows. Reads only."""
    if not os.path.isdir(dest) or archive._is_link(dest):
        raise Refusal("missing destination")
    names = sorted(os.listdir(dest))
    listed = attestation.get("artifacts")
    if not isinstance(listed, list) or not listed or not all(isinstance(a, dict) for a in listed):
        raise Refusal("attestation lists no artifacts")
    expected = sorted(archive._bare_name(a.get("file")) for a in listed)
    if len(set(expected)) != len(expected):
        raise Refusal("attestation lists an artifact twice")
    if names != expected:
        raise Refusal(f"destination is not exactly the attested evidence (extra {sorted(set(names) - set(expected))}, "
                      f"missing {sorted(set(expected) - set(names))})")
    rows = []
    for a in listed:
        p = archive._evidence_file(dest, a["file"])
        sha, size = _sha(p)
        if (sha, size) != (a.get("sha256"), a.get("bytes")):
            raise Refusal(f"destination artifact differs from source: {a['file']}")
        row = {"file": a["file"], "bytes": size, "sourceSha256": a["sha256"], "destinationSha256": sha}
        if "source" in a:
            try:
                dsha, dsize = archive._gunzip_sha(p)
            except archive.GUNZIP_ERRORS as e:
                raise Refusal(f"corrupt destination artifact {a['file']}: {e}")
            if (dsha, dsize) != (a.get("sourceSha256"), a.get("sourceBytes")):
                raise Refusal(f"gzip mismatch at destination: {a['file']}")
            row.update({"decompressesTo": a["source"], "decompressedSha256": dsha, "decompressedBytes": dsize})
        rows.append(row)
    # The copied archive receipt must be the attested one and must agree with the artifacts.
    rbytes = archive._read_small(archive._evidence_file(dest, archive.RETENTION_RECEIPT), "archive receipt")
    if _digest(rbytes) != attestation.get("archiveReceiptSha256"):
        raise Refusal("archive receipt at destination is not the attested one")
    removal_sha = attestation.get("deletionRecordSha256")
    if archive.DELETION_RECORD in names:
        if _sha(archive._evidence_file(dest, archive.DELETION_RECORD))[0] != removal_sha:
            raise Refusal("source-removal record at destination is not the attested one")
    elif removal_sha is not None:
        raise Refusal("attested source-removal record is missing at the destination")
    receipt = archive._parse_object(rbytes, "archive receipt")
    for _, c in archive.receipt_pairs(receipt):
        row = next((x for x in rows if x["file"] == c["file"]), None)
        if not row or row["destinationSha256"] != c.get("sha256") or "decompressedSha256" not in row:
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
    abytes = archive._read_small(attestation_path, "source attestation")
    att = archive._parse_object(abytes, "source attestation")
    if att.get("schema") != ATTESTATION_SCHEMA:
        raise Refusal("not a source attestation")
    run_id = _run_id(att.get("runId"), "source attestation")
    if os.path.basename(dest) != run_id:
        raise Refusal(f"wrong run: destination {os.path.basename(dest)!r} is not attested run {run_id!r}")
    if not SESSION_RE.match(session):
        raise Refusal("session is not YYYY-MM-DD")
    parts = dest.replace("\\", "/").split("/")
    if len(parts) < 4 or parts[-2] != "observation" or parts[-3] != session:
        raise Refusal("destination is not <...>/sessions/<session>/observation/<runId>")
    if (att.get("implementationSha"), att.get("preregistrationSha256")) != (impl, prereg):
        raise Refusal("wrong identities: the run was not captured under the expected implementation/preregistration")
    started = att.get("runStartedAt")
    if not isinstance(started, str):
        raise Refusal("source attestation has no runStartedAt")
    try:
        belongs_to = str(_session_of(started))
    except ValueError:
        raise Refusal("source attestation runStartedAt is not a timestamp")
    if belongs_to != session:
        raise Refusal(f"run belongs to session {belongs_to}, not {session}")
    rp = receipt_path(dest, run_id)
    receipts_dir = os.path.dirname(rp)
    rows, receipt = _verify_destination(dest, att)
    if (receipt.get("implementationSha"), receipt.get("preregistrationSha256"), receipt.get("runId")) != (impl, prereg, run_id):
        raise Refusal("wrong identities in the copied archive receipt")
    att_sha = _digest(abytes)
    if os.path.lexists(rp):
        # A re-run changes nothing: the standing receipt is returned if it is for this attestation.
        old_bytes = archive._read_small(archive._evidence_file(receipts_dir, os.path.basename(rp)), "off-box receipt")
        old = archive._parse_object(old_bytes, "off-box receipt")
        if old.get("sourceAttestationSha256") != att_sha:
            raise Refusal("an off-box receipt for this run already exists for a different attestation")
        return old, _digest(old_bytes), True
    os.makedirs(receipts_dir, exist_ok=True)
    # Keep the attestation beside the receipt: the receipt binds its exact bytes.
    att_copy = f"{run_id}.source-attestation.json"
    if os.path.lexists(os.path.join(receipts_dir, att_copy)):
        # Left by an attempt that stopped before its receipt. It stands only if it is these bytes.
        if archive._read_small(archive._evidence_file(receipts_dir, att_copy), "source attestation copy") != abytes:
            raise Refusal("a different source attestation is already kept for this run")
    else:
        archive._create_file_durably(os.path.join(receipts_dir, att_copy), abytes)
    rec = {
        "schema": RECEIPT_SCHEMA,
        "session": session,
        "runId": run_id,
        "implementationSha": impl,
        "preregistrationSha256": prereg,
        "sourceHost": att.get("host"),
        "sourcePath": att.get("sourcePath"),
        "offboxDestinationPath": dest,
        "sourceAttestationSha256": att_sha,
        "archiveReceiptSha256": att["archiveReceiptSha256"],
        "deletionRecordSha256": att.get("deletionRecordSha256"),
        "artifacts": rows,
        "decompressionVerified": all("decompressedSha256" in r for r in rows if r["file"].endswith(".gz")),
        "exportedAt": _iso(_now()),
        "verification": "PASS",
    }
    archive._create_json_durably(rp, rec)
    return rec, _digest(archive._read_small(rp, "off-box receipt")), False


def reverify(receipt_file):
    receipt_file = os.path.abspath(receipt_file)
    rbytes = archive._read_small(receipt_file, "off-box receipt")
    rec = archive._parse_object(rbytes, "off-box receipt")
    if rec.get("schema") != RECEIPT_SCHEMA:
        raise Refusal("not an off-box export receipt")
    run_id = _run_id(rec.get("runId"), "off-box receipt")
    dest = rec.get("offboxDestinationPath")
    # The receipt records an absolute destination. It is followed only when this
    # receipt sits exactly where verify-copy writes the receipt for that
    # destination, so a receipt cannot point the tool at an unrelated directory.
    if (not isinstance(dest, str) or not os.path.isabs(dest) or os.path.basename(os.path.normpath(dest)) != run_id
            or os.path.normcase(os.path.realpath(receipt_path(dest, run_id))) != os.path.normcase(os.path.realpath(receipt_file))):
        raise Refusal("off-box receipt is not beside the destination it records")
    att = {"artifacts": [], "archiveReceiptSha256": _need(rec, "archiveReceiptSha256", "off-box receipt"),
           "deletionRecordSha256": rec.get("deletionRecordSha256")}
    rows = _need(rec, "artifacts", "off-box receipt")
    if not isinstance(rows, list):
        raise Refusal("off-box receipt artifacts are malformed")
    for r in rows:
        a = {"file": _need(r, "file", "off-box receipt artifact"), "bytes": _need(r, "bytes", "off-box receipt artifact"),
             "sha256": _need(r, "destinationSha256", "off-box receipt artifact")}
        if "decompressedSha256" in r:
            a.update({"source": _need(r, "decompressesTo", "off-box receipt artifact"), "sourceSha256": r["decompressedSha256"],
                      "sourceBytes": _need(r, "decompressedBytes", "off-box receipt artifact")})
        att["artifacts"].append(a)
    _verify_destination(os.path.normpath(dest), att)
    return {"schema": REVERIFY_SCHEMA, "runId": run_id, "offboxReceiptSha256": _digest(rbytes),
            "offboxDestinationPath": dest, "destinationIntact": True,
            "artifacts": len(rows), "reverifiedAt": _iso(_now())}


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
        else:
            print(__doc__)
            return 2
    except Refusal as r:
        print(json.dumps({"refused": str(r)}))
        return 1
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
