"""Tests for the research session exporter. No network, no strategy state."""
import gzip
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
EXPORTER = REPO / "python" / "export_session.py"


def load_exporter():
    """Imports the exporter by path, so these tests work regardless of cwd or
    PYTHONPATH -- `python -m unittest python.test_export_session` from the repo
    root does not put `python/` on sys.path."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("export_session", EXPORTER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_export(inputs, output, session_date=None):
    argv = [sys.executable, str(EXPORTER), *[str(i) for i in inputs], "--output", str(output)]
    if session_date:
        argv += ["--session-date", session_date]
    result = subprocess.run(argv, capture_output=True, text=True, check=True)
    return json.loads(result.stdout)


def write_capture(path, records):
    path.write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")


class ExportSessionTest(unittest.TestCase):
    def test_round_trips_records_and_reports_honest_counts(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            capture = tmp / "discovery-audit-1.jsonl"
            records = [{"recorded_at": "2026-09-10T14:00:00Z", "kind": "coverage", "n": i}
                       for i in range(50)]
            write_capture(capture, records)

            summary = run_export([capture], tmp / "out")
            self.assertEqual(summary["sessionDate"], "2026-09-10")
            self.assertEqual(summary["records"], 50)
            self.assertEqual(summary["errorRecords"], 0)

            exported = tmp / "out" / "session-2026-09-10" / "discovery-audit-1.ndjson.gz"
            with gzip.open(exported, "rt", encoding="utf-8") as source:
                back = [json.loads(line) for line in source]
            self.assertEqual(back, records, "every record must survive the round trip")

    def test_manifest_describes_exactly_what_was_written(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            capture = tmp / "live_pending_signals.jsonl"
            write_capture(capture, [{"symbol": "AAA", "timestamp": "2026-09-10T14:00:00Z"}])
            run_export([capture], tmp / "out")

            manifest = json.loads((tmp / "out" / "session-2026-09-10" / "manifest.json").read_text())
            self.assertEqual(manifest["schemaVersion"], 2)
            self.assertEqual(manifest["sessionDate"], "2026-09-10")
            entry = manifest["files"][0]
            self.assertEqual(entry["group"], "signals", "file must be classified by content type")
            self.assertEqual(entry["records"], 1)
            self.assertEqual(len(entry["sha256"]), 64)
            self.assertEqual(len(entry["sourceSha256"]), 64)
            self.assertIn("limitations", manifest)

            # The checksum in the manifest must match the file actually written.
            sums = (tmp / "out" / "session-2026-09-10" / "SHA256SUMS").read_text()
            self.assertIn(entry["sha256"], sums)

    def test_a_malformed_line_is_counted_and_still_exported(self):
        # Dropping it would create a silent gap -- exactly what the capture
        # format's own lost-record accounting exists to prevent.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            capture = tmp / "discovery-audit-1.jsonl"
            capture.write_text(
                '{"recorded_at":"2026-09-10T14:00:00Z","ok":1}\n'
                'this is not json\n'
                '{"recorded_at":"2026-09-10T14:00:01Z","ok":2}\n',
                encoding="utf-8")
            summary = run_export([capture], tmp / "out")
            self.assertEqual(summary["records"], 3)
            self.assertEqual(summary["errorRecords"], 1)

            exported = tmp / "out" / "session-2026-09-10" / "discovery-audit-1.ndjson.gz"
            with gzip.open(exported, "rt", encoding="utf-8") as source:
                self.assertEqual(len(source.readlines()), 3, "nothing is dropped")

    def test_session_date_comes_from_the_data_not_from_today(self):
        # An export run days later must not relabel the session.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            capture = tmp / "capture.jsonl"
            write_capture(capture, [{"recorded_at": "2019-03-04T14:00:00Z"}])
            summary = run_export([capture], tmp / "out")
            self.assertEqual(summary["sessionDate"], "2019-03-04")

    def test_no_credential_shaped_value_reaches_the_manifest(self):
        # Checks values, not prose: the manifest's own `limitations` text
        # legitimately contains the word "secret", and matching on English
        # words would flag that while missing an actual leaked token.
        import re
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            capture = tmp / "capture.jsonl"
            write_capture(capture, [{"recorded_at": "2026-09-10T14:00:00Z", "symbol": "AAA"}])
            run_export([capture], tmp / "out")
            manifest = json.loads(
                (tmp / "out" / "session-2026-09-10" / "manifest.json").read_text())

            credential = re.compile(
                r"(gh[ps]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}"
                r"|sk-[A-Za-z0-9]{20,}|PK[A-Z0-9]{16,}|AKIA[0-9A-Z]{16})")

            def scan(node, path="manifest"):
                if isinstance(node, dict):
                    for key, value in node.items():
                        self.assertNotIn(key.lower(), {"env", "environment", "credentials"},
                                         f"{path} must not carry an environment block")
                        scan(value, f"{path}.{key}")
                elif isinstance(node, list):
                    for i, value in enumerate(node):
                        scan(value, f"{path}[{i}]")
                elif isinstance(node, str) and path != "manifest.limitations":
                    self.assertIsNone(credential.search(node),
                                      f"credential-shaped value at {path}")

            scan(manifest)
            # Hashes are the only long opaque strings, and they are hex digests.
            for entry in manifest["files"]:
                self.assertRegex(entry["sha256"], r"^[0-9a-f]{64}$")

    def test_repetitive_capture_data_compresses_substantially(self):
        # Substantiates the export-time compression decision with a measurement
        # rather than an assumption.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            capture = tmp / "discovery-audit-1.jsonl"
            write_capture(capture, [
                {"recorded_at": "2026-09-10T14:00:00Z", "kind": "snapshot_batch",
                 "data": {"scan_id": "s1", "requested": [f"SYM{i}"],
                          "snapshots": {f"SYM{i}": {"latestTrade": {"p": 1.23, "t": "2026-09-10T14:00:00Z"}}}}}
                for i in range(2000)
            ])
            summary = run_export([capture], tmp / "out")
            self.assertGreater(summary["compressionRatio"], 5.0,
                               f"expected substantial compression, got {summary['compressionRatio']}x")


def try_export(inputs, output, expect=(), session_date=None):
    """Runs the exporter without asserting success, so failure paths are
    testable. Returns the completed process."""
    argv = [sys.executable, str(EXPORTER), *[str(i) for i in inputs], "--output", str(output)]
    for group in expect:
        argv += ["--expect", group]
    if session_date:
        argv += ["--session-date", session_date]
    return subprocess.run(argv, capture_output=True, text=True)


class DirectoryInputCompletenessTest(unittest.TestCase):
    """R3. A directory input used to glob `*.jsonl` only, so the episode
    capture -- which is written as `.ndjson` -- was silently excluded and the
    export still exited 0. These pin both suffixes and the loud-failure paths.
    """

    def _episodes(self, path, n=3):
        write_capture(path, [
            {"openedAt": "2026-09-10T14:00:00Z", "closedAt": "2026-09-10T14:05:00Z",
             "openingContext": {"capturedAt": "2026-09-10T14:00:00.5Z"}, "i": i}
            for i in range(n)
        ])

    def test_directory_with_only_jsonl_is_exported(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            write_capture(raw / "discovery-audit-1.jsonl",
                          [{"recorded_at": "2026-09-10T14:00:00Z"}])
            summary = run_export([raw], tmp / "out")
            self.assertEqual(summary["files"], 1)

    def test_directory_with_only_ndjson_is_exported(self):
        # The regression: this produced "no capture files found" before R3.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            self._episodes(raw / "episodes-2026-09-10.ndjson")
            summary = run_export([raw], tmp / "out", session_date="2026-09-10")
            self.assertEqual(summary["files"], 1)
            self.assertEqual(summary["records"], 3)

    def test_directory_with_both_suffixes_exports_both(self):
        # The exact Session 001 shape: episodes as .ndjson beside a .jsonl
        # ledger. Before R3 this exported the ledger alone and looked fine.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            self._episodes(raw / "episodes-2026-09-10.ndjson", n=4)
            write_capture(raw / "alpaca_paper_ledger.jsonl",
                          [{"account_id": "x", "trades": []}])
            summary = run_export([raw], tmp / "out", session_date="2026-09-10")
            self.assertEqual(summary["files"], 2)
            self.assertEqual(summary["records"], 5)
            manifest = json.loads(
                (tmp / "out" / "session-2026-09-10" / "manifest.json").read_text())
            self.assertEqual(sorted(manifest["groupsPresent"]), ["episodes", "trader"])

    def test_explicit_files_still_work_for_both_suffixes(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            a = tmp / "episodes-2026-09-10.ndjson"; self._episodes(a)
            b = tmp / "discovery-audit-1.jsonl"
            write_capture(b, [{"recorded_at": "2026-09-10T14:00:00Z"}])
            summary = run_export([a, b], tmp / "out", session_date="2026-09-10")
            self.assertEqual(summary["files"], 2)

    def test_empty_directory_fails_loudly(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            result = try_export([raw], tmp / "out")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no capture files found", result.stderr)

    def test_missing_expected_episodes_fails_non_zero(self):
        # A valid-looking partial export is worse than a loud failure.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            write_capture(raw / "alpaca_paper_ledger.jsonl", [{"account_id": "x"}])
            result = try_export([raw], tmp / "out", expect=["episodes"],
                                session_date="2026-09-10")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("episodes", result.stderr)

    def test_missing_expected_discovery_fails_non_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            self._episodes(raw / "episodes-2026-09-10.ndjson")
            result = try_export([raw], tmp / "out", expect=["discovery"],
                                session_date="2026-09-10")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("discovery", result.stderr)

    def test_present_expectation_succeeds(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            self._episodes(raw / "episodes-2026-09-10.ndjson")
            result = try_export([raw], tmp / "out", expect=["episodes"],
                                session_date="2026-09-10")
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_manifest_counts_and_checksums_match_independent_computation(self):
        import hashlib
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "raw"; raw.mkdir()
            source = raw / "episodes-2026-09-10.ndjson"
            self._episodes(source, n=7)
            run_export([raw], tmp / "out", session_date="2026-09-10")
            out = tmp / "out" / "session-2026-09-10"
            manifest = json.loads((out / "manifest.json").read_text())
            entry = manifest["files"][0]

            self.assertEqual(entry["records"], 7)
            self.assertEqual(
                entry["sourceSha256"],
                hashlib.sha256(source.read_bytes()).hexdigest(),
                "sourceSha256 must be the digest of the input bytes")
            self.assertEqual(
                entry["sha256"],
                hashlib.sha256((out / entry["name"]).read_bytes()).hexdigest(),
                "sha256 must be the digest of the artifact actually written")


class DiscoverySegmentClassificationTest(unittest.TestCase):
    """Found in 2026-09-11 deployment validation. Real discovery segments are
    named `<day>-<run>-<seq>.jsonl` and carry no group word in the filename, so
    filename-only classification called them `other` and `--expect discovery`
    failed on a complete capture."""

    def test_a_date_named_segment_in_discovery_audit_is_classified_discovery(self):
        ex = load_exporter()
        p = Path("/srv/data/discovery-audit/2026-09-11-1-1789087386026894-8.jsonl")
        self.assertEqual(ex.classify(p), "discovery")

    def test_episodes_in_a_research_directory_still_classify_by_filename(self):
        ex = load_exporter()
        p = Path("/srv/data/research/episodes-2026-09-11.ndjson")
        self.assertEqual(ex.classify(p), "episodes")

    def test_expect_discovery_passes_on_date_named_segments(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            raw = tmp / "discovery-audit"; raw.mkdir()
            write_capture(raw / "2026-09-11-1-999-1.jsonl",
                          [{"recorded_at": "2026-09-11T14:00:00Z", "kind": "ignition"}])
            result = try_export([raw], tmp / "out", expect=["discovery"],
                                session_date="2026-09-11")
            self.assertEqual(result.returncode, 0, result.stderr)


class ManifestTimeSemanticsTest(unittest.TestCase):
    """R4. `captureStartedAt`/`captureEndedAt` were filesystem mtimes, so an
    export from copied files described the copy. These pin four clocks apart.
    """

    def test_four_clocks_stay_distinct_and_correct(self):
        import os
        import datetime as dt
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            source = tmp / "episodes-2026-09-10.ndjson"
            # Event time and capture time deliberately differ from each other,
            # and the mtime is forced to a third, much later instant -- the
            # shape a transferred file actually has.
            write_capture(source, [
                {"openedAt": "2026-09-10T13:30:00Z", "closedAt": "2026-09-10T19:00:00Z",
                 "openingContext": {"capturedAt": "2026-09-10T13:30:00.250Z"}},
                {"openedAt": "2026-09-10T14:00:00Z", "closedAt": "2026-09-10T20:00:00Z",
                 "openingContext": {"capturedAt": "2026-09-10T20:00:00.750Z"}},
            ])
            forced = dt.datetime(2026, 9, 11, 3, 0, 0, tzinfo=dt.timezone.utc).timestamp()
            os.utime(source, (forced, forced))

            run_export([source], tmp / "out", session_date="2026-09-10")
            manifest = json.loads(
                (tmp / "out" / "session-2026-09-10" / "manifest.json").read_text())

            self.assertEqual(manifest["schemaVersion"], 2)
            # The old, dishonest fields are gone.
            self.assertNotIn("captureStartedAt", manifest)
            self.assertNotIn("captureEndedAt", manifest)

            event = manifest["eventTimeRange"]
            capture = manifest["captureTimeRange"]
            mtime = manifest["sourceFileModifiedRange"]

            self.assertEqual(event["start"], "2026-09-10T13:30:00Z")
            self.assertEqual(event["end"], "2026-09-10T20:00:00Z")
            self.assertEqual(capture["start"], "2026-09-10T13:30:00.250Z")
            self.assertEqual(capture["end"], "2026-09-10T20:00:00.750Z")
            self.assertTrue(mtime["start"].startswith("2026-09-11T03:00:00"),
                            f"mtime must be the filesystem clock, got {mtime['start']}")

            # All four must be genuinely different values, which is the whole
            # point: conflating any two of them is how F10 happened.
            self.assertNotEqual(event["start"], capture["start"])
            self.assertNotEqual(event["end"], mtime["end"])
            self.assertNotEqual(manifest["exportedAt"], mtime["end"])
            self.assertIn("timestampSemantics", manifest)
            self.assertIn("filesystem",
                          manifest["timestampSemantics"]["sourceFileModifiedRange"].lower())

    def test_dataset_without_capture_timestamps_yields_null_not_a_substitute(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            ledger = tmp / "alpaca_paper_ledger.jsonl"
            write_capture(ledger, [{"account_id": "x", "trades": []}])
            run_export([ledger], tmp / "out", session_date="2026-09-10")
            manifest = json.loads(
                (tmp / "out" / "session-2026-09-10" / "manifest.json").read_text())
            self.assertIsNone(manifest["captureTimeRange"],
                              "a missing semantic timestamp must be null, never back-filled")
            self.assertIsNone(manifest["eventTimeRange"])
            self.assertIsNotNone(manifest["sourceFileModifiedRange"]["start"])


if __name__ == "__main__":
    unittest.main()
