"""Adversarial tests for off-box copy verification of archived observer runs.

Evidence is built by the real archive step (`retain`) on synthetic runs;
nothing here uses campaign evidence. The tool under test has no command that
deletes, and every command must leave the evidence it reads byte-identical.
"""
import gzip
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import shutil

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
TOOL = ROOT / "ops" / "observation" / "observation_offbox.py"


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


def archived(tmp_path, sources="present", **kw):
    """An archived run on the 'capture host'.

    `sources`: "present" is what `retain` leaves; "absent" and "recorded" stand
    in for a host where the uncompressed sources were later removed by
    something outside this repository (the TEST removes them), without and
    with the record such a removal may leave.
    """
    run = make_run(tmp_path / "host", **kw)
    _, sha = archive.retain(str(run))
    if sources != "present":
        names = sorted(p.name for p in run.glob("observations-*.ndjson"))
        for n in names:
            os.remove(run / n)
        if sources == "recorded":
            (run / archive.DELETION_RECORD).write_text(json.dumps({"state": "complete", "receiptSha256": sha, "removed": names}))
    return run


def export(tmp_path, run, session=SESSION):
    """attest-source on the 'capture host', copy the attested files, ready for verify-copy on the 'second machine'."""
    att = offbox.attest_source(str(run))
    att_path = tmp_path / "attestation.json"
    att_path.write_text(json.dumps(att))
    dest = tmp_path / "campaign" / "sessions" / session / "observation" / run.name
    dest.mkdir(parents=True)
    for a in att["artifacts"]:
        shutil.copyfile(run / a["file"], dest / a["file"])
    return att_path, dest


def refused(fn, *args, match, **kw):
    with pytest.raises(archive.Refusal, match=match):
        fn(*args, **kw)


def snapshot(d):
    out = {}
    for p in sorted(pathlib.Path(d).rglob("*")):
        if p.is_symlink():
            out[p.relative_to(d).as_posix()] = "link"
        elif p.is_file():
            out[p.relative_to(d).as_posix()] = hashlib.sha256(p.read_bytes()).hexdigest()
    return out


def symlink(target, link):
    try:
        os.symlink(target, link)
    except (OSError, NotImplementedError) as e:
        pytest.skip(f"this host cannot create symlinks: {e}")


# --- attest-source ---------------------------------------------------------------

def test_attestation_covers_the_artifacts_and_reports_the_sources(tmp_path):
    run = archived(tmp_path)
    before = snapshot(run)
    att = offbox.attest_source(str(run))
    gz = sorted(p.name for p in run.glob("*.gz"))
    assert sorted(a["file"] for a in att["artifacts"]) == sorted(gz + [archive.MANIFEST, archive.RETENTION_RECEIPT])
    assert att["verification"]["rawSourcesPresent"] is True and att["verification"]["rawSourcesMatchReceipt"] is True
    assert att["deletionRecordSha256"] is None
    assert snapshot(run) == before


@pytest.mark.parametrize("sources", ["absent", "recorded"])
def test_a_run_whose_sources_are_gone_is_still_attestable(tmp_path, sources):
    run = archived(tmp_path, sources=sources)
    att = offbox.attest_source(str(run))
    assert att["verification"]["rawSourcesPresent"] is False and att["verification"]["rawSourcesMatchReceipt"] is None
    if sources == "recorded":
        assert att["deletionRecordSha256"] == hashlib.sha256((run / archive.DELETION_RECORD).read_bytes()).hexdigest()
        assert archive.DELETION_RECORD in [a["file"] for a in att["artifacts"]]
    else:
        assert att["deletionRecordSha256"] is None


def test_sources_must_be_all_present_and_unchanged_or_all_absent(tmp_path):
    run = archived(tmp_path)
    p = run / "observations-1.ndjson"
    original = p.read_bytes()
    p.write_bytes(original.replace(b'"sequence":201', b'"sequence":999', 1))
    refused(offbox.attest_source, str(run), match="uncompressed source differs")
    p.write_bytes(original)
    os.remove(run / "observations-2.ndjson")
    refused(offbox.attest_source, str(run), match="not exactly the receipt's sources")


