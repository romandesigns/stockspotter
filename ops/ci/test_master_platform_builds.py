"""Release boundary checks for continuous builds, independent of publication."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def job(text, name):
    match = re.search(r"^  " + re.escape(name) + r":\n(.*?)(?=^  [\w-]+:\n|\Z)", text, re.M | re.S)
    if match is None:
        raise AssertionError(f"Missing job {name}")
    return match.group(1)


class MasterPlatformBuildTests(unittest.TestCase):
    def setUp(self):
        self.text = (ROOT / ".github/workflows/master-platform-builds.yml").read_text()
        self.desktop = job(self.text, "desktop-build")
        self.android = job(self.text, "android-preview")

    def test_every_master_push_has_a_build_attempt(self):
        triggers = self.text.split("\njobs:", 1)[0]
        self.assertIn("push:\n    branches: [master]", triggers)
        self.assertNotIn("paths:", triggers)
        self.assertNotIn("workflow_run:", triggers)
        # A shared concurrency group can discard pending SHAs even when
        # cancel-in-progress is false. Each push must remain independent.
        self.assertNotIn("concurrency:", self.text)

    def test_platform_gates_are_independent(self):
        self.assertIn("needs: [server-validation, desktop-audit]", self.desktop)
        self.assertNotIn("mobile-validation", self.desktop)
        self.assertIn("needs: mobile-validation", self.android)
        for body in (self.desktop, self.android):
            self.assertIn("if: github.ref == 'refs/heads/master'", body)
            self.assertNotIn("always()", body.split("    steps:", 1)[0])
            self.assertNotIn("continue-on-error", body)

    def test_checkout_and_artifacts_are_bound_to_event_sha(self):
        for body in (self.desktop, self.android):
            self.assertIn("ref: ${{ github.sha }}", body)
            self.assertIn("${{ github.sha }}-${{ github.run_id }}", body)
            self.assertIn("if-no-files-found: error", body)

    def test_continuous_desktop_never_overwrites_published_assets(self):
        self.assertIn("tauri-apps/tauri-action@", self.desktop)
        for forbidden in ("tagName:", "releaseName:", "releaseId:", "includeUpdaterJson:", "TAURI_SIGNING", "secrets."):
            self.assertNotIn(forbidden, self.desktop)
        self.assertIn('"createUpdaterArtifacts":false', self.desktop)
        self.assertIn('args: --config "${{ steps.continuous-config.outputs.path }}"', self.desktop)
        self.assertIn("Unexpected updater signatures", self.desktop)
        self.assertIn("No Windows installers were produced", self.desktop)
        self.assertIn("Get-FileHash -LiteralPath", self.desktop)

    def test_desktop_audits_its_own_lockfile(self):
        audit = job(self.text, "desktop-audit")
        self.assertIn("cargo audit --file apps/client/src-tauri/Cargo.lock", audit)
        self.assertNotIn("--ignore", audit)

    def test_android_is_preview_only_and_waits_for_real_result(self):
        self.assertIn("--platform android --profile preview --non-interactive", self.android)
        for forbidden in ("--platform all", "--platform ios", "--profile production", "--auto-submit", "--no-wait"):
            self.assertNotIn(forbidden, self.android)
        self.assertIn('--message "$SOURCE_SHA" --json > eas-build-result.json', self.android)
        self.assertIn("eas-cli@24.12.1", self.android)
        self.assertIn("verify_eas_preview.py", self.android)
        self.assertIn("working-directory: apps/mobile", self.android)

    def test_mobile_gate_is_forced_on_non_mobile_pushes(self):
        caller = job(self.text, "mobile-validation")
        self.assertIn("uses: ./.github/workflows/validate.yml", caller)
        self.assertIn("force_mobile: true", caller)
        validate = (ROOT / ".github/workflows/validate.yml").read_text()
        self.assertIn("force_mobile:\n", validate)
        self.assertIn("FORCE_MOBILE: ${{ inputs.force_mobile || false }}", validate)
        self.assertIn('[ "$FORCE_MOBILE" = "true" ]', validate)
        self.assertIn("needs.mobile_scope.outputs.changed == 'true'", validate)
        self.assertEqual(set(re.findall(r"^  ([\w-]+):$", validate.split("\njobs:\n", 1)[1], re.M)), {"mobile_scope", "checks", "audit", "mobile"})


if __name__ == "__main__":
    unittest.main()
