"""Adversarial tests for observer-run retention: closed run -> compress -> verify -> receipt.

`retain` only adds files. Every refusal, and every success, must leave the
bytes that were already there exactly as they were, and the tool must have no
command that deletes.
"""
import gzip
import hashlib
import importlib.util
import json
import os
import pathlib
import re

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
TOOL = ROOT / "ops" / "observation" / "observation_archive.py"
_spec = importlib.util.spec_from_file_location("observation_archive", TOOL)
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
    """Relative path -> SHA-256 of every file under `d`. Links are recorded, not followed."""
    out = {}
    for p in sorted(pathlib.Path(d).rglob("*")):
        if p.is_symlink():
            out[p.relative_to(d).as_posix()] = "link"
        elif p.is_file():
            out[p.relative_to(d).as_posix()] = hashlib.sha256(p.read_bytes()).hexdigest()
    return out


def refused(fn, *args, match):
    with pytest.raises(archive.Refusal, match=match):
        fn(*args)


def symlink(target, link):
    try:
        os.symlink(target, link)
    except (OSError, NotImplementedError) as e:
        pytest.skip(f"this host cannot create symlinks: {e}")


# --- closure ----------------------------------------------------------------

def test_active_run_refused(tmp_path):
    d = make_run(tmp_path, close_last=False)
    before = snapshot(d)
    refused(archive.retain, str(d), match="active run")
    assert snapshot(d) == before and not (d / archive.RETENTION_RECEIPT).exists()


def test_missing_run_end_refused(tmp_path):
    d = make_run(tmp_path, run_end=False)
    before = snapshot(d)
    refused(archive.retain, str(d), match="missing run_end")
    assert snapshot(d) == before


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
    os.remove(d / "observations-1.ndjson")  # the test damages its own fixture
    refused(archive.retain, str(d), match="not contiguous")


def test_unknown_file_refused_before_anything(tmp_path):
    d = make_run(tmp_path)
    (d / "observations-0.ndjson.close-failed").write_text("x")
    before = snapshot(d)
    refused(archive.retain, str(d), match="unknown files")
    assert snapshot(d) == before


def test_duplicate_run_end_refused(tmp_path):
    d = make_run(tmp_path)
    p = d / "observations-0.ndjson"
    lines = p.read_bytes().split(b"\n")
    lines.insert(2, _line({"kind": "run_end", "runId": RUN_ID}).encode())
    p.write_bytes(b"\n".join(lines))
    refused(archive.retain, str(d), match="2 run_end records")


def test_run_without_frozen_identity_refused(tmp_path):
    d = make_run(tmp_path)
    p = d / "observations-0.ndjson"
    p.write_bytes(p.read_bytes().replace(('"implementationSha":"%s"' % IMPL).encode(), b'"implementationSha":null', 1))
    refused(archive.retain, str(d), match="run identity incomplete")


# --- retain adds, and changes nothing ------------------------------------------

def test_retain_round_trip_adds_files_and_leaves_every_source_byte_identical(tmp_path):
    d = make_run(tmp_path, files=3)
    originals = {p.name: p.read_bytes() for p in d.iterdir()}
    before = snapshot(d)
    receipt, sha = archive.retain(str(d))
    assert receipt["runId"] == RUN_ID and receipt["implementationSha"] == IMPL and receipt["preregistrationSha256"] == PREREG
    assert [s["file"] for s in receipt["sources"]] == sorted(originals)
    assert receipt["runEnd"]["kind"] == "run_end" and receipt["runStart"]["kind"] == "run_start"
    # The receipt describes what was done and promises no later step.
    assert "deletion" not in receipt and "delet" not in json.dumps(receipt).lower()
    after = snapshot(d)
    assert {n: after[n] for n in before} == before, "every source is byte-identical"
    assert sorted(after) == sorted(list(originals) + [n + ".gz" for n in originals] + [archive.MANIFEST, archive.RETENTION_RECEIPT])
    for n, b in originals.items():
        assert gzip.decompress((d / (n + ".gz")).read_bytes()) == b
    assert hashlib.sha256((d / archive.RETENTION_RECEIPT).read_bytes()).hexdigest() == sha
    assert archive.verify(str(d)) == []


