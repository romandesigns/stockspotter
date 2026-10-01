#!/usr/bin/env python3
"""Post-close compression, verification and preregistration identity for
consumer-received observation captures (Step 4B-preflight §12-§14).

Runs as its own process, never on the market-data consumer thread. Standard
library only (gzip, hashlib, json), so it runs unchanged on the VPS host's
python3 and on the archive machine.

Contract for every file it compresses (§12):

  1. the uncompressed file is CLOSED -- its last line is a terminated
     `file_close` record. An active (unclosed) file is never touched;
  2. SHA-256 and size of the uncompressed bytes are computed;
  3. those are recorded in the run's manifest entry *before* compression;
  4. the file is compressed to `<name>.gz.part`, fsynced;
  5. the compressed file is decompressed and re-hashed;
  6. only if that SHA equals the source SHA is `.part` renamed to `.gz` and
     the entry marked `verified` -- the only state in which it is exportable.

The uncompressed source is never deleted by this tool.

Commands:
  compress <run_dir>          compress every closed, not-yet-verified file
  verify   <run_dir>          re-verify every manifest entry (exit 1 on any failure)
  prereg-sha <artifact.json>  RFC 8785 canonical SHA-256 of a preregistration
  export <run_dir> <dest_root> <certificate.json> <session> <prereg_sha> <impl_sha>
                              copy a compressed, verified run off-box + receipt
                              (exit 1 unless deletion-eligible; deletes nothing)
  verify-export <dest_dir>    re-check an export against its receipt
  retain <run_dir>            archive ONE closed run: strict closure check, compress,
                              verify, write archive-receipt.json (deletes nothing)
  delete-source <run_dir> <receipt_sha256>
                              delete exactly the receipt's uncompressed sources after
                              re-verifying everything; records archive-deletion.json
  storage-report <root>       read-only free/active/closed/archived byte report

Retention contract (GPT Phase-B decision P1, 2026-10-01): closed run ->
compress -> verify -> receipt -> delete uncompressed source. Operator-run as
root, one run at a time; never automated by this tool. `retain` and
`delete-source` are separate so the receipt can be checked between them, and
`delete-source` is bound to the exact receipt bytes it was authorized for.
"""
import datetime
import gzip
import hashlib
import json
import os
import re
import shutil
import sys

MANIFEST = "archive-manifest.json"
SUFFIX = ".ndjson"
CHUNK = 1 << 20


def _sha_and_size(path):
    h, n = hashlib.sha256(), 0
    with open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                break
            h.update(b)
            n += len(b)
    return h.hexdigest(), n


def _gunzip_sha(path):
    h, n = hashlib.sha256(), 0
    with gzip.open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                break
            h.update(b)
            n += len(b)
    return h.hexdigest(), n


def is_closed(path):
    """Closed iff the final line is a terminated `file_close` record.

    Mirrors the reader's content-based closure: size and mtime are never used.
    """
    size = os.path.getsize(path)
    if size == 0:
        return False
    with open(path, "rb") as f:
        tail_len = min(size, 1 << 16)
        f.seek(size - tail_len)
        tail = f.read()
    if not tail.endswith(b"\n"):
        return False
    last = tail[:-1].rsplit(b"\n", 1)[-1]
    try:
        return json.loads(last).get("kind") == "file_close"
    except ValueError:
        return False


def _load_manifest(run_dir):
    p = os.path.join(run_dir, MANIFEST)
    if os.path.exists(p):
        with open(p, encoding="utf-8") as f:
            return json.load(f)
    return {"schema": "observation-archive-manifest-v1", "runDir": os.path.basename(os.path.normpath(run_dir)), "files": {}}


def _write_manifest(run_dir, manifest):
    p = os.path.join(run_dir, MANIFEST)
    tmp = p + ".part"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=1, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, p)


