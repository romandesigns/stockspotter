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
                              verify, write archive-receipt.json
  storage-report <root>       read-only byte report: free space, and runs grouped by
                              what is observed in each directory (nothing verified)

What `retain` and `storage-report` do and do not do:

  * `storage-report` only reads, and it verifies nothing. It groups runs by
    what it observes -- whether a file named `archive-receipt.json` is present,
    whether uncompressed sources are present -- and every such group and total
    says `Unverified` in its name. It does not open a receipt, hash an artifact
    or decompress anything, so a malformed or empty receipt lands in the same
    group as a good one. Whether a run is a valid archive is what `verify` and
    `observation_offbox.py attest-source` establish, not this report.
  * `retain` proves the run CLOSED, then adds files beside the sources: one
    `.gz` per source, the manifest and `archive-receipt.json`. It changes no
    file that existed before it ran. It refuses a run that already has a
    receipt, and a run holding archive output that is not complete and
    verified (a stray `.gz`, a manifest with an unverified entry), so it never
    replaces an artifact or a manifest left by an earlier attempt. The receipt
    is created exclusively and is never rewritten.
  * No command removes an observation file, and the only file this tool ever
    removes is its own `<name>.gz.part` temporary, when a fresh compression
    fails its check. What happens to uncompressed sources after a run is
    archived is outside this tool.
  * The older commands keep their behaviour and are not covered by the
    statement about `retain`: run directly, `compress` rewrites the manifest it
    maintains and replaces a `.gz` that is not yet marked verified, and
    `export` rewrites its own copies and its receipt at the destination.

Names read from a manifest or a receipt are untrusted: each must be a bare
file name naming a regular file, not a link, directly inside the run
directory. Decompression is streamed and capped
(OBSERVATION_MAX_DECOMPRESSED_BYTES, OBSERVATION_MAX_COMPRESSED_BYTES; bytes
per artifact, default 20 GiB each -- a session measures about 4.25 GB typical
and 9.4 GB under stress uncompressed, against a 16 GiB capture budget); an
artifact over a cap is refused, not truncated.

