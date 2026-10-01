"""Adversarial tests for the observer-run retention contract (GPT Phase-B P1, 2026-10-01):
closed run -> compress -> verify -> receipt -> delete uncompressed source.

Every refusal must leave the source bytes exactly as they were.
"""
import gzip
import hashlib
import importlib.util
import json
import os
import pathlib

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("observation_archive", ROOT / "ops" / "observation" / "observation_archive.py")
archive = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(archive)

RUN_ID = "stockspotter-vps-1-20261002T201000123Z-0"
IMPL = "de5b55e66bd77b70fa7a026d178fb09634704264"
PREREG = "746446819af285d20a749a81eb27cf8984d7d488860beb4f8f575106942be7f8"


def _line(r):
    # serde_json's compact form, which is what the observer writes.
    return json.dumps(r, separators=(",", ":"))


def make_run(root, files=2, rows=300, run_end=True, close_last=True, run_end_text=None, run_id=RUN_ID, start_run_id=None):
    d = root / run_id
    d.mkdir()
    for i in range(files):
        name = f"observations-{i}.ndjson"
        recs = []
        if i == 0:
            recs.append({"kind": "run_start", "protocolVersion": "consumer-received-protocol-v1", "runId": start_run_id or run_id,
                         "namespace": "stockspotter-vps", "pid": 1, "startedAt": "2026-10-03T00:10:00.123Z",
                         "freshnessMaxAgeSecs": 30, "implementationSha": IMPL, "preregistrationSha256": PREREG})
        else:
            recs.append({"kind": "file_start", "runId": run_id, "fileName": name, "sequence": i, "previousFile": f"observations-{i - 1}.ndjson"})
        recs += [{"kind": "receipt", "runId": run_id, "sequence": i * rows + k, "symbol": "S%d" % (k % 37)} for k in range(rows)]
        last = i == files - 1
        lines = [_line(r) for r in recs]
        if last and run_end:
            lines.append(run_end_text if run_end_text is not None else _line(
                {"kind": "run_end", "runId": run_id, "endedAt": "2026-10-04T00:10:00.5Z",
                 "counters": {"attempted": 1, "written": 1, "dropped": 0, "writeErrors": 0, "overflowed": False},
                 "captureBytes": 10, "captureMaxBytes": 17179869184, "status": {"tapAttached": True, "tapDropped": 0}}))
        if not last or close_last:
            close = {"kind": "file_close", "runId": run_id, "fileName": name, "recordsWritten": len(lines), "closedAt": "2026-10-04T00:10:01Z"}
            if not last:
                close["nextFile"] = f"observations-{i + 1}.ndjson"
            lines.append(_line(close))
        (d / name).write_bytes(("\n".join(lines) + "\n").encode())
    return d


def snapshot(d):
    return {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(d.iterdir()) if p.name.endswith(".ndjson")}


def refused(fn, *args, match):
    with pytest.raises(archive.Refusal, match=match):
        fn(*args)


# --- closure ----------------------------------------------------------------

def test_active_run_refused(tmp_path):
    d = make_run(tmp_path, close_last=False)
    before = snapshot(d)
    refused(archive.retain, str(d), match="active run")
    assert snapshot(d) == before and not (d / archive.RETENTION_RECEIPT).exists()


def test_missing_run_end_refused(tmp_path):
    d = make_run(tmp_path, run_end=False)
    refused(archive.retain, str(d), match="missing run_end")


def test_malformed_run_end_refused(tmp_path):
    d = make_run(tmp_path, run_end_text='{"kind":"run_end","runId":"' + RUN_ID + '"')  # truncated JSON
    refused(archive.retain, str(d), match="malformed run_end")
    (tmp_path / "b").mkdir()
    d2 = make_run(tmp_path / "b", run_end_text=_line({"kind": "run_end", "runId": RUN_ID}))  # missing required fields
    refused(archive.retain, str(d2), match="malformed run_end")


def test_run_identity_mismatch_refused(tmp_path):
    d = make_run(tmp_path, start_run_id="stockspotter-vps-1-20261001T201000123Z-0")
    refused(archive.retain, str(d), match="run identity mismatch")