def test_retain_is_not_repeated_and_the_receipt_is_never_rewritten(tmp_path):
    d = make_run(tmp_path)
    archive.retain(str(d))
    before = snapshot(d)
    refused(archive.retain, str(d), match="already has an archive receipt")
    # The writer itself cannot replace a file, whoever calls it.
    refused(archive._create_json_durably, str(d / archive.RETENTION_RECEIPT), {"x": 1}, match="never overwritten")
    refused(archive._create_file_durably, str(d / "observations-0.ndjson"), b"", match="never overwritten")
    assert snapshot(d) == before


def test_receipt_write_failure_leaves_the_sources_untouched(tmp_path, monkeypatch):
    d = make_run(tmp_path)
    before = snapshot(d)

    def fail(path, value):
        raise OSError("disk full")
    monkeypatch.setattr(archive, "_create_json_durably", fail)
    with pytest.raises(OSError):
        archive.retain(str(d))
    monkeypatch.undo()
    after = snapshot(d)
    assert {n: after[n] for n in before} == before and not (d / archive.RETENTION_RECEIPT).exists()
    # The artifacts it had finished are complete and verified, so a retry only adds the receipt.
    archive.retain(str(d))
    final = snapshot(d)
    assert {n: final[n] for n in after} == after


def test_retain_never_replaces_existing_archive_output(tmp_path):
    # A stray artifact with no manifest: finishing would overwrite it.
    d = make_run(tmp_path)
    (d / "observations-0.ndjson.gz").write_bytes(b"left by something else")
    before = snapshot(d)
    refused(archive.retain, str(d), match="partially archived")
    assert snapshot(d) == before
    # A manifest with an unverified entry: finishing would rewrite it.
    (tmp_path / "b").mkdir()
    d2 = make_run(tmp_path / "b")
    archive.compress(str(d2))
    m = json.loads((d2 / archive.MANIFEST).read_text())
    m["files"]["observations-1.ndjson"]["verified"] = False
    (d2 / archive.MANIFEST).write_text(json.dumps(m))
    before = snapshot(d2)
    refused(archive.retain, str(d2), match="partially archived")
    assert snapshot(d2) == before
    # A complete, verified earlier `compress`: retain adds the receipt and nothing else changes.
    (tmp_path / "c").mkdir()
    d3 = make_run(tmp_path / "c")
    archive.compress(str(d3))
    before = snapshot(d3)
    archive.retain(str(d3))
    after = snapshot(d3)
    assert {n: after[n] for n in before} == before
    assert sorted(set(after) - set(before)) == [archive.RETENTION_RECEIPT]


# --- names from a manifest or receipt ---------------------------------------------

@pytest.mark.parametrize("name", [
    "../outside.ndjson.gz", "..", "", ".", "sub/observations-0.ndjson.gz", "sub\\observations-0.ndjson.gz",
    "/etc/passwd", "C:\\Windows\\win.ini", "C:observations-0.ndjson.gz", "\\\\host\\share\\x.gz", "x:stream", "a\0b", None, 7,
])
def test_unsafe_names_are_refused(name):
    refused(archive._bare_name, name, match="unsafe file name")


def test_manifest_names_cannot_leave_the_run_directory(tmp_path):
    d = make_run(tmp_path)
    archive.compress(str(d))
    outside = tmp_path / "outside.ndjson.gz"
    outside.write_bytes((d / "observations-0.ndjson.gz").read_bytes())  # byte-identical: only the name check can refuse
    manifest = json.loads((d / archive.MANIFEST).read_text())
    for bad in ["../outside.ndjson.gz", str(outside), "sub/../observations-0.ndjson.gz"]:
        m = json.loads(json.dumps(manifest))
        m["files"]["observations-0.ndjson"]["compressed"] = bad
        (d / archive.MANIFEST).write_text(json.dumps(m))
        before = snapshot(tmp_path)
        failures = archive.verify(str(d))
        assert len(failures) == 1 and "unsafe file name" in failures[0], (bad, failures)
        assert archive.main(["x", "verify", str(d)]) == 1
        refused(archive.export, str(d), str(tmp_path / "dest"), str(d / archive.MANIFEST), "2026-10-03", PREREG, IMPL, match="unsafe file name")
        assert snapshot(tmp_path) == before


