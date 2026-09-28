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