def test_ambiguous_run_directory_refused(tmp_path):
    d = make_run(tmp_path, run_id="observation-run")
    refused(archive.retain, str(d), match="ambiguous run directory")


def test_broken_file_chain_refused(tmp_path):
    d = make_run(tmp_path, files=3)
    os.remove(d / "observations-1.ndjson")
    refused(archive.retain, str(d), match="not contiguous")


def test_unknown_file_refused_before_anything(tmp_path):
    d = make_run(tmp_path)
    (d / "observations-0.ndjson.close-failed").write_text("x")
    refused(archive.retain, str(d), match="unknown files")
    assert not (d / archive.MANIFEST).exists()


def test_duplicate_run_end_refused(tmp_path):
    d = make_run(tmp_path)
    p = d / "observations-0.ndjson"
    lines = p.read_bytes().split(b"\n")
    lines.insert(2, _line({"kind": "run_end", "runId": RUN_ID}).encode())
    p.write_bytes(b"\n".join(lines))
    refused(archive.retain, str(d), match="2 run_end records")


# --- archive + delete ---------------------------------------------------------

def test_successful_round_trip_and_exact_deletion_set(tmp_path):
    d = make_run(tmp_path, files=3)
    originals = {p.name: p.read_bytes() for p in d.iterdir()}
    receipt, sha = archive.retain(str(d))
    assert receipt["runId"] == RUN_ID and receipt["implementationSha"] == IMPL and receipt["preregistrationSha256"] == PREREG
    assert [s["file"] for s in receipt["sources"]] == sorted(originals)
    assert receipt["runEnd"]["kind"] == "run_end" and receipt["runStart"]["kind"] == "run_start"
    assert receipt["deletion"]["state"] == "eligible-awaiting-operator"
    # Nothing deleted by retain.
    assert all((d / n).exists() for n in originals)
    record = archive.delete_source(str(d), sha)
    assert record["state"] == "complete" and record["removedAbsent"] and record["retainedEvidencePresent"]
    assert sorted(record["removed"]) == sorted(originals)
    remaining = sorted(p.name for p in d.iterdir())
    assert remaining == sorted([n + ".gz" for n in originals] + [archive.MANIFEST, archive.RETENTION_RECEIPT, archive.DELETION_RECORD])
    for n, b in originals.items():
        assert gzip.decompress((d / (n + ".gz")).read_bytes()) == b
    assert d.is_dir()


def test_wrong_receipt_sha_refuses_deletion(tmp_path):
    d = make_run(tmp_path)
    before = snapshot(d)
    archive.retain(str(d))
    refused(archive.delete_source, str(d), "0" * 64, match="does not match the authorized")
    assert snapshot(d) == before


def test_changed_source_refuses_deletion(tmp_path):
    d = make_run(tmp_path)
    _, sha = archive.retain(str(d))
    p = d / "observations-0.ndjson"
    b = p.read_bytes()
    p.write_bytes(b.replace(b'"symbol":"S1"', b'"symbol":"S9"', 1))
    refused(archive.delete_source, str(d), sha, match="changed source")
    assert p.exists() and (d / "observations-1.ndjson").exists()