def _now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def compress(run_dir, level=6):
    manifest = _load_manifest(run_dir)
    done, skipped_open = [], []
    for name in sorted(os.listdir(run_dir)):
        if not name.endswith(SUFFIX):
            continue
        src = os.path.join(run_dir, name)
        if not is_closed(src):
            skipped_open.append(name)
            continue
        entry = manifest["files"].get(name)
        if entry and entry.get("verified"):
            continue
        sha, size = _sha_and_size(src)
        manifest["files"][name] = {"sourceSha256": sha, "sourceBytes": size, "recordedAt": _now(), "verified": False}
        _write_manifest(run_dir, manifest)  # identity recorded before compression
        part = src + ".gz.part"
        with open(src, "rb") as fin, open(part, "wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=level, mtime=0) as gz:
                while True:
                    b = fin.read(CHUNK)
                    if not b:
                        break
                    gz.write(b)
            raw.flush()
            os.fsync(raw.fileno())
        dsha, dsize = _gunzip_sha(part)
        if dsha != sha or dsize != size:
            os.remove(part)
            raise RuntimeError(f"{name}: decompressed SHA {dsha} != source {sha}; not archived")
        gz_path = src + ".gz"
        os.replace(part, gz_path)
        csha, csize = _sha_and_size(gz_path)
        manifest["files"][name].update({
            "compressed": name + ".gz", "algorithm": f"gzip-{level}", "compressedSha256": csha,
            "compressedBytes": csize, "decompressedSha256": dsha, "verified": True, "verifiedAt": _now(),
        })
        _write_manifest(run_dir, manifest)
        done.append(name)
    return done, skipped_open


def verify(run_dir):
    manifest = _load_manifest(run_dir)
    failures = []
    for name, e in sorted(manifest["files"].items()):
        if not e.get("verified"):
            failures.append(f"{name}: not verified")
            continue
        gz_path = os.path.join(run_dir, e["compressed"])
        csha, _ = _sha_and_size(gz_path)
        if csha != e["compressedSha256"]:
            failures.append(f"{name}: compressed SHA mismatch")
            continue
        dsha, dsize = _gunzip_sha(gz_path)
        if dsha != e["sourceSha256"] or dsize != e["sourceBytes"]:
            failures.append(f"{name}: decompressed content mismatch")
    return failures


RECEIPT = "export-receipt.json"


def export(run_dir, dest_root, certificate_path, session, prereg_sha, impl_sha):
    """Copies a closed, compressed, verified run off-box and writes a receipt.

    Deletes nothing. The receipt records whether the VPS source *would* be
    deletion-eligible under the proposed policy; acting on that is a separate,
    manual, authorized step.
    """
    run_id = os.path.basename(os.path.normpath(run_dir))
    manifest = _load_manifest(run_dir)
    with open(certificate_path, "rb") as f:
        cert_bytes = f.read()
    cert = json.loads(cert_bytes)
    dest = os.path.join(dest_root, run_id)
    os.makedirs(dest, exist_ok=True)

    run_files = sorted(n for n in os.listdir(run_dir) if n.endswith(SUFFIX))
    unarchived = [n for n in run_files if not manifest["files"].get(n, {}).get("verified")]
    artifacts, problems = [], []
    for name in run_files:
        e = manifest["files"].get(name)
        if not e or not e.get("verified"):
            continue
        src = os.path.join(run_dir, e["compressed"])
        dst = os.path.join(dest, e["compressed"])
        _copy_fsync(src, dst)
        dsha, dbytes = _sha_and_size(dst)
        if dsha != e["compressedSha256"]:
            problems.append(name + ": destination hash differs")
        artifacts.append({
            "file": name, "sourceSha256": e["sourceSha256"], "sourceBytes": e["sourceBytes"],
            "compressed": e["compressed"], "compressedSha256": e["compressedSha256"],
            "destinationSha256": dsha, "destinationBytes": dbytes,
        })
    # The manifest and certificate travel with the evidence.
    _copy_fsync(os.path.join(run_dir, MANIFEST), os.path.join(dest, MANIFEST))
    cert_dest = os.path.join(dest, "certificate.json")
    with open(cert_dest, "wb") as f:
        f.write(cert_bytes)
        f.flush()
        os.fsync(f.fileno())
    # Verification AT THE DESTINATION: decompression reproduces every source.
    dest_failures = verify(dest)
    inner = cert.get("certificate") or {}
    identity_ok = (
        cert.get("verdict") in ("PASS", "FAIL", "INDETERMINATE")
        and inner.get("runId") == run_id
        and inner.get("preregistrationSha256") == prereg_sha
        and inner.get("implementationSha") == impl_sha
    )
    conditions = {
        "copyComplete": not unarchived and len(artifacts) == len(run_files),
        "destinationHashesMatch": not problems,
        "verifiedAtDestination": not dest_failures,
        "certificateArchived": _sha_and_size(cert_dest)[0] == hashlib.sha256(cert_bytes).hexdigest(),
        "certificateBindsRunAndIdentity": identity_ok,
    }
    receipt = {
        "schema": "observation-export-receipt-v1",
        "session": session,
        "runId": run_id,
        "preregistrationSha256": prereg_sha,
        "implementationSha": impl_sha,
        "certificateSha256": hashlib.sha256(cert_bytes).hexdigest(),
        "certificateVerdict": cert.get("verdict"),
        "exportedAt": _now(),
        "destination": os.path.abspath(dest),
        "artifacts": artifacts,
        "unarchivedFiles": unarchived,
        "problems": problems + dest_failures,
        "conditions": conditions,
        # Receipt existence is the last condition; it holds once this is written.
        "deletionEligible": all(conditions.values()),
        "deletionPolicy": "eligibility only; deletion is manual and separately authorized",
    }
    tmp = os.path.join(dest, RECEIPT + ".part")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(receipt, f, indent=1, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, os.path.join(dest, RECEIPT))
    return receipt


def verify_export(dest):
    """Re-checks an export against its own receipt."""
    with open(os.path.join(dest, RECEIPT), encoding="utf-8") as f:
        r = json.load(f)
    failures = []
    for a in r["artifacts"]:
        sha, _ = _sha_and_size(os.path.join(dest, a["compressed"]))
        if sha != a["destinationSha256"] or sha != a["compressedSha256"]:
            failures.append(a["compressed"] + ": hash changed since export")
    if _sha_and_size(os.path.join(dest, "certificate.json"))[0] != r["certificateSha256"]:
        failures.append("certificate changed since export")
    failures += verify(dest)
    return failures


# ---------------------------------------------------------------------------
# Retention: closed run -> compress -> verify -> receipt -> delete source
# ---------------------------------------------------------------------------

RETENTION_RECEIPT = "archive-receipt.json"
DELETION_RECORD = "archive-deletion.json"
# ObserverRun::allocate: "{namespace}-{pid}-{%Y%m%dT%H%M%S%3fZ}-{collision}".
RUN_DIR_RE = re.compile(r"^[A-Za-z0-9-]{1,48}-[0-9]+-[0-9]{8}T[0-9]{9}Z-[0-9]+$")
SOURCE_RE = re.compile(r"^observations-([0-9]+)\.ndjson$")
RUN_END_TAG = b'"kind":"run_end"'
FLOOR_BYTES = 40 * 1024 ** 3
# Step 4B final freeze-readiness report §5 (measured components): per trading
# session, uncompressed. Planning figures only.
TYPICAL_SESSION_BYTES = 4_250_000_000
STRESS_SESSION_BYTES = 9_420_000_000


class Refusal(Exception):
    """A retention gate failed. Nothing has been deleted."""


def _fsync_dir(path):
    if os.name == "nt":  # directories cannot be opened for fsync on Windows
        return
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _write_json_durably(path, value):
    tmp = path + ".part"
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        json.dump(value, f, indent=1, sort_keys=True)
        f.write("\n")
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)
    _fsync_dir(os.path.dirname(os.path.abspath(path)))