def test_a_removal_record_must_be_complete_bound_and_consistent(tmp_path):
    run = archived(tmp_path, sources="recorded")
    rec = run / archive.DELETION_RECORD
    good = json.loads(rec.read_text())
    for change in [{"state": "in-progress"}, {"receiptSha256": "0" * 64}, {"removed": ["observations-0.ndjson"]}]:
        rec.write_text(json.dumps({**good, **change}))
        refused(offbox.attest_source, str(run), match="incomplete or not bound")
    rec.write_text("not json")
    refused(offbox.attest_source, str(run), match="malformed source-removal record")
    # A record claiming removal while sources are present.
    (tmp_path / "b").mkdir()
    run2 = archived(tmp_path / "b")
    sha = hashlib.sha256((run2 / archive.RETENTION_RECEIPT).read_bytes()).hexdigest()
    names = sorted(p.name for p in run2.glob("observations-*.ndjson"))
    (run2 / archive.DELETION_RECORD).write_text(json.dumps({"state": "complete", "receiptSha256": sha, "removed": names}))
    refused(offbox.attest_source, str(run2), match="uncompressed sources exist")


def test_unarchived_or_active_run_cannot_be_attested(tmp_path):
    active = make_run(tmp_path / "host")
    refused(offbox.attest_source, str(active), match="no archive receipt")
    refused(offbox.attest_source, str(tmp_path / "host" / "observation-run"), match="ambiguous run directory")


# --- verify-copy -------------------------------------------------------------------

@pytest.mark.parametrize("sources", ["present", "absent", "recorded"])
def test_complete_export_passes_and_binds_everything(tmp_path, sources):
    run = archived(tmp_path, sources=sources)
    att, dest = export(tmp_path, run)
    host_before, dest_before = snapshot(run), snapshot(dest)
    rec, sha, existed = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    assert not existed and rec["verification"] == "PASS" and rec["decompressionVerified"]
    assert rec["schema"] == "observation-offbox-export-receipt-v1"
    assert rec["session"] == SESSION and rec["runId"] == RUN_ID
    assert rec["sourcePath"] == os.path.abspath(run) and rec["offboxDestinationPath"] == os.path.abspath(dest)
    assert rec["archiveReceiptSha256"] == hashlib.sha256((run / archive.RETENTION_RECEIPT).read_bytes()).hexdigest()
    if sources == "recorded":
        assert rec["deletionRecordSha256"] == hashlib.sha256((run / archive.DELETION_RECORD).read_bytes()).hexdigest()
    else:
        assert rec["deletionRecordSha256"] is None
    assert sorted(r["file"] for r in rec["artifacts"]) == sorted(p.name for p in dest.iterdir())
    for r in rec["artifacts"]:
        assert r["sourceSha256"] == r["destinationSha256"]
    if sources != "recorded":  # nothing in the receipt speaks of deleting anything
        assert "delet" not in json.dumps({k: v for k, v in rec.items() if k != "deletionRecordSha256"}).lower()
    # The receipt lives outside the copied evidence, which stays an exact copy.
    rp = pathlib.Path(offbox.receipt_path(str(dest), RUN_ID))
    assert rp.parent.name == "receipts" and rp.parent.parent.name == SESSION
    assert hashlib.sha256(rp.read_bytes()).hexdigest() == sha
    assert (rp.parent / f"{RUN_ID}.source-attestation.json").read_bytes() == att.read_bytes()
    assert "secret" not in rp.read_text().lower()
    assert snapshot(run) == host_before and snapshot(dest) == dest_before


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
    # A source whose .gz and attestation were altered consistently: the hash
    # checks pass, decompression does not.
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
    # And on the capture host, attest-source refuses the same tampering.
    (run / "observations-0.ndjson.gz").write_bytes(bad)
    refused(offbox.attest_source, str(run), match="compressed artifact changed")


