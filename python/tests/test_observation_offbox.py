"""Adversarial tests for off-box retention (GPT Phase-B support acceptance, 2026-10-01).

Evidence is built by the real retention chain (`retain` + `delete-source`) on
synthetic runs; nothing here uses campaign evidence.
"""
import datetime
import gzip
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]


def _load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "ops" / "observation" / f"{name}.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


offbox = _load("observation_offbox")
archive = offbox.archive

IMPL = "de5b55e66bd77b70fa7a026d178fb09634704264"
PREREG = "746446819af285d20a749a81eb27cf8984d7d488860beb4f8f575106942be7f8"
SESSION = "2026-10-05"
RUN_ID = "stockspotter-vps-1-20261005T001000400Z-0"


def _line(r):
    return json.dumps(r, separators=(",", ":"))


def make_run(root, run_id=RUN_ID, started="2026-10-05T00:10:00.400Z", files=3, impl=IMPL):
    d = root / run_id
    d.mkdir(parents=True)
    for i in range(files):
        name = f"observations-{i}.ndjson"
        recs = [{"kind": "run_start", "protocolVersion": "consumer-received-protocol-v1", "runId": run_id, "namespace": "stockspotter-vps",
                 "pid": 1, "startedAt": started, "implementationSha": impl, "preregistrationSha256": PREREG}] if i == 0 else \
               [{"kind": "file_start", "runId": run_id, "fileName": name, "sequence": i}]
        recs += [{"kind": "receipt", "runId": run_id, "sequence": i * 200 + k} for k in range(200)]
        lines = [_line(r) for r in recs]
        last = i == files - 1
        if last:
            lines.append(_line({"kind": "run_end", "runId": run_id, "endedAt": "2026-10-06T00:10:00.5Z", "counters": {},
                                "captureBytes": 1, "captureMaxBytes": 2, "status": {}}))
        close = {"kind": "file_close", "runId": run_id, "fileName": name, "recordsWritten": len(lines)}
        if not last:
            close["nextFile"] = f"observations-{i + 1}.ndjson"
        lines.append(_line(close))
        (d / name).write_bytes(("\n".join(lines) + "\n").encode())
    return d


def archived(tmp_path, **kw):
    """A fully archived VPS run (sources compressed, verified, deleted)."""
    vps = tmp_path / "vps"
    run = make_run(vps, **kw)
    _, sha = archive.retain(str(run))
    archive.delete_source(str(run), sha)
    return run


def export(tmp_path, run, session=SESSION):
    """attest-source on the 'VPS', copy, verify-copy on the 'workstation'."""
    att = offbox.attest_source(str(run))
    att_path = tmp_path / "attestation.json"
    att_path.write_text(json.dumps(att))
    dest = tmp_path / "H" / "step4-campaign" / "sessions" / session / "observation" / run.name
    shutil.copytree(run, dest)
    return att_path, dest


def refused(fn, *args, match, **kw):
    with pytest.raises(archive.Refusal, match=match):
        fn(*args, **kw)


def vps_snapshot(run):
    return {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(run.iterdir())}


# --- export -------------------------------------------------------------------

