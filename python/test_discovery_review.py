import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops"))
from discovery_review import review


class Response:
    def __init__(self, mode):
        self.content = json.dumps({"executionMode": mode, "trades": 0}).encode()
    def __enter__(self):
        return self
    def __exit__(self, *args):
        pass
    def read(self):
        return self.content


class DailyReviewTests(unittest.TestCase):
    def test_empty_capture_stays_unknown_with_confirmed_paper_status(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {"STOCKSPOTTER_API_TOKEN": "test"}), patch("urllib.request.urlopen", return_value=Response("alpaca_paper")):
            root = Path(directory)
            review("2026-09-08", root)
            output = json.loads((root / "discovery-reports/latest.json").read_text())
            self.assertEqual(output["status"], "insufficient_evidence")
            self.assertIsNone(output["whole_market_recall"])
            self.assertEqual(output["paper_status"]["executionMode"], "alpaca_paper")

    def test_simulation_status_cannot_be_reported_as_broker_paper_results(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {"STOCKSPOTTER_API_TOKEN": "test"}), patch("urllib.request.urlopen", return_value=Response("journal")):
            root = Path(directory)
            review("2026-09-08", root)
            output = json.loads((root / "discovery-reports/latest.json").read_text())
            self.assertIsNone(output["paper_status"])
            self.assertIn("not broker paper mode", output["paper_status_error"])


if __name__ == "__main__":
    unittest.main()