def _first_line(path, limit=1 << 20):
    with open(path, "rb") as f:
        head = f.read(limit)
    if b"\n" not in head:
        raise Refusal(f"{os.path.basename(path)}: no complete first line")
    return head.split(b"\n", 1)[0]


def _last_lines(path, n):
    """The last `n` complete lines (file must end with a newline)."""
    size = os.path.getsize(path)
    span = 1 << 16
    while True:
        with open(path, "rb") as f:
            f.seek(max(0, size - span))
            tail = f.read()
        if not tail.endswith(b"\n"):
            raise Refusal(f"{os.path.basename(path)}: last line is not terminated")
        lines = tail[:-1].split(b"\n")
        if len(lines) > n or span >= size:
            return lines[-n:]
        span *= 4


def _record(line, what):
    try:
        v = json.loads(line)
    except ValueError:
        raise Refusal(f"malformed {what}")
    if not isinstance(v, dict):
        raise Refusal(f"malformed {what}")
    return v


def _count_tag(path, tag):
    n, keep = 0, b""
    with open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                return n
            # `keep` is shorter than the tag, so it never holds a whole one:
            # a tag straddling the chunk boundary is counted exactly once here.
            buf = keep + b
            n += buf.count(tag)
            keep = buf[-(len(tag) - 1):]


