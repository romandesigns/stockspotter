from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "stockspotter_deploy_guard", ROOT / "ops" / "vps" / "deploy_guard.py"
)
assert SPEC and SPEC.loader
guard = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = guard
SPEC.loader.exec_module(guard)


SHA = "a" * 40


def check(run_id: int, name: str, *, status: str = "completed", conclusion: str = "success",
          app_id: int = guard.GITHUB_ACTIONS_APP_ID, head_sha: str = SHA) -> dict:
    return {
        "id": run_id,
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "head_sha": head_sha,
        "app": {"id": app_id},
    }


def passing_checks() -> list[dict]:
    return [check(1, guard.REQUIRED_CHECKS[0]), check(2, guard.REQUIRED_CHECKS[1])]


class VerifyCheckRunsTests(unittest.TestCase):
    def test_exact_server_checks_pass_even_when_mobile_is_blocked(self) -> None:
        runs = passing_checks() + [
            check(3, "Mobile (types and dependency advisories)", conclusion="failure")
        ]
        guard.verify_check_runs(runs, SHA)

    def test_missing_required_check_refuses(self) -> None:
        with self.assertRaisesRegex(guard.GateError, "Dependency advisories"):
            guard.verify_check_runs(passing_checks()[:1], SHA)

    def test_wrong_sha_refuses(self) -> None:
        with self.assertRaisesRegex(guard.GateError, "different commit"):
            guard.verify_check_runs([check(1, guard.REQUIRED_CHECKS[0], head_sha="b" * 40)], SHA)

    def test_same_name_from_untrusted_app_does_not_satisfy_gate(self) -> None:
        runs = [check(1, guard.REQUIRED_CHECKS[0], app_id=7), check(2, guard.REQUIRED_CHECKS[1])]
        with self.assertRaisesRegex(guard.GateError, "Tests, lint and build"):
            guard.verify_check_runs(runs, SHA)

    def test_latest_attempt_controls_even_if_older_attempt_passed(self) -> None:
        runs = passing_checks() + [check(3, guard.REQUIRED_CHECKS[0], conclusion="failure")]
        with self.assertRaisesRegex(guard.GateError, "Tests, lint and build"):
            guard.verify_check_runs(runs, SHA)

    def test_latest_in_progress_attempt_refuses(self) -> None:
        runs = passing_checks() + [check(3, guard.REQUIRED_CHECKS[1], status="in_progress", conclusion="")]
        with self.assertRaisesRegex(guard.GateError, "Dependency advisories"):
            guard.verify_check_runs(runs, SHA)

    def test_non_success_conclusions_refuse(self) -> None:
        for conclusion in ("failure", "cancelled", "neutral", "skipped"):
            with self.subTest(conclusion=conclusion):
                runs = passing_checks() + [check(3, guard.REQUIRED_CHECKS[0], conclusion=conclusion)]
                with self.assertRaisesRegex(guard.GateError, "Tests, lint and build"):
                    guard.verify_check_runs(runs, SHA)

    def test_invalid_commit_and_malformed_inventory_refuse(self) -> None:
        with self.assertRaises(guard.GateError):
            guard.verify_check_runs(passing_checks(), "not-a-full-sha")
        with self.assertRaises(guard.GateError):
            guard.verify_check_runs([None], SHA)  # type: ignore[list-item]


class FetchCheckRunsTests(unittest.TestCase):
    class Response:
        def __init__(self, body: dict, link: str | None = None):
            self._body = json.dumps(body).encode("utf-8")
            self.headers = {"Link": link} if link else {}

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def read(self):
            return self._body

    def test_fetches_all_pages_and_validates_inventory_size(self) -> None:
        first = {"total_count": 2, "check_runs": [check(1, guard.REQUIRED_CHECKS[0])]}
        second = {"total_count": 2, "check_runs": [check(2, guard.REQUIRED_CHECKS[1])]}
        responses = [
            self.Response(first, '<https://api.github.com/page/2>; rel="next"'),
            self.Response(second),
        ]
        with patch.object(guard, "urlopen", side_effect=responses) as open_url:
            runs = guard.fetch_check_runs(SHA)
        guard.verify_check_runs(runs, SHA)
        self.assertEqual(open_url.call_count, 2)

    def test_incomplete_page_fails_closed(self) -> None:
        with patch.object(
            guard,
            "urlopen",
            return_value=self.Response({"total_count": 2, "check_runs": [check(1, guard.REQUIRED_CHECKS[0])]}),
        ):
            with self.assertRaisesRegex(guard.GateError, "inventory is incomplete"):
                guard.fetch_check_runs(SHA)


class DeployScriptContractTests(unittest.TestCase):
    def test_exact_remote_tip_and_check_gate_precede_image_build(self) -> None:
        source = (ROOT / "ops" / "vps" / "deploy.sh").read_text(encoding="utf-8")
        self.assertIn('if [ "$AFTER" != "$REMOTE_SHA" ]; then', source)
        gate = source.index('python3 ops/vps/deploy_guard.py --commit "$AFTER" --branch "$BRANCH"')
        recheck = source.index('Deployment refused: checkout changed after exact-commit validation')
        build = source.index('docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml build')
        self.assertLess(gate, recheck)
        self.assertLess(recheck, build)
        self.assertIn('git merge --ff-only FETCH_HEAD', source)
        self.assertIn('git status --porcelain --untracked-files=all', source)


if __name__ == "__main__":
    unittest.main()