These path checks assume an operator-controlled run directory that nothing is
writing to while a command runs. A name is checked and then opened as two
separate operations, so this is not hardened against a hostile process changing
the directory concurrently. Nothing here is a claim of production readiness.
"""
import datetime
import gzip
import hashlib
import json
import os
import re
import shutil
import stat
import sys
import zlib

MANIFEST = "archive-manifest.json"
SUFFIX = ".ndjson"
CHUNK = 1 << 20

# Read caps, in bytes per artifact. A session measures about 4.25 GB (typical)
# and 9.4 GB (stress) uncompressed and the observer's capture budget is 16 GiB,
# so 20 GiB admits anything the observer can write and still stops a
# decompression bomb. Each is overridden by a positive integer in the
# environment variable of the same name.
ENV_MAX_DECOMPRESSED = "OBSERVATION_MAX_DECOMPRESSED_BYTES"
ENV_MAX_COMPRESSED = "OBSERVATION_MAX_COMPRESSED_BYTES"
DEFAULT_MAX_DECOMPRESSED_BYTES = 20 * 1024 ** 3
DEFAULT_MAX_COMPRESSED_BYTES = 20 * 1024 ** 3
# Receipts, manifests and attestations are small JSON documents.
MAX_JSON_BYTES = 64 * 1024 ** 2
# What reading a gzip stream raises when the stream is damaged.
GUNZIP_ERRORS = (OSError, EOFError, ValueError, zlib.error)


class Refusal(Exception):
    """A gate failed and the command stopped there."""


def _limit(env, default):
    raw = os.environ.get(env)
    if raw is None:
        return default
    if not raw.isascii() or not raw.isdigit() or int(raw) <= 0:
        raise Refusal(f"{env} must be a positive integer number of bytes, got {raw!r}")
    return int(raw)


def _bare_name(name):
    """A file name taken from a manifest, receipt or attestation: exactly one
    plain path component. Separators, a drive or stream qualifier (`:`), NUL,
    `.`, `..`, the empty string and anything absolute are refused before the
    name is joined to a directory."""
    if (not isinstance(name, str) or name in ("", ".", "..") or any(c in name for c in "/\\:\0")
            or os.path.isabs(name) or os.path.basename(name) != name):
        raise Refusal(f"unsafe file name in evidence: {name!r}")
    return name


def _is_link(path):
    """A symlink, or on Windows any reparse point (junction, mount point)."""
    st = os.lstat(path)
    return stat.S_ISLNK(st.st_mode) or bool(getattr(st, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT)


def _regular(path):
    """A regular file that is not a link."""
    try:
        return stat.S_ISREG(os.lstat(path).st_mode) and not _is_link(path)
    except OSError:
        return False


def _evidence_file(directory, name):
    """`name` inside `directory`: a bare name, a regular file, not a link, and
    resolving to a direct child of the directory."""
    path = os.path.join(directory, _bare_name(name))
    if not os.path.lexists(path):
        raise Refusal(f"missing evidence file: {name}")
    if not _regular(path):
        raise Refusal(f"{name} is not a regular file (symlinks and reparse points are refused)")
    if os.path.dirname(os.path.realpath(path)) != os.path.realpath(directory):
        raise Refusal(f"{name} resolves outside its directory")
    return path


def _read_small(path, what):
    with open(path, "rb") as f:
        b = f.read(MAX_JSON_BYTES + 1)
    if len(b) > MAX_JSON_BYTES:
        raise Refusal(f"{what} is larger than {MAX_JSON_BYTES} bytes")
    return b


def _parse_object(b, what):
    try:
        v = json.loads(b)
    except ValueError:
        raise Refusal(f"malformed {what}")
    if not isinstance(v, dict):
        raise Refusal(f"malformed {what}")
    return v


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


def _gunzip_sha(path, max_bytes=None):
    """SHA-256 and size of the decompressed stream.

    Streams one chunk at a time and stops at the cap: an artifact whose
    compressed size or decompressed size exceeds its limit raises Refusal.
    """
    name = os.path.basename(path)
    compressed_cap = _limit(ENV_MAX_COMPRESSED, DEFAULT_MAX_COMPRESSED_BYTES)
    if os.path.getsize(path) > compressed_cap:
        raise Refusal(f"compressed-size cap exceeded at {name}: more than {compressed_cap} bytes ({ENV_MAX_COMPRESSED})")
    cap = _limit(ENV_MAX_DECOMPRESSED, DEFAULT_MAX_DECOMPRESSED_BYTES) if max_bytes is None else max_bytes
    h, n = hashlib.sha256(), 0
    with gzip.open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                break
            n += len(b)
            if n > cap:
                raise Refusal(f"decompressed-size cap exceeded at {name}: more than {cap} bytes ({ENV_MAX_DECOMPRESSED})")
            h.update(b)
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
        if size > _limit(ENV_MAX_DECOMPRESSED, DEFAULT_MAX_DECOMPRESSED_BYTES):
            # Refused before anything is written: its verification would be refused anyway.
            raise Refusal(f"decompressed-size cap exceeded at {name}: {size} bytes ({ENV_MAX_DECOMPRESSED})")
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
        try:
            dsha, dsize = _gunzip_sha(part)
        except Refusal:
            os.remove(part)  # this invocation's own temporary; never left to block the run
            raise
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
        try:
            gz_path = _evidence_file(run_dir, e.get("compressed"))
        except Refusal as r:
            failures.append(f"{name}: {r}")
            continue
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
        src = _evidence_file(run_dir, e["compressed"])
        dst = os.path.join(dest, _bare_name(e["compressed"]))
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
        sha, _ = _sha_and_size(_evidence_file(dest, a["compressed"]))
        if sha != a["destinationSha256"] or sha != a["compressedSha256"]:
            failures.append(a["compressed"] + ": hash changed since export")
    if _sha_and_size(os.path.join(dest, "certificate.json"))[0] != r["certificateSha256"]:
        failures.append("certificate changed since export")
    failures += verify(dest)
    return failures


# ---------------------------------------------------------------------------
# Retention: closed run -> compress -> verify -> receipt
# ---------------------------------------------------------------------------

RETENTION_RECEIPT = "archive-receipt.json"
# Nothing in this repository writes this file. A run whose uncompressed sources
# were removed after archiving may carry such a record; it is recognised by
# name so that run stays readable, and is never created or modified here.
DELETION_RECORD = "archive-deletion.json"
# ObserverRun::allocate: "{namespace}-{pid}-{%Y%m%dT%H%M%S%3fZ}-{collision}".
RUN_DIR_RE = re.compile(r"^[A-Za-z0-9-]{1,48}-[0-9]+-[0-9]{8}T[0-9]{9}Z-[0-9]+$")
SOURCE_RE = re.compile(r"^observations-([0-9]+)\.ndjson$")
RUN_END_TAG = b'"kind":"run_end"'
FLOOR_BYTES = 40 * 1024 ** 3
# Measured uncompressed capture size per trading session: a typical session
# and the stress case. Planning figures only.
TYPICAL_SESSION_BYTES = 4_250_000_000
STRESS_SESSION_BYTES = 9_420_000_000


def _fsync_dir(path):
    if os.name == "nt":  # directories cannot be opened for fsync on Windows
        return
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _create_file_durably(path, data):
    """Creates `path` with `data` and makes it durable. Never replaces a file:
    the open is exclusive, so an existing path is a refusal.

    Written in place rather than through a temporary and a rename, because a
    rename can replace. The cost is that a crash mid-write can leave a partial
    file; every reader refuses a malformed receipt, so that fails closed.
    """
    try:
        f = open(path, "xb")
    except FileExistsError:
        raise Refusal(f"{os.path.basename(path)} already exists; it is never overwritten")
    with f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    _fsync_dir(os.path.dirname(os.path.abspath(path)))


def _create_json_durably(path, value):
    _create_file_durably(path, (json.dumps(value, indent=1, sort_keys=True) + "\n").encode("utf-8"))


def receipt_pairs(receipt):
    """The (source, artifact) entries of an archive receipt, names validated.

    A source is `observations-N.ndjson` and its artifact is exactly
    `<source>.gz`, so a receipt has no freedom to name a file of its choosing.
    """
    sources, compressed = receipt.get("sources"), receipt.get("compressed")
    if not isinstance(sources, list) or not isinstance(compressed, list) or not sources or len(sources) != len(compressed):
        raise Refusal("archive receipt sources and compressed artifacts do not pair up")
    by_source = {}
    for c in compressed:
        if not isinstance(c, dict):
            raise Refusal("malformed archive receipt artifact")
        source = _bare_name(c.get("source"))
        if _bare_name(c.get("file")) != source + ".gz" or source in by_source:
            raise Refusal(f"archive receipt artifact {c.get('file')!r} is not exactly {source}.gz")
        by_source[source] = c
    pairs = []
    for s in sources:
        if not isinstance(s, dict):
            raise Refusal("malformed archive receipt source")
        name = _bare_name(s.get("file"))
        if not SOURCE_RE.match(name) or name not in by_source:
            raise Refusal(f"archive receipt source {name!r} is not an observation file with an artifact")
        pairs.append((s, by_source.pop(name)))
    return pairs


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
        if span >= 1 << 26:  # a closing record is never tens of megabytes
            raise Refusal(f"{os.path.basename(path)}: no terminated closing records in its last {span} bytes")
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
    """Sorts a run directory's entries. Anything unrecognised -- including any
    link or reparse point, whatever its name -- is returned as unknown."""
    sources, compressed, other, unknown = {}, set(), set(), []
    for name in sorted(os.listdir(run_dir)):
        p = os.path.join(run_dir, name)
        m = SOURCE_RE.match(name)
        if m and _regular(p):
            sources[int(m.group(1))] = name
        elif name.endswith(SUFFIX + ".gz") and SOURCE_RE.match(name[:-3]) and _regular(p):
            compressed.add(name)
        elif name in (MANIFEST, RETENTION_RECEIPT, DELETION_RECORD) and _regular(p):
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
    if not os.path.isdir(run_dir) or _is_link(run_dir):
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
    """Archives one closed run and writes its receipt.

    Adds files beside the sources and changes none that exist. A run holding
    earlier archive output is accepted only if that output is complete and
    verified -- then compression has nothing left to do and only the receipt
    is added. Anything in between is refused, because finishing it would mean
    replacing a manifest or an artifact that is already there.
    """
    run_dir = os.path.normpath(run_dir)
    if os.path.lexists(os.path.join(run_dir, RETENTION_RECEIPT)):
        raise Refusal("run already has an archive receipt")
    info = closed_run(run_dir)
    _, compressed_present, other, _ = classify(run_dir)
    if DELETION_RECORD in other:
        raise Refusal("run carries a source-removal record but no archive receipt")
    if compressed_present or MANIFEST in other:
        files = _load_manifest(run_dir).get("files") if MANIFEST in other else None
        complete = (isinstance(files, dict) and sorted(files) == sorted(info["sources"])
                    and all(isinstance(e, dict) and e.get("verified") is True and e.get("compressed") == n + ".gz"
                            for n, e in files.items())
                    and compressed_present == {n + ".gz" for n in info["sources"]})
        if not complete:
            raise Refusal("partially archived run: retain never replaces an existing artifact or manifest; "
                          "it archives a run holding only its sources, or adds the receipt to one whose every "
                          "source is already compressed and verified (`compress` finishes an interrupted "
                          "compression explicitly)")
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
    }
    receipt_pairs(receipt)  # what is written is what every reader will accept
    rpath = os.path.join(run_dir, RETENTION_RECEIPT)
    _create_json_durably(rpath, receipt)
    return receipt, hashlib.sha256(_read_small(rpath, "archive receipt")).hexdigest()


def _dir_bytes(run_dir, names):
    return sum(os.path.getsize(os.path.join(run_dir, n)) for n in names)


def storage_report(root, free_bytes=None):
    """Read-only: where the observation root's bytes are, and how many sessions fit above the floor.

    Verifies nothing. A run is grouped by what is observed in its directory:
    `receiptPresent...Unverified` means only that a regular file named
    `archive-receipt.json` is there -- it is not opened, so it may be empty or
    malformed -- and the byte totals count files by name without hashing them.
    `closedNoReceipt` passed the quick closure check; `active` did not.
    """
    if free_bytes is None:
        # Bytes available to an unprivileged writer (statvfs f_bavail on POSIX):
        # the same figure `df` reports as Avail.
        free_bytes = shutil.disk_usage(root).free
    classes = {"active": [], "closedNoReceipt": [], "receiptPresentSourcesPresentUnverified": [],
               "receiptPresentSourcesAbsentUnverified": [], "unrecognised": []}
    totals = {"activeRunBytes": 0, "closedNoReceiptBytes": 0, "receiptPresentSourceBytesUnverified": 0,
              "compressedFileBytesUnverified": 0}
    for run_id in sorted(os.listdir(root)):
        d = os.path.join(root, run_id)
        if not os.path.isdir(d) or _is_link(d) or not RUN_DIR_RE.match(run_id):
            continue
        sources, compressed, other, unknown = classify(d)
        src_bytes = _dir_bytes(d, sources.values())
        totals["compressedFileBytesUnverified"] += _dir_bytes(d, compressed)
        if unknown or (DELETION_RECORD in other and RETENTION_RECEIPT not in other):
            classes["unrecognised"].append(run_id)
            totals["activeRunBytes"] += src_bytes
            continue
        if RETENTION_RECEIPT in other:
            # Observed, not verified: a file of that name exists. Its content is not read.
            classes["receiptPresentSourcesPresentUnverified" if sources else "receiptPresentSourcesAbsentUnverified"].append(run_id)
            totals["receiptPresentSourceBytesUnverified"] += src_bytes
        else:
            try:
                closed_run(d, full=False)
                classes["closedNoReceipt"].append(run_id)
                totals["closedNoReceiptBytes"] += src_bytes
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
        "receiptsVerified": False,
        "note": "runs are grouped by the files observed in each directory; no receipt was opened and no artifact was hashed",
        **totals,
        "runs": {k: v for k, v in classes.items()},
        "estimatedRemainingTypicalSessions": max(0, headroom // TYPICAL_SESSION_BYTES),
        "estimatedRemainingStressSessions": max(0, headroom // STRESS_SESSION_BYTES),
        "planningBasis": {"typicalSessionBytes": TYPICAL_SESSION_BYTES, "stressSessionBytes": STRESS_SESSION_BYTES,
                          "source": "measured capture size per trading session, uncompressed (typical and stress)"},
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
        return _dispatch(argv)
    except Refusal as r:
        print(json.dumps({"refused": str(r)}))
        return 1


def _dispatch(argv):
    if len(argv) == 3 and argv[1] == "retain":
        receipt, sha = retain(argv[2])
        print(json.dumps({"receiptSha256": sha, "runId": receipt["runId"],
                          "sources": receipt["sources"], "compressed": receipt["compressed"]}, indent=1))
        return 0
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