def test_a_damaged_deflate_stream_is_a_refusal_not_a_crash(tmp_path):
    run = archived(tmp_path, sources="absent")
    gz = run / "observations-0.ndjson.gz"
    b = bytearray(gz.read_bytes())
    b[len(b) // 2] ^= 0xFF
    gz.write_bytes(bytes(b))
    # Make the receipt vouch for the damaged bytes, so only decompression can object.
    rp = run / archive.RETENTION_RECEIPT
    r = json.loads(rp.read_text())
    c = next(c for c in r["compressed"] if c["file"] == gz.name)
    c["sha256"], c["bytes"] = hashlib.sha256(bytes(b)).hexdigest(), len(b)
    rp.write_text(json.dumps(r))
    refused(offbox.attest_source, str(run), match="corrupt compressed artifact|gzip mismatch")


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
    (run / "notes.txt").write_text("x")
    refused(offbox.attest_source, str(run), match="not exactly the archived")


def test_export_retry_and_idempotence(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    # A failed first attempt (partial copy) writes nothing; the retry succeeds.
    saved = (dest / "observations-2.ndjson.gz").read_bytes()
    os.remove(dest / "observations-2.ndjson.gz")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="missing")
    assert not (dest.parent.parent / "receipts").exists()
    (dest / "observations-2.ndjson.gz").write_bytes(saved)
    rec1, sha1, existed1 = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    receipts_before = snapshot(dest.parent.parent / "receipts")
    # Re-running is a no-op that returns the standing receipt.
    rec2, sha2, existed2 = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    assert (existed1, existed2) == (False, True) and sha1 == sha2 and rec1 == rec2
    # A different attestation for the same run cannot overwrite it.
    a = json.loads(att.read_text())
    a["attestedAt"] = "2026-10-07T00:00:00+00:00"
    att2 = tmp_path / "att2.json"
    att2.write_text(json.dumps(a))
    refused(offbox.verify_copy, str(dest), str(att2), SESSION, IMPL, PREREG, match="different attestation")
    assert snapshot(dest.parent.parent / "receipts") == receipts_before


def test_a_kept_attestation_is_never_overwritten(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    receipts = dest.parent.parent / "receipts"
    receipts.mkdir()
    kept = receipts / f"{RUN_ID}.source-attestation.json"
    kept.write_text("{}")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="different source attestation")
    assert kept.read_text() == "{}" and sorted(p.name for p in receipts.iterdir()) == [kept.name]
    # The same bytes left by an interrupted attempt are fine.
    kept.write_bytes(att.read_bytes())
    rec, _, existed = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    assert not existed and rec["verification"] == "PASS"


# --- names and links ---------------------------------------------------------------

@pytest.mark.parametrize("name", ["../observations-0.ndjson.gz", "sub/observations-0.ndjson.gz", "/etc/passwd",
                                  "C:\\observations-0.ndjson.gz", "C:observations-0.ndjson.gz", "..", ""])
def test_attested_names_cannot_leave_the_destination(tmp_path, name):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    a = json.loads(att.read_text())
    next(x for x in a["artifacts"] if x["file"] == "observations-0.ndjson.gz")["file"] = name
    att.write_text(json.dumps(a))
    before = snapshot(tmp_path)
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="unsafe file name")
    assert snapshot(tmp_path) == before


def test_receipt_names_cannot_leave_the_run_directory(tmp_path):
    run = archived(tmp_path, sources="absent")
    outside = tmp_path / "observations-0.ndjson.gz"
    outside.write_bytes((run / "observations-0.ndjson.gz").read_bytes())  # byte-identical: only the name check can refuse
    rp = run / archive.RETENTION_RECEIPT
    good = json.loads(rp.read_text())
    for bad in ["../../observations-0.ndjson.gz", str(outside)]:
        r = json.loads(json.dumps(good))
        r["compressed"][0]["file"] = bad
        rp.write_text(json.dumps(r))
        refused(offbox.attest_source, str(run), match="unsafe file name")
    r = json.loads(json.dumps(good))
    r["sources"][0]["file"] = "../observations-0.ndjson"
    r["compressed"][0]["source"] = "../observations-0.ndjson"
    rp.write_text(json.dumps(r))
    refused(offbox.attest_source, str(run), match="unsafe file name")
    # An attestation naming a run id that is really a path.
    rp.write_text(json.dumps(good))
    att, dest = export(tmp_path, run)
    a = json.loads(att.read_text())
    a["runId"] = "../../../outside"
    att.write_text(json.dumps(a))
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="names no valid run")