def test_receipt_names_are_validated_before_use():
    ok = {"sources": [{"file": "observations-0.ndjson"}], "compressed": [{"file": "observations-0.ndjson.gz", "source": "observations-0.ndjson"}]}
    assert len(archive.receipt_pairs(ok)) == 1
    for sources, compressed, why in [
        ([{"file": "../observations-0.ndjson"}], [{"file": "../observations-0.ndjson.gz", "source": "../observations-0.ndjson"}], "unsafe file name"),
        ([{"file": "observations-0.ndjson"}], [{"file": "/tmp/observations-0.ndjson.gz", "source": "observations-0.ndjson"}], "unsafe file name"),
        ([{"file": "observations-0.ndjson"}], [{"file": "observations-1.ndjson.gz", "source": "observations-0.ndjson"}], "not exactly"),
        ([{"file": "notes.txt"}], [{"file": "notes.txt.gz", "source": "notes.txt"}], "not an observation file"),
        ([], [], "do not pair up"),
        ([{"file": "observations-0.ndjson"}], [], "do not pair up"),
    ]:
        refused(archive.receipt_pairs, {"sources": sources, "compressed": compressed}, match=why)


def test_symlinks_are_refused_as_sources_and_as_artifacts(tmp_path):
    # A source that is a link: the run is not recognisable, so nothing is archived.
    d = make_run(tmp_path)
    real = tmp_path / "elsewhere.ndjson"
    os.rename(d / "observations-1.ndjson", real)
    symlink(real, d / "observations-1.ndjson")
    before = snapshot(tmp_path)
    refused(archive.retain, str(d), match="unknown files")
    assert snapshot(tmp_path) == before
    # An artifact that is a link, to bytes the manifest vouches for.
    (tmp_path / "b").mkdir()
    d2 = make_run(tmp_path / "b")
    archive.compress(str(d2))
    moved = tmp_path / "elsewhere.gz"
    os.rename(d2 / "observations-0.ndjson.gz", moved)
    symlink(moved, d2 / "observations-0.ndjson.gz")
    before = snapshot(tmp_path)
    failures = archive.verify(str(d2))
    assert len(failures) == 1 and "not a regular file" in failures[0]
    refused(archive.retain, str(d2), match="unknown files")
    assert snapshot(tmp_path) == before
    # A run directory that is itself a link.
    link = tmp_path / "stockspotter-vps-1-20261009T201000123Z-0"
    try:
        os.symlink(d2, link, target_is_directory=True)
    except (OSError, NotImplementedError) as e:
        pytest.skip(f"this host cannot create directory symlinks: {e}")
    refused(archive.retain, str(link), match="not a run directory")


# --- bounded reads -------------------------------------------------------------------

def test_decompressed_size_cap(tmp_path, monkeypatch, capsys):
    d = make_run(tmp_path)
    archive.compress(str(d))
    gz = d / "observations-0.ndjson.gz"
    size = (d / "observations-0.ndjson").stat().st_size
    assert archive._gunzip_sha(str(gz), max_bytes=size)[1] == size
    refused(archive._gunzip_sha, str(gz), size - 1, match="decompressed-size cap exceeded")
    # Through the override, on every path that decompresses.
    before = snapshot(d)
    monkeypatch.setenv(archive.ENV_MAX_DECOMPRESSED, "100")
    refused(archive.verify, str(d), match="decompressed-size cap exceeded")
    assert archive.main(["x", "verify", str(d)]) == 1
    assert "decompressed-size cap exceeded" in capsys.readouterr().out
    refused(archive.retain, str(d), match="decompressed-size cap exceeded")
    assert snapshot(d) == before
    # A source over the cap is refused before anything is written beside it.
    (tmp_path / "b").mkdir()
    fresh = make_run(tmp_path / "b")
    before = snapshot(fresh)
    refused(archive.retain, str(fresh), match="decompressed-size cap exceeded")
    assert archive.main(["x", "compress", str(fresh)]) == 1
    assert snapshot(fresh) == before
    # A cap that is not a positive integer is an error, not a silent default.
    for bad in ["0", "-5", "ten", "", "1e9"]:
        monkeypatch.setenv(archive.ENV_MAX_DECOMPRESSED, bad)
        refused(archive.verify, str(d), match="must be a positive integer")
    monkeypatch.delenv(archive.ENV_MAX_DECOMPRESSED)
    assert archive.verify(str(d)) == []