def classify(run_dir):
    """Sorts a run directory's entries. Anything unrecognised is returned as unknown."""
    sources, compressed, other, unknown = {}, set(), set(), []
    for name in sorted(os.listdir(run_dir)):
        p = os.path.join(run_dir, name)
        m = SOURCE_RE.match(name)
        if m and os.path.isfile(p) and not os.path.islink(p):
            sources[int(m.group(1))] = name
        elif name.endswith(SUFFIX + ".gz") and SOURCE_RE.match(name[:-3]) and os.path.isfile(p):
            compressed.add(name)
        elif name in (MANIFEST, RETENTION_RECEIPT, DELETION_RECORD) and os.path.isfile(p):
            other.add(name)
        else:
            unknown.append(name)
    return sources, compressed, other, unknown


def closed_run(run_dir, full=True):
    """Proves a run is CLOSED, or raises Refusal. Reads only.

    CLOSED means: an unambiguous run directory holding a contiguous chain of
    `observations-N.ndjson` files, every one ending in a terminated
    `file_close` (each pointing at the next, the last pointing nowhere);
    `run_start` first, naming this directory and both frozen identities; and
    exactly one durable `run_end`, immediately before the final `file_close`,
    naming this directory.
    """
    run_dir = os.path.normpath(run_dir)
    run_id = os.path.basename(run_dir)
    if not os.path.isdir(run_dir) or os.path.islink(run_dir):
        raise Refusal("not a run directory")
    if not RUN_DIR_RE.match(run_id):
        raise Refusal(f"ambiguous run directory name {run_id!r}")
    sources, _, _, unknown = classify(run_dir)
    if unknown:
        raise Refusal(f"unknown files in run directory: {unknown}")
    if not sources:
        raise Refusal("missing source: no observations-*.ndjson")
    if sorted(sources) != list(range(len(sources))):
        raise Refusal(f"source chain is not contiguous from 0: {sorted(sources)}")
    names = [sources[i] for i in range(len(sources))]
    for i, name in enumerate(names):
        p = os.path.join(run_dir, name)
        if not is_closed(p):
            raise Refusal(f"active run: {name} has no terminal file_close")
        close = _record(_last_lines(p, 1)[0], f"file_close in {name}")
        if close.get("runId") != run_id or close.get("fileName") != name:
            raise Refusal(f"run identity mismatch in {name} file_close")
        expected_next = names[i + 1] if i + 1 < len(names) else None
        if close.get("nextFile") != expected_next:
            raise Refusal(f"{name}: file_close nextFile {close.get('nextFile')!r} != {expected_next!r}")
    start = _record(_first_line(os.path.join(run_dir, names[0])), "run_start")
    if start.get("kind") != "run_start" or start.get("runId") != run_id:
        raise Refusal("run identity mismatch: first record is not this run's run_start")
    ns = start.get("namespace")
    if not isinstance(ns, str) or not run_id.startswith(ns + "-"):
        raise Refusal("run identity mismatch: namespace does not prefix the run directory")
    impl, prereg = start.get("implementationSha"), start.get("preregistrationSha256")
    if not (isinstance(impl, str) and re.fullmatch(r"[0-9a-f]{40}", impl)):
        raise Refusal("run identity incomplete: run_start has no implementationSha")
    if not (isinstance(prereg, str) and re.fullmatch(r"[0-9a-f]{64}", prereg)):
        raise Refusal("run identity incomplete: run_start has no preregistrationSha256")
    last = os.path.join(run_dir, names[-1])
    penultimate = _last_lines(last, 2)
    if len(penultimate) < 2:
        raise Refusal("missing run_end")
    end_line = penultimate[0]
    end = _record(end_line, "run_end")
    if end.get("kind") != "run_end":
        raise Refusal("missing run_end: the final file_close is not preceded by run_end")
    if end.get("runId") != run_id:
        raise Refusal("run identity mismatch in run_end")
    for key in ("endedAt", "counters", "captureBytes", "captureMaxBytes", "status"):
        if key not in end:
            raise Refusal(f"malformed run_end: no {key}")
    if full:
        total = sum(_count_tag(os.path.join(run_dir, n), RUN_END_TAG) for n in names)
        if total != 1:
            raise Refusal(f"malformed run: {total} run_end records")
    return {"runId": run_id, "namespace": ns, "implementationSha": impl, "preregistrationSha256": prereg,
            "sources": names, "runStart": start, "runEnd": end,
            "runEndSha256": hashlib.sha256(end_line).hexdigest()}


