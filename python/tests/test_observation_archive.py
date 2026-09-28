"""Tests for ops/observation/observation_archive.py (Step 4B-preflight §12, §14)."""
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

# Pinned in the Rust suite too (`observation_preflight_tests.rs`), so the two
# RFC 8785 implementations are held to the same bytes.
FIXTURE_SHA256 = "0984c8f37ad115fab3fc1b9b70db72a1916f4f76f0fdf9e3fbc56d799ae66486"


def _write(path, records, close=True, terminate=True):
    lines = [json.dumps(r) for r in records]
    if close:
        lines.append(json.dumps({"kind": "file_close", "runId": "r", "fileName": path.name, "recordsWritten": len(records)}))
    body = "\n".join(lines) + ("\n" if terminate else "")
    path.write_text(body, encoding="utf-8")


def _sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def _run(tmp_path):
    run = tmp_path / "run"
    run.mkdir()
    rows = [{"kind": "receipt", "runId": "r", "sequence": i, "symbol": "S%d" % (i % 50)} for i in range(2000)]
    _write(run / "observations-0.ndjson", rows)
    _write(run / "observations-1.ndjson", rows[:10], close=False)  # active
    return run


def test_only_closed_files_are_compressed_and_sources_are_kept(tmp_path):
    run = _run(tmp_path)
    before = {p.name: _sha(p) for p in run.glob("*.ndjson")}
    done, open_files = archive.compress(str(run))
    assert done == ["observations-0.ndjson"]
    assert open_files == ["observations-1.ndjson"]
    assert not (run / "observations-1.ndjson.gz").exists(), "an active file is never compressed"
    assert {p.name: _sha(p) for p in run.glob("*.ndjson")} == before, "sources untouched"
    m = json.loads((run / archive.MANIFEST).read_text())
    e = m["files"]["observations-0.ndjson"]
    assert e["verified"] is True
    assert e["sourceSha256"] == before["observations-0.ndjson"]
    assert e["decompressedSha256"] == e["sourceSha256"]
    with gzip.open(run / "observations-0.ndjson.gz", "rb") as f:
        assert hashlib.sha256(f.read()).hexdigest() == e["sourceSha256"]
    assert archive.verify(str(run)) == []


def test_compression_is_idempotent_and_deterministic(tmp_path):
    run = _run(tmp_path)
    archive.compress(str(run))
    first = _sha(run / "observations-0.ndjson.gz")
    done, _ = archive.compress(str(run))
    assert done == [], "a verified file is not recompressed"
    assert _sha(run / "observations-0.ndjson.gz") == first


def test_a_tampered_archive_fails_verification(tmp_path):
    run = _run(tmp_path)
    archive.compress(str(run))
    gz = run / "observations-0.ndjson.gz"
    data = bytearray(gz.read_bytes())
    data[-12] ^= 0xFF
    gz.write_bytes(bytes(data))
    assert archive.verify(str(run)) != []


def test_unterminated_or_unclosed_files_are_not_closed(tmp_path):
    p = tmp_path / "observations-0.ndjson"
    _write(p, [{"kind": "receipt"}], close=True, terminate=False)
    assert not archive.is_closed(str(p))
    _write(p, [{"kind": "receipt"}], close=False)
    assert not archive.is_closed(str(p))
    _write(p, [{"kind": "receipt"}], close=True)
    assert archive.is_closed(str(p))


def test_the_fixture_preregistration_identity_matches_the_rust_implementation():
    fixture = ROOT / "ops" / "observation" / "step4-preregistration-v1.fixture.json"
    assert archive.prereg_sha(str(fixture)) == FIXTURE_SHA256


def test_canonical_bytes_refuse_floats_and_ignore_formatting():
    with pytest.raises(ValueError):
        archive.canonical_bytes({"floor": 0.3})
    a = archive.canonical_bytes(json.loads('{"b":1,"a":{"y":"x","x":[1,2]}}'))
    b = archive.canonical_bytes(json.loads('{ "a" : { "x" : [1, 2], "y" : "x" }, "b" : 1 }'))
    assert a == b == b'{"a":{"x":[1,2],"y":"x"},"b":1}'


def _cert(tmp_path, run_id, prereg="p" * 64, impl="i" * 40, verdict="PASS"):
    c = tmp_path / ("cert-" + prereg[:4] + ".json")
    c.write_text(json.dumps({"schema": "observation-certificate-v1", "verdict": verdict,
                             "certificate": {"runId": run_id, "preregistrationSha256": prereg, "implementationSha": impl}}))
    return c


def _closed_run(tmp_path):
    run = tmp_path / "srv-1-20260929T001000000Z-0"
    run.mkdir()
    rows = [{"kind": "receipt", "runId": "r", "sequence": i} for i in range(500)]
    _write(run / "observations-0.ndjson", rows)
    _write(run / "observations-1.ndjson", rows[:50])
    return run


def test_export_binds_identity_and_every_artifact_and_deletes_nothing(tmp_path):
    run = _closed_run(tmp_path)
    archive.compress(str(run))
    before = sorted(p.name for p in run.iterdir())
    r = archive.export(str(run), str(tmp_path / "dest"), str(_cert(tmp_path, run.name)), "2026-09-29", "p" * 64, "i" * 40)
    assert r["deletionEligible"] is True, r["conditions"]
    assert r["runId"] == run.name and r["session"] == "2026-09-29"
    assert r["preregistrationSha256"] == "p" * 64 and r["implementationSha"] == "i" * 40
    assert len(r["artifacts"]) == 2
    for a in r["artifacts"]:
        assert a["destinationSha256"] == a["compressedSha256"]
        assert len(a["sourceSha256"]) == 64
    assert sorted(p.name for p in run.iterdir()) == before, "the source run is untouched"
    assert archive.verify_export(r["destination"]) == []


def test_export_is_not_deletion_eligible_when_anything_is_missing_or_mismatched(tmp_path):
    run = _closed_run(tmp_path)
    archive.compress(str(run))
    r = archive.export(str(run), str(tmp_path / "d1"), str(_cert(tmp_path, run.name, prereg="q" * 64)), "s", "p" * 64, "i" * 40)
    assert r["deletionEligible"] is False and r["conditions"]["certificateBindsRunAndIdentity"] is False
    _write(run / "observations-2.ndjson", [{"kind": "receipt"}])
    r = archive.export(str(run), str(tmp_path / "d2"), str(_cert(tmp_path, run.name)), "s", "p" * 64, "i" * 40)
    assert r["deletionEligible"] is False and r["conditions"]["copyComplete"] is False
    assert r["unarchivedFiles"] == ["observations-2.ndjson"]


def test_a_tampered_export_fails_reverification(tmp_path):
    run = _closed_run(tmp_path)
    archive.compress(str(run))
    r = archive.export(str(run), str(tmp_path / "dest"), str(_cert(tmp_path, run.name)), "s", "p" * 64, "i" * 40)
    gz = pathlib.Path(r["destination"]) / r["artifacts"][0]["compressed"]
    data = bytearray(gz.read_bytes())
    data[-10] ^= 0xFF
    gz.write_bytes(bytes(data))
    assert archive.verify_export(r["destination"]) != []
