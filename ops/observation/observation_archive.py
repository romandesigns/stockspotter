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
"""
import datetime
import gzip
import hashlib
import json
import os
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