def retain(run_dir):
    """Archives one closed run and writes its receipt. Deletes nothing."""
    run_dir = os.path.normpath(run_dir)
    if os.path.exists(os.path.join(run_dir, RETENTION_RECEIPT)):
        raise Refusal("run already has an archive receipt")
    info = closed_run(run_dir)
    before = {n: _sha_and_size(os.path.join(run_dir, n)) for n in info["sources"]}
    done, skipped_open = compress(run_dir)
    if skipped_open:
        raise Refusal(f"compression skipped open files {skipped_open}")
    failures = verify(run_dir)
    if failures:
        raise Refusal(f"verification failed: {failures}")
    manifest = _load_manifest(run_dir)
    if sorted(manifest["files"]) != sorted(info["sources"]):
        raise Refusal("archive manifest does not cover exactly the run's sources")
    sources, compressed = [], []
    for n in info["sources"]:
        e = manifest["files"][n]
        sha, size = _sha_and_size(os.path.join(run_dir, n))
        if (sha, size) != before[n] or (sha, size) != (e["sourceSha256"], e["sourceBytes"]):
            raise Refusal(f"changed source after hashing: {n}")
        sources.append({"file": n, "bytes": size, "sha256": sha})
        compressed.append({"file": e["compressed"], "bytes": e["compressedBytes"], "sha256": e["compressedSha256"],
                           "algorithm": e["algorithm"], "decompressedSha256": e["decompressedSha256"], "source": n})
    receipt = {
        "schema": "observation-archive-receipt-v1",
        "runId": info["runId"],
        "namespace": info["namespace"],
        "implementationSha": info["implementationSha"],
        "preregistrationSha256": info["preregistrationSha256"],
        "runPath": os.path.abspath(run_dir),
        "runStart": info["runStart"],
        "runEnd": info["runEnd"],
        "runEndSha256": info["runEndSha256"],
        "sources": sources,
        "compressed": compressed,
        "verification": {"closedRun": True, "compressedHashesMatch": True,
                         "decompressionReproducesSource": True, "sourcesUnchangedSinceHashing": True},
        "archivedAt": _now(),
        "deletion": {"state": "eligible-awaiting-operator",
                     "policy": "GPT Phase-B P1 2026-10-01: delete uncompressed sources only via delete-source, bound to this receipt's SHA-256"},
    }
    _write_json_durably(os.path.join(run_dir, RETENTION_RECEIPT), receipt)
    with open(os.path.join(run_dir, RETENTION_RECEIPT), "rb") as f:
        receipt_sha = hashlib.sha256(f.read()).hexdigest()
    return receipt, receipt_sha