def test_symlinks_are_refused_on_both_sides(tmp_path):
    # Capture host: archived_run rejects a linked artifact exactly as closed_run rejects a linked source.
    run = archived(tmp_path, sources="absent")
    moved = tmp_path / "elsewhere.gz"
    os.rename(run / "observations-0.ndjson.gz", moved)
    symlink(moved, run / "observations-0.ndjson.gz")
    before = snapshot(tmp_path)
    refused(offbox.attest_source, str(run), match="not a regular file")
    assert snapshot(tmp_path) == before
    os.remove(run / "observations-0.ndjson.gz")  # the test's own link
    os.rename(moved, run / "observations-0.ndjson.gz")
    # A linked receipt.
    moved_receipt = tmp_path / "receipt-elsewhere.json"
    os.rename(run / archive.RETENTION_RECEIPT, moved_receipt)
    symlink(moved_receipt, run / archive.RETENTION_RECEIPT)
    refused(offbox.attest_source, str(run), match="not a regular file")
    os.remove(run / archive.RETENTION_RECEIPT)
    os.rename(moved_receipt, run / archive.RETENTION_RECEIPT)
    # Second machine: a linked artifact in the copy.
    att, dest = export(tmp_path, run)
    moved = tmp_path / "copy-elsewhere.gz"
    os.rename(dest / "observations-1.ndjson.gz", moved)
    symlink(moved, dest / "observations-1.ndjson.gz")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="not a regular file")
    # A run directory that is a link.
    link = tmp_path / "host" / "stockspotter-vps-1-20261009T001000400Z-0"
    try:
        os.symlink(run, link, target_is_directory=True)
    except (OSError, NotImplementedError) as e:
        pytest.skip(f"this host cannot create directory symlinks: {e}")
    refused(offbox.attest_source, str(link), match="ambiguous run directory")


# --- bounded reads -------------------------------------------------------------------

def test_size_caps_refuse_on_both_sides(tmp_path, monkeypatch, capsys):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    before = snapshot(tmp_path)
    monkeypatch.setenv(archive.ENV_MAX_DECOMPRESSED, "100")
    refused(offbox.attest_source, str(run), match="decompressed-size cap exceeded")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="decompressed-size cap exceeded")
    assert offbox.main(["x", "attest-source", str(run)]) == 1
    assert "decompressed-size cap exceeded" in capsys.readouterr().out
    monkeypatch.delenv(archive.ENV_MAX_DECOMPRESSED)
    monkeypatch.setenv(archive.ENV_MAX_COMPRESSED, "100")
    refused(offbox.attest_source, str(run), match="compressed-size cap exceeded")
    refused(offbox.verify_copy, str(dest), str(att), SESSION, IMPL, PREREG, match="compressed-size cap exceeded")
    assert offbox.main(["x", "verify-copy", str(dest), str(att), SESSION, IMPL, PREREG]) == 1
    assert "compressed-size cap exceeded" in capsys.readouterr().out
    assert snapshot(tmp_path) == before


# --- reverify -----------------------------------------------------------------------

def exported(tmp_path):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    _, sha, _ = offbox.verify_copy(str(dest), str(att), SESSION, IMPL, PREREG)
    return run, dest, offbox.receipt_path(str(dest), RUN_ID), sha


def test_reverify_confirms_an_intact_copy_and_changes_nothing(tmp_path):
    _, dest, rp, sha = exported(tmp_path)
    before = snapshot(tmp_path)
    rv = offbox.reverify(rp)
    assert rv["schema"] == offbox.REVERIFY_SCHEMA and rv["destinationIntact"] is True and rv["offboxReceiptSha256"] == sha
    assert rv["artifacts"] == len(list(dest.iterdir()))
    assert snapshot(tmp_path) == before


def test_reverify_refuses_a_changed_or_missing_destination(tmp_path):
    _, dest, rp, _ = exported(tmp_path)
    p = dest / "observations-0.ndjson.gz"
    good = p.read_bytes()
    p.write_bytes(good[:-1])
    refused(offbox.reverify, rp, match="differs from source")
    p.write_bytes(good)
    (dest / "stray.tmp").write_text("x")
    refused(offbox.reverify, rp, match="not exactly the attested")
    shutil.rmtree(dest)  # the test discards its own copy
    refused(offbox.reverify, rp, match="missing destination")


