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
            self.assertEqual(manifest["schemaVersion"], 1)
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


if __name__ == "__main__":
    unittest.main()