def test_corrupt_gzip_refuses_deletion(tmp_path):
    d = make_run(tmp_path)
    _, sha = archive.retain(str(d))
    gz = d / "observations-1.ndjson.gz"
    b = bytearray(gz.read_bytes())
    b[len(b) // 2] ^= 0xFF
    gz.write_bytes(bytes(b))
    before = snapshot(d)
    refused(archive.delete_source, str(d), sha, match="compressed artifact changed")
    assert snapshot(d) == before


def test_decompression_mismatch_refuses_deletion(tmp_path):
    # A receipt that (wrongly) vouches for a gzip of different bytes: the
    # decompression check is what catches it, even with the receipt authorized.
    d = make_run(tmp_path)
    archive.retain(str(d))
    gz = d / "observations-0.ndjson.gz"
    gz.write_bytes(gzip.compress(b"not the source\n", mtime=0))
    rpath = d / archive.RETENTION_RECEIPT
    r = json.loads(rpath.read_text())
    c = next(c for c in r["compressed"] if c["file"] == gz.name)
    c["sha256"], c["bytes"] = hashlib.sha256(gz.read_bytes()).hexdigest(), gz.stat().st_size
    rpath.write_text(json.dumps(r, indent=1, sort_keys=True) + "\n")
    sha = hashlib.sha256(rpath.read_bytes()).hexdigest()
    refused(archive.delete_source, str(d), sha, match="decompression mismatch")
    assert (d / "observations-0.ndjson").exists()


def test_receipt_write_failure_refuses_deletion(tmp_path, monkeypatch):
    d = make_run(tmp_path)
    before = snapshot(d)

    def fail(path, value):
        raise OSError("disk full")
    monkeypatch.setattr(archive, "_write_json_durably", fail)
    with pytest.raises(OSError):
        archive.retain(str(d))
    monkeypatch.undo()
    refused(archive.delete_source, str(d), "0" * 64, match="no archive receipt")
    assert snapshot(d) == before


def test_extra_unknown_source_refuses_deletion(tmp_path):
    d = make_run(tmp_path)
    _, sha = archive.retain(str(d))
    (d / "observations-7.ndjson").write_text("{}\n")
    refused(archive.delete_source, str(d), sha, match="not contiguous|deletion set differs")
    assert (d / "observations-0.ndjson").exists()
    os.remove(d / "observations-7.ndjson")
    (d / "notes.txt").write_text("x")
    refused(archive.delete_source, str(d), sha, match="unknown files")
    assert (d / "observations-0.ndjson").exists()


def test_deletion_is_never_repeated(tmp_path):
    d = make_run(tmp_path)
    _, sha = archive.retain(str(d))
    archive.delete_source(str(d), sha)
    refused(archive.delete_source, str(d), sha, match="deletion record already exists")


def test_retain_is_not_repeated(tmp_path):
    d = make_run(tmp_path)
    archive.retain(str(d))
    refused(archive.retain, str(d), match="already has an archive receipt")


def test_run_without_frozen_identity_refused(tmp_path):
    d = make_run(tmp_path)
    p = d / "observations-0.ndjson"
    p.write_bytes(p.read_bytes().replace(('"implementationSha":"%s"' % IMPL).encode(), b'"implementationSha":null', 1))
    refused(archive.retain, str(d), match="run identity incomplete")


# --- storage report -------------------------------------------------------------

def test_storage_report_classifies_runs_and_projects_sessions(tmp_path):
    active = make_run(tmp_path, close_last=False, run_id="stockspotter-vps-1-20261002T201000123Z-0")
    closed = make_run(tmp_path, run_id="stockspotter-vps-1-20261003T001000004Z-0")
    archived = make_run(tmp_path, run_id="stockspotter-vps-1-20261004T001000002Z-0")
    _, sha = archive.retain(str(archived))
    archive.delete_source(str(archived), sha)
    (tmp_path / "not-a-run").mkdir()
    free = archive.FLOOR_BYTES + 10 * archive.TYPICAL_SESSION_BYTES + 5
    r = archive.storage_report(str(tmp_path), free_bytes=free)
    assert r["runs"]["active"] == [active.name]
    assert r["runs"]["closedUnarchived"] == [closed.name]
    assert r["runs"]["archivedSourcesDeleted"] == [archived.name]
    assert r["closedUnarchivedBytes"] == sum(p.stat().st_size for p in closed.glob("observations-*.ndjson"))
    assert r["compressedArchiveBytes"] > 0
    assert r["aboveFloor"] and r["estimatedRemainingTypicalSessions"] == 10
    assert r["estimatedRemainingStressSessions"] == (10 * archive.TYPICAL_SESSION_BYTES + 5) // archive.STRESS_SESSION_BYTES
    below = archive.storage_report(str(tmp_path), free_bytes=archive.FLOOR_BYTES - 1)
    assert not below["aboveFloor"] and below["estimatedRemainingTypicalSessions"] == 0