def test_compressed_size_cap(tmp_path, monkeypatch, capsys):
    d = make_run(tmp_path)
    archive.retain(str(d))
    before = snapshot(d)
    smallest = min(p.stat().st_size for p in d.glob("*.gz"))
    monkeypatch.setenv(archive.ENV_MAX_COMPRESSED, str(smallest - 1))
    refused(archive.verify, str(d), match="compressed-size cap exceeded")
    assert archive.main(["x", "verify", str(d)]) == 1
    assert "compressed-size cap exceeded" in capsys.readouterr().out
    monkeypatch.setenv(archive.ENV_MAX_COMPRESSED, str(max(p.stat().st_size for p in d.glob("*.gz"))))
    assert archive.verify(str(d)) == []
    assert snapshot(d) == before
    # A fresh compression refused by the cap leaves no temporary behind to block the run,
    # and the sources are as they were.
    (tmp_path / "b").mkdir()
    fresh = make_run(tmp_path / "b")
    sources_before = snapshot(fresh)
    monkeypatch.setenv(archive.ENV_MAX_COMPRESSED, "10")
    refused(archive.retain, str(fresh), match="compressed-size cap exceeded")
    after = snapshot(fresh)
    assert {n: after[n] for n in sources_before} == sources_before
    assert sorted(set(after) - set(sources_before)) == [archive.MANIFEST]


def test_default_caps_admit_a_real_session():
    assert archive.DEFAULT_MAX_DECOMPRESSED_BYTES >= 16 * 1024 ** 3 > archive.STRESS_SESSION_BYTES > archive.TYPICAL_SESSION_BYTES
    assert archive.DEFAULT_MAX_COMPRESSED_BYTES >= archive.DEFAULT_MAX_DECOMPRESSED_BYTES


# --- storage report -------------------------------------------------------------

def test_storage_report_classifies_runs_and_projects_sessions(tmp_path):
    active = make_run(tmp_path, close_last=False, run_id="stockspotter-vps-1-20261002T201000123Z-0")
    closed = make_run(tmp_path, run_id="stockspotter-vps-1-20261003T001000004Z-0")
    archived = make_run(tmp_path, run_id="stockspotter-vps-1-20261004T001000002Z-0")
    archive.retain(str(archived))
    gone = make_run(tmp_path, run_id="stockspotter-vps-1-20261005T001000002Z-0")
    archive.retain(str(gone))
    for p in gone.glob("observations-*.ndjson"):
        os.remove(p)  # the TEST removes them, standing in for whatever does so outside this tool
    (tmp_path / "not-a-run").mkdir()
    before = snapshot(tmp_path)
    free = archive.FLOOR_BYTES + 10 * archive.TYPICAL_SESSION_BYTES + 5
    r = archive.storage_report(str(tmp_path), free_bytes=free)
    assert r["runs"]["active"] == [active.name]
    assert r["runs"]["closedUnarchived"] == [closed.name]
    assert r["runs"]["archivedSourcesPresent"] == [archived.name]
    assert r["runs"]["archivedSourcesAbsent"] == [gone.name]
    assert r["closedUnarchivedBytes"] == sum(p.stat().st_size for p in closed.glob("observations-*.ndjson"))
    assert r["archivedSourceBytes"] == sum(p.stat().st_size for p in archived.glob("observations-*.ndjson"))
    assert r["compressedArchiveBytes"] > 0
    assert r["aboveFloor"] and r["estimatedRemainingTypicalSessions"] == 10
    assert r["estimatedRemainingStressSessions"] == (10 * archive.TYPICAL_SESSION_BYTES + 5) // archive.STRESS_SESSION_BYTES
    below = archive.storage_report(str(tmp_path), free_bytes=archive.FLOOR_BYTES - 1)
    assert not below["aboveFloor"] and below["estimatedRemainingTypicalSessions"] == 0
    assert snapshot(tmp_path) == before