def test_complete_export_passes_and_binds_everything(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    rec, sha, existed = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    assert not existed and rec["verification"] == "PASS" and rec["decompressionVerified"]
    assert rec["schema"] == "observation-offbox-export-receipt-v1"
    assert rec["session"] == SESSION and rec["runId"] == RUN_ID
    assert rec["vpsSourcePath"] == os.path.abspath(run) and rec["offboxDestinationPath"] == os.path.abspath(dest)
    assert rec["archiveReceiptSha256"] == hashlib.sha256((run / archive.RETENTION_RECEIPT).read_bytes()).hexdigest()
    assert rec["deletionRecordSha256"] == hashlib.sha256((run / archive.DELETION_RECORD).read_bytes()).hexdigest()
    files = sorted(r["file"] for r in rec["artifacts"])
    assert files == sorted(p.name for p in run.iterdir())
    for r in rec["artifacts"]:
        assert r["sourceSha256"] == r["destinationSha256"]
    # The receipt lives outside the copied evidence, which stays an exact copy.
    rp = pathlib.Path(offbox.receipt_path(str(dest), RUN_ID))
    assert rp.parent.name == "receipts" and rp.parent.parent.name == SESSION
    assert hashlib.sha256(rp.read_bytes()).hexdigest() == sha
    assert (rp.parent / f"{RUN_ID}.source-attestation.json").read_bytes() == att.read_bytes()
    assert "secret" not in rp.read_text().lower()


def test_partial_copy_refused(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    os.remove(dest / "observations-1.ndjson.gz")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="missing")
    assert not pathlib.Path(offbox.receipt_path(str(dest), RUN_ID)).exists()


def test_corrupt_destination_refused(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    p = dest / "observations-0.ndjson.gz"
    b = bytearray(p.read_bytes())
    b[len(b) // 2] ^= 0xFF
    p.write_bytes(bytes(b))
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="differs from source")


def test_gzip_mismatch_refused_even_when_hashes_were_attested(tmp_path):
    # A source whose .gz and receipt were altered consistently: the hash checks
    # pass, decompression does not.
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    a = json.loads(att.read_text())
    bad = gzip.compress(b"not the source\n", mtime=0)
    (dest / "observations-0.ndjson.gz").write_bytes(bad)
    for x in a["artifacts"]:
        if x["file"] == "observations-0.ndjson.gz":
            x["sha256"], x["bytes"] = hashlib.sha256(bad).hexdigest(), len(bad)
    att.write_text(json.dumps(a))
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="gzip mismatch")
    # And on the VPS side, attest-source refuses the same tampering.
    (run / "observations-0.ndjson.gz").write_bytes(bad)
    refused(offbox.attest_source, str(run), match="compressed artifact changed")


def test_wrong_run_refused(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    other = dest.with_name("stockspotter-vps-1-20261006T001000400Z-0")
    os.rename(dest, other)
    refused(offbox.verify_copy, str(other), str(att), SESSION, IMPL, PREREG, match="wrong run")


def test_wrong_session_path_refused(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run, session="2026-10-06")
    refused(offbox.verify_copy, str(dest), str(att), "2026-10-06", IMPL, PREREG, match="belongs to session 2026-10-05")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="not <...>/sessions/<session>")


def test_wrong_identities_refused(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    refused(offbox.verify_copy, str(dest), str(att), SESSION, "0" * 40, PREREG, match="wrong identities")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, "0" * 64, match="wrong identities")


def test_extra_unknown_file_refused_both_sides(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    (dest / "notes.txt").write_text("x")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="not exactly the attested")
    (run / "observations-9.ndjson").write_text("{}\n")
    refused(offbox.attest_source, str(run), match="not exactly the archived")


def test_unarchived_or_active_run_cannot_be_attested(tmp_path):
    active = make_run(tmp_path / "vps")
    refused(offbox.attest_source, str(active), match="no archive receipt")
    run = make_run(tmp_path / "vps2")
    archive.retain(str(run))  # compressed, sources still present
    refused(offbox.attest_source, str(run), match="no deletion record")


def test_export_retry_and_idempotence(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    # A failed first attempt (partial copy) writes nothing; the retry succeeds.
    saved = (dest / "observations-2.ndjson.gz").read_bytes()
    os.remove(dest / "observations-2.ndjson.gz")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="missing")
    (dest / "observations-2.ndjson.gz").write_bytes(saved)
    rec1, sha1, existed1 = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    # Re-running is a no-op that returns the standing receipt.
    rec2, sha2, existed2 = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    assert (existed1, existed2) == (False, True) and sha1 == sha2 and rec1 == rec2
    # A different attestation for the same run cannot overwrite it.
    a = json.loads(att.read_text())
    a["attestedAt"] = "2026-10-07T00:00:00+00:00"
    att2 = tmp_path / "att2.json"
    att2.write_text(json.dumps(a))
    refused(offbox.verify_copy, str(dest), str(att2), SESSION, IMPL, PREREG, match="different attestation")


# --- reverify + future VPS compressed-archive deletion ----------------------------

def exported(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    _, sha, _ = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    rp = offbox.receipt_path(str(dest), RUN_ID)
    rv = tmp_path / "reverify.json"
    rv.write_text(json.dumps(offbox.reverify(rp)))
    return run, dest, rp, sha, rv


def test_exact_future_deletion_set(tmp_path):
    run, dest, rp, sha, rv = exported(tmp_path)
    gz = sorted(p.name for p in run.iterdir() if p.name.endswith(".gz"))
    rec = offbox.delete_archive(str(run), rp, sha, str(rv))
    assert rec["state"] == "complete" and sorted(rec["removed"]) == gz and rec["tombstoneKept"]
    assert sorted(p.name for p in run.iterdir()) == sorted(
        [archive.RETENTION_RECEIPT, archive.MANIFEST, archive.DELETION_RECORD, offbox.OFFBOX_DELETION])
    # The destination is untouched by a VPS deletion.
    assert sorted(p.name for p in dest.iterdir()) == sorted(gz + [archive.RETENTION_RECEIPT, archive.MANIFEST, archive.DELETION_RECORD])
    refused(offbox.delete_archive, str(run), rp, sha, str(rv), match="already exists")


def test_deletion_refuses_wrong_receipt_sha(tmp_path):
    run, _, rp, _, rv = exported(tmp_path)
    before = vps_snapshot(run)
    refused(offbox.delete_archive, str(run), rp, "0" * 64, str(rv), match="does not match the authorized")
    assert vps_snapshot(run) == before


def test_deletion_refuses_changed_or_missing_destination(tmp_path):
    run, dest, rp, sha, _ = exported(tmp_path)
    before = vps_snapshot(run)
    p = dest / "observations-0.ndjson.gz"
    good = p.read_bytes()
    p.write_bytes(good[:-1])
    refused(offbox.reverify, rp, match="differs from source")
    p.write_bytes(good)
    shutil.rmtree(dest)
    refused(offbox.reverify, rp, match="missing destination")
    # Without a fresh intact reverification, the VPS refuses.
    stale = tmp_path / "stale.json"
    stale.write_text(json.dumps({"schema": offbox.REVERIFY_SCHEMA, "offboxReceiptSha256": sha, "destinationIntact": True,
                                 "reverifiedAt": "2026-01-01T00:00:00+00:00"}))
    refused(offbox.delete_archive, str(run), rp, sha, str(stale), match="stale")
    other = tmp_path / "other.json"
    other.write_text(json.dumps({"schema": offbox.REVERIFY_SCHEMA, "offboxReceiptSha256": "1" * 64, "destinationIntact": True,
                                 "reverifiedAt": datetime.datetime.now(datetime.timezone.utc).isoformat()}))
    refused(offbox.delete_archive, str(run), rp, sha, str(other), match="no intact destination")
    assert vps_snapshot(run) == before


def test_deletion_refuses_changed_vps_evidence_and_unknown_files(tmp_path):
    run, _, rp, sha, rv = exported(tmp_path)
    (run / "stray.tmp").write_text("x")
    refused(offbox.delete_archive, str(run), rp, sha, str(rv), match="not exactly the archived")
    os.remove(run / "stray.tmp")
    p = run / "observations-1.ndjson.gz"
    b = bytearray(p.read_bytes())
    b[10] ^= 0x01
    p.write_bytes(bytes(b))
    refused(offbox.delete_archive, str(run), rp, sha, str(rv), match="compressed artifact changed")


def test_deletion_refuses_incomplete_export_and_other_runs(tmp_path):
    run, dest, rp, sha, rv = exported(tmp_path)
    rec = json.loads(pathlib.Path(rp).read_text())
    rec["verification"] = "PARTIAL"
    bad = tmp_path / "partial.json"
    bad.write_text(json.dumps(rec))
    refused(offbox.delete_archive, str(run), str(bad), hashlib.sha256(bad.read_bytes()).hexdigest(), str(rv), match="incomplete export")
    other = archived(tmp_path / "second", run_id="stockspotter-vps-1-20261006T001000400Z-0", started="2026-10-06T00:10:00.400Z")
    refused(offbox.delete_archive, str(other), rp, sha, str(rv), match="different run")