def test_reverify_follows_only_the_destination_beside_the_receipt(tmp_path):
    run, dest, rp, _ = exported(tmp_path)
    # A receipt that records some other directory -- here the capture host's own
    # run, which would even verify -- is not followed.
    rec = json.loads(pathlib.Path(rp).read_text())
    elsewhere = tmp_path / "elsewhere.json"
    elsewhere.write_text(json.dumps(rec))
    refused(offbox.reverify, str(elsewhere), match="not beside the destination")
    rec["offboxDestinationPath"] = os.path.abspath(run)
    elsewhere.write_text(json.dumps(rec))
    refused(offbox.reverify, str(elsewhere), match="not beside the destination")
    for bad in ["relative/path", "", None, os.path.abspath(dest.parent)]:
        rec["offboxDestinationPath"] = bad
        elsewhere.write_text(json.dumps(rec))
        refused(offbox.reverify, str(elsewhere), match="not beside the destination")
    refused(offbox.reverify, str(run / archive.RETENTION_RECEIPT), match="not an off-box export receipt")


# --- no deletion, and nothing changed ---------------------------------------------

@pytest.mark.parametrize("command", ["delete-archive", "delete-source", "delete", "remove", "rm", "prune", "purge", "truncate", "clean"])
def test_there_is_no_deleting_command(tmp_path, command, capsys):
    run, dest, rp, sha = exported(tmp_path)
    before = snapshot(tmp_path)
    for args in ([], [str(run)], [str(run), rp], [str(run), rp, sha], [str(run), rp, sha, rp], [str(run), rp, sha, rp, "x"]):
        assert offbox.main(["observation_offbox.py", command, *args]) == 2, (command, args)
        assert "Commands:" in capsys.readouterr().out  # the usage text, not a result
    assert snapshot(tmp_path) == before


def test_the_tool_has_no_deletion_code():
    assert not hasattr(offbox, "delete_archive") and not hasattr(offbox, "OFFBOX_DELETION")
    source = TOOL.read_text(encoding="utf-8")
    for forbidden in ["os.remove", "os.unlink", "shutil", "os.rmdir", "os.removedirs", "os.truncate", ".truncate(", ".unlink(",
                      "os.replace", "os.rename", "delete-archive", "delete_archive"]:
        assert forbidden not in source, forbidden
    # It opens nothing for writing itself; the two files it creates go through the exclusive writer.
    assert not re.search(r"\bopen\(", source)
    assert source.count("_create_file_durably(") + source.count("_create_json_durably(") == 2


def test_every_command_leaves_the_evidence_byte_identical(tmp_path, capsys):
    run = archived(tmp_path)
    att, dest = export(tmp_path, run)
    host_before, dest_before = snapshot(run), snapshot(dest)
    assert offbox.main(["x", "attest-source", str(run)]) == 0
    assert offbox.main(["x", "verify-copy", str(dest), str(att), SESSION, IMPL, PREREG]) == 0
    rp = offbox.receipt_path(str(dest), RUN_ID)
    receipts_before = snapshot(pathlib.Path(rp).parent)
    assert sorted(receipts_before) == [f"{RUN_ID}.offbox-export-receipt.json", f"{RUN_ID}.source-attestation.json"]
    assert offbox.main(["x", "verify-copy", str(dest), str(att), SESSION, IMPL, PREREG]) == 0  # standing receipt
    assert offbox.main(["x", "reverify", rp]) == 0
    assert offbox.main(["x", "attest-source", str(dest)]) == 0   # the copy is itself an archived run, sources absent
    assert offbox.main(["x", "reverify", str(att)]) == 1         # not a receipt
    assert offbox.main(["x", "delete-archive", str(run), rp, "0" * 64, rp]) == 2
    capsys.readouterr()
    assert snapshot(run) == host_before
    assert snapshot(dest) == dest_before
    assert snapshot(pathlib.Path(rp).parent) == receipts_before