# --- no deletion, and nothing changed ---------------------------------------------

DELETING_COMMANDS = ["delete-source", "delete-archive", "delete", "remove", "rm", "prune", "purge", "truncate", "clean"]


@pytest.mark.parametrize("command", DELETING_COMMANDS)
def test_there_is_no_deleting_command(tmp_path, command, capsys):
    d = make_run(tmp_path)
    _, sha = archive.retain(str(d))
    before = snapshot(tmp_path)
    for args in ([], [str(d)], [str(d), sha], [str(d), sha, "x"], [str(d), sha, "x", "y"]):
        assert archive.main(["observation_archive.py", command, *args]) == 2, (command, args)
        assert "Commands:" in capsys.readouterr().out  # the usage text, not a result
    assert snapshot(tmp_path) == before


def test_the_tool_has_no_deletion_code():
    assert not hasattr(archive, "delete_source") and not hasattr(archive, "delete_archive")
    source = TOOL.read_text(encoding="utf-8")
    for forbidden in ["os.unlink", "shutil.rmtree", "os.rmdir", "os.removedirs", "os.truncate", ".truncate(", ".unlink(", "shutil.move"]:
        assert forbidden not in source, forbidden
    # Every removal is of the tool's own temporary of a compression that failed its check.
    removals = [line.split("#")[0].strip() for line in source.splitlines() if re.search(r"\bos\.remove\(", line)]
    assert removals == ["os.remove(part)", "os.remove(part)"]
    assert "delete-source" not in source and "delete_source" not in source


def test_every_command_leaves_existing_evidence_byte_identical(tmp_path, capsys):
    active = make_run(tmp_path, close_last=False, run_id="stockspotter-vps-1-20261002T201000123Z-0")
    closed = make_run(tmp_path, run_id="stockspotter-vps-1-20261003T001000004Z-0")
    archived = make_run(tmp_path, run_id="stockspotter-vps-1-20261004T001000002Z-0")
    archive.retain(str(archived))
    prereg = tmp_path / "prereg.json"
    prereg.write_text(json.dumps({"protocolVersion": "v1", "floors": {"a": 1}}))
    before = snapshot(tmp_path)
    run = lambda *a: archive.main(["observation_archive.py", *a])  # noqa: E731
    assert run("storage-report", str(tmp_path)) == 0
    assert run("verify", str(archived)) == 0
    assert run("compress", str(archived)) == 0   # everything already verified: nothing to do
    assert run("prereg-sha", str(prereg)) == 0
    assert run("retain", str(archived)) == 1     # already has a receipt
    assert run("retain", str(active)) == 1       # not closed
    assert run("retain", str(tmp_path / "missing")) == 1
    assert run("delete-source", str(archived), "0" * 64) == 2
    assert snapshot(tmp_path) == before, "read-only and refused commands change nothing at all"
    # The one command that writes only adds.
    assert run("retain", str(closed)) == 0
    after = snapshot(tmp_path)
    assert {n: after[n] for n in before} == before
    added = sorted(set(after) - set(before))
    assert added == sorted(f"{closed.name}/{n}" for n in
                           ["observations-0.ndjson.gz", "observations-1.ndjson.gz", archive.MANIFEST, archive.RETENTION_RECEIPT])
    # Off-box export copies out; the run it reads is unchanged.
    cert = tmp_path / "certificate.json"
    cert.write_text(json.dumps({"verdict": "PASS", "certificate": {"runId": closed.name, "preregistrationSha256": PREREG, "implementationSha": IMPL}}))
    dest = tmp_path / "dest"
    dest.mkdir()
    before_export = snapshot(closed)
    assert run("export", str(closed), str(dest), str(cert), "2026-10-03", PREREG, IMPL) == 0
    assert run("verify-export", str(dest / closed.name)) == 0
    assert snapshot(closed) == before_export
    capsys.readouterr()
