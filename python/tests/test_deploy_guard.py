from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlparse


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "stockspotter_deploy_guard", ROOT / "ops" / "vps" / "deploy_guard.py"
)
assert SPEC and SPEC.loader
guard = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = guard
SPEC.loader.exec_module(guard)


SHA = "a" * 40
BRANCH = "release/test-guard"
RUN_ID = 301


def workflow_run(*, run_id: int = RUN_ID, sha: str = SHA, branch: str = BRANCH,
                 workflow_id: int = guard.VALIDATE_WORKFLOW_ID, event: str = "push",
                 run_number: int = 7, attempt: int = 1, status: str = "completed",
                 conclusion: str = "failure") -> dict:
    return {
        "id": run_id,
        "workflow_id": workflow_id,
        "head_sha": sha,
        "head_branch": branch,
        "event": event,
        "run_number": run_number,
        "run_attempt": attempt,
        "status": status,
        "conclusion": conclusion,
    }


def job(name: str, *, run_id: int = RUN_ID, sha: str = SHA,
        status: str = "completed", conclusion: str = "success", job_id: int | None = None) -> dict:
    if job_id is None:
        job_id = 100 if name == guard.REQUIRED_JOBS[0] else 101
    return {"id": job_id, "run_id": run_id, "head_sha": sha, "name": name,
            "status": status, "conclusion": conclusion}


def passing_server_jobs() -> list[dict]:
    return [job(name, job_id=100 + index) for index, name in enumerate(guard.REQUIRED_JOBS)]


class VerifyServerJobsTests(unittest.TestCase):
    def test_exact_trusted_push_jobs_pass_even_when_mobile_and_workflow_fail(self) -> None:
        # The mobile advisory job is expected to fail. The overall workflow
        # conclusion is therefore failure while the two server jobs pass.
        run = workflow_run(conclusion="failure")
        jobs = passing_server_jobs() + [job("Mobile (types and dependency advisories)", conclusion="failure", job_id=102)]
        guard.verify_server_jobs([run], {RUN_ID: jobs}, SHA, BRANCH)

    def test_untrusted_workflow_id_refuses(self) -> None:
        with self.assertRaisesRegex(guard.GateError, "untrusted workflow"):
            guard.verify_server_jobs([workflow_run(workflow_id=999)], {RUN_ID: passing_server_jobs()}, SHA, BRANCH)

    def test_wrong_sha_branch_or_event_refuses(self) -> None:
        cases = [
            (workflow_run(sha="b" * 40), "different branch, SHA, or event"),
            (workflow_run(branch="master"), "different branch, SHA, or event"),
            (workflow_run(event="pull_request"), "different branch, SHA, or event"),
        ]
        for run, message in cases:
            with self.subTest(run=run):
                with self.assertRaisesRegex(guard.GateError, message):
                    guard.verify_server_jobs([run], {RUN_ID: passing_server_jobs()}, SHA, BRANCH)

    def test_non_deploy_branch_and_invalid_sha_refuse(self) -> None:
        with self.assertRaisesRegex(guard.GateError, "full lowercase"):
            guard.verify_server_jobs([workflow_run()], {RUN_ID: passing_server_jobs()}, "bad", BRANCH)
        with self.assertRaisesRegex(guard.GateError, "unsupported deploy branch"):
            guard.verify_server_jobs([workflow_run()], {RUN_ID: passing_server_jobs()}, SHA, "topic/test")

    def test_latest_run_attempt_controls(self) -> None:
        older = workflow_run(run_number=6)
        latest = workflow_run(run_id=302, run_number=7, attempt=2)
        jobs = passing_server_jobs()
        failed_latest_jobs = [job(name, run_id=302, conclusion="failure" if name == guard.REQUIRED_JOBS[0] else "success")
                              for name in guard.REQUIRED_JOBS]
        with self.assertRaisesRegex(guard.GateError, "Tests, lint and build"):
            guard.verify_server_jobs([older, latest], {RUN_ID: jobs, 302: failed_latest_jobs}, SHA, BRANCH)

    def test_in_progress_latest_run_refuses(self) -> None:
        with self.assertRaisesRegex(guard.GateError, "not completed"):
            guard.verify_server_jobs([workflow_run(status="in_progress")], {RUN_ID: passing_server_jobs()}, SHA, BRANCH)

    def test_failed_or_incomplete_required_job_refuses(self) -> None:
        for status, conclusion in (("completed", "failure"), ("in_progress", ""), ("completed", "skipped")):
            jobs = [job(name, status=status, conclusion=conclusion) if name == guard.REQUIRED_JOBS[1] else job(name)
                    for name in guard.REQUIRED_JOBS]
            with self.subTest(status=status, conclusion=conclusion):
                with self.assertRaisesRegex(guard.GateError, "Dependency advisories"):
                    guard.verify_server_jobs([workflow_run()], {RUN_ID: jobs}, SHA, BRANCH)

    def test_missing_ambiguous_stale_job_and_missing_inventory_refuse(self) -> None:
        with self.assertRaisesRegex(guard.GateError, "missing or ambiguous"):
            guard.verify_server_jobs([workflow_run()], {RUN_ID: passing_server_jobs()[:1]}, SHA, BRANCH)
        with self.assertRaisesRegex(guard.GateError, "missing or ambiguous"):
            guard.verify_server_jobs([workflow_run()], {RUN_ID: passing_server_jobs() + [job(guard.REQUIRED_JOBS[0])]}, SHA, BRANCH)
        stale = [job(name, sha="b" * 40) for name in guard.REQUIRED_JOBS]
        with self.assertRaisesRegex(guard.GateError, "different run or SHA"):
            guard.verify_server_jobs([workflow_run()], {RUN_ID: stale}, SHA, BRANCH)
        with self.assertRaisesRegex(guard.GateError, "inventory is missing or malformed"):
            guard.verify_server_jobs([workflow_run()], {}, SHA, BRANCH)

    def test_invalid_run_inventory_refuses(self) -> None:
        for runs in ([], [None], [workflow_run(run_id=True)], [workflow_run(run_number="7")],
                     [workflow_run(), workflow_run(run_id=RUN_ID, run_number=8)]):
            with self.subTest(runs=runs):
                with self.assertRaises(guard.GateError):
                    guard.verify_server_jobs(runs, {RUN_ID: passing_server_jobs()}, SHA, BRANCH)