def delete_source(run_dir, receipt_sha):
    """Deletes exactly the receipt's uncompressed sources, after re-verifying all of it."""
    run_dir = os.path.normpath(run_dir)
    rpath = os.path.join(run_dir, RETENTION_RECEIPT)
    if not os.path.isfile(rpath):
        raise Refusal("no archive receipt")
    with open(rpath, "rb") as f:
        rbytes = f.read()
    if hashlib.sha256(rbytes).hexdigest() != receipt_sha:
        raise Refusal("receipt does not match the authorized SHA-256")
    receipt = json.loads(rbytes)
    if receipt.get("schema") != "observation-archive-receipt-v1":
        raise Refusal("not an archive receipt")
    if os.path.exists(os.path.join(run_dir, DELETION_RECORD)):
        raise Refusal("a deletion record already exists; inspect it, do not re-run")
    info = closed_run(run_dir)
    for k in ("runId", "namespace", "implementationSha", "preregistrationSha256", "runEndSha256"):
        if receipt.get(k) != info[k]:
            raise Refusal(f"run identity mismatch with receipt: {k}")
    if os.path.abspath(run_dir) != receipt.get("runPath"):
        raise Refusal("receipt was written for a different run path")
    sources, compressed, _, unknown = classify(run_dir)
    if unknown:
        raise Refusal(f"unknown files in run directory: {unknown}")
    deletion = [s["file"] for s in receipt["sources"]]
    if sorted(sources.values()) != sorted(deletion):
        raise Refusal(f"deletion set differs from the sources present: {sorted(sources.values())} vs {sorted(deletion)}")
    by_source = {c["source"]: c for c in receipt["compressed"]}
    for s in receipt["sources"]:
        sha, size = _sha_and_size(os.path.join(run_dir, s["file"]))
        if (sha, size) != (s["sha256"], s["bytes"]):
            raise Refusal(f"changed source after hashing: {s['file']}")
        c = by_source.get(s["file"])
        if not c or c["file"] not in compressed:
            raise Refusal(f"missing compressed artifact for {s['file']}")
        gz = os.path.join(run_dir, c["file"])
        if _sha_and_size(gz) != (c["sha256"], c["bytes"]):
            raise Refusal(f"compressed artifact changed: {c['file']}")
        try:
            dsha, dsize = _gunzip_sha(gz)
        except (OSError, EOFError, ValueError) as e:
            raise Refusal(f"corrupt compressed artifact {c['file']}: {e}")
        if (dsha, dsize) != (s["sha256"], s["bytes"]):
            raise Refusal(f"decompression mismatch: {c['file']}")
    record = {
        "schema": "observation-archive-deletion-v1",
        "runId": receipt["runId"],
        "receiptSha256": receipt_sha,
        "authorizedSet": [{"file": s["file"], "bytes": s["bytes"], "sha256": s["sha256"]} for s in receipt["sources"]],
        "state": "in-progress",
        "startedAt": _now(),
    }
    dpath = os.path.join(run_dir, DELETION_RECORD)
    _write_json_durably(dpath, record)  # intent is durable before anything is removed
    for name in deletion:
        os.remove(os.path.join(run_dir, name))
    _fsync_dir(run_dir)
    remaining = sorted(os.listdir(run_dir))
    record.update({
        "state": "complete",
        "completedAt": _now(),
        "removed": deletion,
        "removedAbsent": all(not os.path.exists(os.path.join(run_dir, n)) for n in deletion),
        "remaining": remaining,
        "retainedEvidencePresent": all(n in remaining for n in [c["file"] for c in receipt["compressed"]] + [RETENTION_RECEIPT, MANIFEST]),
    })
    _write_json_durably(dpath, record)
    return record


def _dir_bytes(run_dir, names):
    return sum(os.path.getsize(os.path.join(run_dir, n)) for n in names)


