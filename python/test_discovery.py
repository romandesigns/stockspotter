import unittest
from collections import Counter
from datetime import datetime, timedelta, timezone
import tempfile
from pathlib import Path
from analyze_discovery import analyze, read_files


START = datetime(2026, 9, 8, 14, tzinfo=timezone.utc)


def record(kind, seconds, data, lost=0):
    return {"schema": 1, "kind": kind, "recorded_at": (START + timedelta(seconds=seconds)).isoformat(),
            "lost_records": lost, "data": data}


def market(stale=False, gap=False):
    out = []
    for second in range(0, 1801, 15):
        if gap and 600 <= second <= 690:
            continue
        at = START + timedelta(seconds=second - (120 if stale else 0))
        price = 1 if second < 600 else 1.11
        scan_id = str(second)
        out.append(record("scan_started", second, {"scan_id": scan_id, "feed": "test", "universe": ["MISSED", "SEEN"]}))
        out.append(record("snapshot_batch", second, {"scan_id": scan_id, "requested": ["MISSED", "SEEN"],
            "snapshots": {s: {"latestTrade": {"p": price, "t": at.isoformat()}} for s in ["MISSED", "SEEN"]}}))
        out.append(record("snapshot_complete", second, {"scan_id": scan_id}))
    return out


class DiscoveryTests(unittest.TestCase):
    def test_independent_denominator_includes_unwatched_runner_and_uses_receipt_time(self):
        rows = market()
        rows += [record("coverage", 290, {"full_ignition": ["SEEN"], "universe_ignition": [], "receipts": {
            "SEEN": {"ignition_monitored": True, "received_at": (START + timedelta(seconds=280)).isoformat(),
                     "market_at": (START + timedelta(seconds=280)).isoformat()}}}),
            record("ignition", 450, {"symbol": "SEEN", "stage": "confirmed", "market_at": (START + timedelta(seconds=440)).isoformat()})]
        result = analyze(rows)
        self.assertEqual(result["candidate_count"], 2)
        by_symbol = {r["symbol"]: r for r in result["candidates"]}
        self.assertIsNone(by_symbol["MISSED"]["monitored_receipt_before_base"])
        self.assertFalse(by_symbol["MISSED"]["configured_monitor_before_base"])
        self.assertTrue(by_symbol["SEEN"]["monitored_receipt_before_base"])
        self.assertEqual(by_symbol["SEEN"]["alert_lead_seconds"], 150)
        self.assertIsNone(result["whole_market_recall"])

    def test_old_trade_snapshots_do_not_fabricate_a_flat_base(self):
        result = analyze(market(stale=True))
        self.assertEqual(result["status"], "insufficient_evidence")
        self.assertGreater(result["quality"]["stale_trade_snapshots"], 0)

    def test_missing_forward_interval_is_censored_even_if_later_price_is_up(self):
        result = analyze(market(gap=True))
        self.assertEqual(result["candidate_count"], 0)
        self.assertGreater(result["bases"]["censored_base_anchors"], 0)

    def test_late_alert_and_future_receipt_cannot_retroactively_cover_a_runner(self):
        rows = market() + [record("coverage", 620, {"full_ignition": ["SEEN"], "universe_ignition": [], "receipts": {}}),
            record("ignition", 620, {"symbol": "SEEN", "stage": "confirmed", "market_at": (START + timedelta(seconds=590)).isoformat()})]
        result = analyze(rows)
        self.assertEqual(result["candidates_with_confirmed_alert_before_crossing"], 0)
        self.assertIsNone(result["candidates"][1]["configured_monitor_before_base"])

    def test_missing_universe_batches_and_recorded_loss_are_visible(self):
        result = analyze([record("scan_started", 0, {"scan_id": "one", "feed": "test", "universe": ["MISSING"]}, lost=3)])
        self.assertEqual(result["quality"]["incomplete_snapshot_scans"], 1)
        self.assertEqual(result["quality"]["unrequested_universe_symbols"], 1)
        self.assertEqual(result["quality"]["max_reported_lost_records"], 3)
        self.assertEqual(result["status"], "insufficient_evidence")

    def test_empty_capture_and_unknown_schema_do_not_report_success(self):
        self.assertEqual(analyze([])["status"], "insufficient_evidence")
        with self.assertRaises(ValueError):
            analyze([{"schema": 2}])

    def test_base_before_open_can_cross_after_open(self):
        rows = market()
        # Move the first base endpoint to 09:25 ET, crossing to 09:30 ET.
        for row in rows:
            row["recorded_at"] = (datetime.fromisoformat(row["recorded_at"]) - timedelta(minutes=40)).isoformat()
            for raw in row["data"].get("snapshots", {}).values():
                raw["latestTrade"]["t"] = (datetime.fromisoformat(raw["latestTrade"]["t"]) - timedelta(minutes=40)).isoformat()
        result = analyze(rows)
        self.assertEqual(result["candidate_count"], 2)
        self.assertEqual(result["candidates"][0]["session"], "premarket")

    def test_partial_line_is_reported_and_complete_corruption_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "capture.jsonl"
            path.write_bytes(b'{"schema":')
            sources, quality = [], Counter()
            self.assertEqual(list(read_files([path], sources, quality)), [])
            self.assertEqual(quality["partial_lines"], 1)
            self.assertEqual(len(sources[0]["sha256"]), 64)
            path.write_bytes(b'{"schema":\n')
            with self.assertRaises(ValueError):
                list(read_files([path], [], Counter()))


if __name__ == "__main__":
    unittest.main()