class FetchApiTests(unittest.TestCase):
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

    def test_fetch_workflow_runs_scopes_to_trusted_workflow_sha_branch_and_push(self) -> None:
        body = {"total_count": 1, "workflow_runs": [workflow_run()]}
        with patch.object(guard, "urlopen", return_value=self.Response(body)) as open_url:
            runs = guard.fetch_workflow_runs(SHA, BRANCH)
        query = parse_qs(urlparse(open_url.call_args.args[0].full_url).query)
        self.assertEqual(query["head_sha"], [SHA])
        self.assertEqual(query["branch"], [BRANCH])
        self.assertEqual(query["event"], ["push"])
        self.assertIn(str(guard.VALIDATE_WORKFLOW_ID), open_url.call_args.args[0].full_url)
        self.assertEqual(runs, [body["workflow_runs"][0]])

    def test_fetch_workflow_runs_refuses_empty_or_incomplete_inventory(self) -> None:
        for body, message in (({"total_count": 0, "workflow_runs": []}, "has no push run"),
                              ({"total_count": 1, "workflow_runs": []}, "inventory is incomplete")):
            with self.subTest(body=body), patch.object(guard, "urlopen", return_value=self.Response(body)):
                with self.assertRaisesRegex(guard.GateError, message):
                    guard.fetch_workflow_runs(SHA, BRANCH)

    def test_fetch_jobs_paginates_and_checks_inventory_size(self) -> None:
        first = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[0])]}
        second = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[1])]}
        responses = [self.Response(first, '<https://api.github.com/page/2>; rel="next"'), self.Response(second)]
        with patch.object(guard, "urlopen", side_effect=responses) as open_url:
            jobs = guard.fetch_jobs(RUN_ID)
        self.assertEqual(open_url.call_count, 2)
        self.assertEqual(len(jobs), 2)

    def test_fetch_jobs_refuses_incomplete_inventory(self) -> None:
        body = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[0])]}
        with patch.object(guard, "urlopen", return_value=self.Response(body)):
            with self.assertRaisesRegex(guard.GateError, "job inventory is incomplete"):
                guard.fetch_jobs(RUN_ID)

    def test_fetch_jobs_refuses_duplicate_ids(self) -> None:
        duplicate = job(guard.REQUIRED_JOBS[0])
        body = {"total_count": 2, "jobs": [duplicate, duplicate]}
        with patch.object(guard, "urlopen", return_value=self.Response(body)):
            with self.assertRaisesRegex(guard.GateError, "duplicate job ids"):
                guard.fetch_jobs(RUN_ID)


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
