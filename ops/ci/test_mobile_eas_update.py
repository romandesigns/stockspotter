"""Release boundary checks for the preview OTA update workflow."""
import json
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def job(text, name):
    match = re.search(r"^  " + re.escape(name) + r":\n(.*?)(?=^  [\w-]+:\n|\Z)", text, re.M | re.S)
    if match is None:
        raise AssertionError(f"Missing job {name}")
    return match.group(1)


class MobileEasUpdateTests(unittest.TestCase):
    def setUp(self):
        self.text = (ROOT / ".github/workflows/mobile-eas-update.yml").read_text()
        self.update = job(self.text, "update")

    def test_publishes_only_from_release_branches(self):
        triggers = self.text.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertIn('push:\n    branches:\n      - "release/**"', triggers)
        for forbidden in ("workflow_run:", "pull_request", "master", "workflow_dispatch"):
            self.assertNotIn(forbidden, triggers)
        self.assertIn("if: startsWith(github.ref, 'refs/heads/release/')", self.update)
        # Only the newest pending push matters for an update; a superseded
        # pending run being replaced is intended here.
        self.assertIn("cancel-in-progress: false", self.text)

    def test_full_mobile_gate_runs_first(self):
        validate = job(self.text, "validate")
        self.assertIn("uses: ./.github/workflows/validate.yml", validate)
        self.assertIn("force_mobile: true", validate)
        self.assertIn("needs: validate", self.update)
        self.assertNotIn("always()", self.update)
        self.assertNotIn("continue-on-error", self.update)

    def test_preview_android_update_only(self):
        self.assertIn("update --channel preview --platform android", self.update)
        for forbidden in ("--channel production", "--platform ios", "--platform all", "--branch", " build --", "submit", "--auto"):
            self.assertNotIn(forbidden, self.update)
        self.assertIn("eas-cli@24.12.1", self.update)
        self.assertIn("ref: ${{ github.sha }}", self.update)
        self.assertIn('--message "release $SOURCE_SHA"', self.update)

    def test_updates_are_bound_to_native_fingerprint(self):
        app = json.loads((ROOT / "apps/mobile/app.json").read_text(encoding="utf-8"))["expo"]
        # appVersion with a fixed "1.0.0" would send JS to every APK whatever
        # native code it carries.
        self.assertEqual(app["runtimeVersion"], {"policy": "fingerprint"})
        self.assertIn("Require the fingerprint runtime policy", self.update)


if __name__ == "__main__":
    unittest.main()