def storage_report(root, free_bytes=None):
    """Read-only: where the observation root's bytes are, and how many sessions fit above the floor."""
    if free_bytes is None:
        # Bytes available to an unprivileged writer (statvfs f_bavail on POSIX):
        # the same figure `df` reports as Avail.
        free_bytes = shutil.disk_usage(root).free
    classes = {"active": [], "closedUnarchived": [], "archivedSourcesPresent": [], "archivedSourcesDeleted": [], "unrecognised": []}
    totals = {"activeRunBytes": 0, "closedUnarchivedBytes": 0, "archivedSourceBytes": 0, "compressedArchiveBytes": 0}
    for run_id in sorted(os.listdir(root)):
        d = os.path.join(root, run_id)
        if not os.path.isdir(d) or not RUN_DIR_RE.match(run_id):
            continue
        sources, compressed, other, unknown = classify(d)
        src_bytes = _dir_bytes(d, sources.values())
        totals["compressedArchiveBytes"] += _dir_bytes(d, compressed)
        if unknown:
            classes["unrecognised"].append(run_id)
            totals["activeRunBytes"] += src_bytes
            continue
        if DELETION_RECORD in other:
            classes["archivedSourcesDeleted"].append(run_id)
        elif RETENTION_RECEIPT in other:
            classes["archivedSourcesPresent"].append(run_id)
            totals["archivedSourceBytes"] += src_bytes
        else:
            try:
                closed_run(d, full=False)
                classes["closedUnarchived"].append(run_id)
                totals["closedUnarchivedBytes"] += src_bytes
            except Refusal:
                classes["active"].append(run_id)
                totals["activeRunBytes"] += src_bytes
    headroom = free_bytes - FLOOR_BYTES
    return {
        "schema": "observation-storage-report-v1",
        "generatedAt": _now(),
        "root": os.path.abspath(root),
        "freeBytes": free_bytes,
        "floorBytes": FLOOR_BYTES,
        "aboveFloor": headroom >= 0,
        **totals,
        "runs": {k: v for k, v in classes.items()},
        "estimatedRemainingTypicalSessions": max(0, headroom // TYPICAL_SESSION_BYTES),
        "estimatedRemainingStressSessions": max(0, headroom // STRESS_SESSION_BYTES),
        "planningBasis": {"typicalSessionBytes": TYPICAL_SESSION_BYTES, "stressSessionBytes": STRESS_SESSION_BYTES,
                          "source": "STEP4B-FINAL-FREEZE-READINESS-REPORT-20260929 section 5, uncompressed"},
    }


def _copy_fsync(src, dst):
    tmp = dst + ".part"
    with open(src, "rb") as fin, open(tmp, "wb") as fout:
        while True:
            b = fin.read(CHUNK)
            if not b:
                break
            fout.write(b)
        fout.flush()
        os.fsync(fout.fileno())
    os.replace(tmp, dst)


def canonical_bytes(value):
    """RFC 8785 for the preregistration subset: integers only, ASCII keys."""
    def check(v, path):
        if isinstance(v, float):
            raise ValueError(f"{path}: floats are not admitted")
        if isinstance(v, dict):
            for k, x in v.items():
                if not k.isascii():
                    raise ValueError(f"{path}.{k}: non-ASCII key")
                check(x, f"{path}.{k}")
        elif isinstance(v, list):
            for i, x in enumerate(v):
                check(x, f"{path}[{i}]")
    check(value, "$")
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def prereg_sha(path):
    with open(path, encoding="utf-8") as f:
        return hashlib.sha256(canonical_bytes(json.load(f))).hexdigest()


def main(argv):
    try:
        if len(argv) == 3 and argv[1] == "retain":
            receipt, sha = retain(argv[2])
            print(json.dumps({"receiptSha256": sha, "runId": receipt["runId"], "deletionEligible": True,
                              "sources": receipt["sources"], "compressed": receipt["compressed"]}, indent=1))
            return 0
        if len(argv) == 4 and argv[1] == "delete-source":
            print(json.dumps(delete_source(argv[2], argv[3]), indent=1))
            return 0
    except Refusal as r:
        print(json.dumps({"refused": str(r), "deleted": []}))
        return 1
    if len(argv) == 3 and argv[1] == "storage-report":
        print(json.dumps(storage_report(argv[2]), indent=1))
        return 0
    if len(argv) == 8 and argv[1] == "export":
        r = export(*argv[2:8])
        print(json.dumps({"deletionEligible": r["deletionEligible"], "conditions": r["conditions"]}))
        return 0 if r["deletionEligible"] else 1
    if len(argv) == 3 and argv[1] == "verify-export":
        failures = verify_export(argv[2])
        print(json.dumps({"failures": failures}))
        return 1 if failures else 0
    if len(argv) != 3 or argv[1] not in ("compress", "verify", "prereg-sha"):
        print(__doc__)
        return 2
    cmd, arg = argv[1], argv[2]
    if cmd == "compress":
        done, open_files = compress(arg)
        print(json.dumps({"compressed": done, "skippedOpen": open_files}))
        return 0
    if cmd == "verify":
        failures = verify(arg)
        print(json.dumps({"failures": failures}))
        return 1 if failures else 0
    print(prereg_sha(arg))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
