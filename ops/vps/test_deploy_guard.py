from __future__ import annotations

import importlib.util
import contextlib
import http.client
import io
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
    return ([job(name, job_id=100 + index) for index, name in enumerate(guard.REQUIRED_JOBS)]
            + [job(guard.MOBILE_SCOPE_JOB, job_id=102),
               job(guard.MOBILE_JOB, conclusion="failure", job_id=103)])


def attempt_key(run: dict) -> tuple[int, int]:
    return (run["id"], run["run_attempt"])


class VerifyServerJobsTests(unittest.TestCase):
    def test_exact_trusted_push_jobs_pass_even_when_mobile_and_workflow_fail(self) -> None:
        # The mobile advisory job is expected to fail. The overall workflow
        # conclusion is therefore failure while the two server jobs pass.
        run = workflow_run(conclusion="failure")
        guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()}, SHA, BRANCH)

    def test_server_deploy_passes_when_mobile_job_is_correctly_skipped(self) -> None:
        run = workflow_run(conclusion="success")
        jobs = passing_server_jobs()
        jobs[-1] = job(guard.MOBILE_JOB, conclusion="skipped", job_id=103)
        guard.verify_server_jobs([run], {attempt_key(run): jobs}, SHA, BRANCH)

    def test_mobile_scope_detector_must_succeed(self) -> None:
        run = workflow_run(conclusion="success")
        jobs = passing_server_jobs()
        jobs[-2] = job(guard.MOBILE_SCOPE_JOB, conclusion="failure", job_id=102)
        with self.assertRaisesRegex(guard.GateError, "Detect mobile inputs"):
            guard.verify_server_jobs([run], {attempt_key(run): jobs}, SHA, BRANCH)

    def test_untrusted_workflow_id_refuses(self) -> None:
        run = workflow_run(workflow_id=999)
        with self.assertRaisesRegex(guard.GateError, "untrusted workflow"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()}, SHA, BRANCH)

    def test_wrong_sha_branch_or_event_refuses(self) -> None:
        cases = [
            (workflow_run(sha="b" * 40), "different branch, SHA, or event"),
            (workflow_run(branch="master"), "different branch, SHA, or event"),
            (workflow_run(event="pull_request"), "different branch, SHA, or event"),
        ]
        for run, message in cases:
            with self.subTest(run=run):
                with self.assertRaisesRegex(guard.GateError, message):
                    guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()}, SHA, BRANCH)

    def test_non_deploy_branch_and_invalid_sha_refuse(self) -> None:
        run = workflow_run()
        with self.assertRaisesRegex(guard.GateError, "full lowercase"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()}, "bad", BRANCH)
        with self.assertRaisesRegex(guard.GateError, "unsupported deploy branch"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()}, SHA, "topic/test")

    def test_latest_run_attempt_controls(self) -> None:
        older = workflow_run(run_number=6)
        latest = workflow_run(run_id=302, run_number=7, attempt=2)
        jobs = passing_server_jobs()
        failed_latest_jobs = [job(name, run_id=302,
                                  conclusion="failure" if name == guard.REQUIRED_JOBS[0] else "success",
                                  job_id=100 + index)
                              for index, name in enumerate(guard.REQUIRED_JOBS)]
        failed_latest_jobs.extend([
            job(guard.MOBILE_SCOPE_JOB, run_id=302, job_id=102),
            job(guard.MOBILE_JOB, run_id=302, conclusion="failure", job_id=103),
        ])
        with self.assertRaisesRegex(guard.GateError, "Tests, lint and build"):
            guard.verify_server_jobs([older, latest], {attempt_key(older): jobs, attempt_key(latest): failed_latest_jobs}, SHA, BRANCH)

    def test_in_progress_latest_run_refuses(self) -> None:
        run = workflow_run(status="in_progress")
        with self.assertRaisesRegex(guard.GateError, "not completed"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()}, SHA, BRANCH)

    def test_cancelled_latest_run_refuses_even_with_green_server_jobs(self) -> None:
        run = workflow_run(conclusion="cancelled")
        with self.assertRaisesRegex(guard.GateError, "unacceptable conclusion"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()}, SHA, BRANCH)

    def test_jobs_from_different_attempt_are_not_used(self) -> None:
        run = workflow_run(attempt=2)
        with self.assertRaisesRegex(guard.GateError, "inventory is missing or malformed"):
            guard.verify_server_jobs([run], {(RUN_ID, 1): passing_server_jobs()}, SHA, BRANCH)

    def test_unclassified_workflow_job_refuses(self) -> None:
        run = workflow_run()
        with self.assertRaisesRegex(guard.GateError, "unclassified job"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs() + [job("New job", job_id=103)]}, SHA, BRANCH)

    def test_failed_or_incomplete_required_job_refuses(self) -> None:
        for status, conclusion in (("completed", "failure"), ("in_progress", ""), ("completed", "skipped")):
            jobs = [job(name, status=status, conclusion=conclusion) if name == guard.REQUIRED_JOBS[1] else job(name)
                    for name in guard.REQUIRED_JOBS]
            with self.subTest(status=status, conclusion=conclusion):
                run = workflow_run()
                with self.assertRaisesRegex(guard.GateError, "Dependency advisories"):
                    guard.verify_server_jobs([run], {attempt_key(run): jobs + [job(guard.MOBILE_SCOPE_JOB, job_id=102), job(guard.MOBILE_JOB, conclusion="failure", job_id=103)]}, SHA, BRANCH)

    def test_missing_ambiguous_stale_job_and_missing_inventory_refuse(self) -> None:
        run = workflow_run()
        with self.assertRaisesRegex(guard.GateError, "missing or ambiguous"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs()[:1]}, SHA, BRANCH)
        with self.assertRaisesRegex(guard.GateError, "missing or ambiguous"):
            guard.verify_server_jobs([run], {attempt_key(run): passing_server_jobs() + [job(guard.REQUIRED_JOBS[0])]}, SHA, BRANCH)
        stale = [job(name, sha="b" * 40) for name in guard.REQUIRED_JOBS]
        stale.extend([job(guard.MOBILE_SCOPE_JOB, job_id=102), job(guard.MOBILE_JOB, conclusion="failure", job_id=103)])
        with self.assertRaisesRegex(guard.GateError, "different run or SHA"):
            guard.verify_server_jobs([run], {attempt_key(run): stale}, SHA, BRANCH)
        with self.assertRaisesRegex(guard.GateError, "inventory is missing or malformed"):
            guard.verify_server_jobs([run], {}, SHA, BRANCH)

    def test_invalid_run_inventory_refuses(self) -> None:
        for runs in ([], [None], [workflow_run(run_id=True)], [workflow_run(run_number="7")],
                     [workflow_run(), workflow_run(run_id=RUN_ID, run_number=8)]):
            with self.subTest(runs=runs):
                with self.assertRaises(guard.GateError):
                    guard.verify_server_jobs(runs, {(RUN_ID, 1): passing_server_jobs()}, SHA, BRANCH)


class FetchApiTests(unittest.TestCase):
    class Response:
        def __init__(self, body: dict | None = None, link: str | None = None, raw: bytes | None = None):
            self._body = raw if raw is not None else json.dumps(body).encode("utf-8")
            self.headers = {"Link": link} if link else {}

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def read(self, size: int = -1):
            return self._body if size < 0 else self._body[:size]

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

    def test_api_opener_rejects_redirects(self) -> None:
        self.assertTrue(any(isinstance(handler, guard._NoRedirectHandler)
                            for handler in guard._API_OPENER.handlers))
        handler = guard._NoRedirectHandler()
        request = guard.Request("https://api.github.com/example")
        self.assertIsNone(handler.redirect_request(
            request, None, 302, "Found", {}, "https://example.invalid/redirect"
        ))

    def test_fetch_workflow_runs_refuses_empty_or_incomplete_inventory(self) -> None:
        for body, message in (({"total_count": 0, "workflow_runs": []}, "has no push run"),
                              ({"total_count": 1, "workflow_runs": []}, "inventory is incomplete")):
            with self.subTest(body=body), patch.object(guard, "urlopen", return_value=self.Response(body)):
                with self.assertRaisesRegex(guard.GateError, message):
                    guard.fetch_workflow_runs(SHA, BRANCH)

    def test_fetch_jobs_paginates_and_checks_inventory_size(self) -> None:
        first = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[0])]}
        second = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[1])]}
        path = f"/repos/{guard.REPOSITORY}/actions/runs/{RUN_ID}/attempts/1/jobs"
        responses = [self.Response(first, f'<https://api.github.com{path}?filter=latest&per_page=100&page=2>; rel="next"'), self.Response(second)]
        with patch.object(guard, "urlopen", side_effect=responses) as open_url:
            jobs = guard.fetch_jobs(RUN_ID, 1)
        self.assertEqual(open_url.call_count, 2)
        self.assertIn(f"/actions/runs/{RUN_ID}/attempts/1/jobs", open_url.call_args_list[0].args[0].full_url)
        self.assertEqual(len(jobs), 2)

    def test_fetch_runs_paginates_from_the_scoped_endpoint(self) -> None:
        path = f"/repos/{guard.REPOSITORY}/actions/workflows/{guard.VALIDATE_WORKFLOW_ID}/runs"
        first = {"total_count": 2, "workflow_runs": [workflow_run()]}
        second_run = workflow_run(run_id=302, run_number=8)
        second = {"total_count": 2, "workflow_runs": [second_run]}
        responses = [self.Response(first, f'<https://api.github.com{path}?page=2>; rel="next"'), self.Response(second)]
        with patch.object(guard, "urlopen", side_effect=responses) as open_url:
            runs = guard.fetch_workflow_runs(SHA, BRANCH)
        self.assertEqual(open_url.call_count, 2)
        self.assertEqual([run["id"] for run in runs], [RUN_ID, 302])

    def test_pagination_count_change_refuses(self) -> None:
        path = f"/repos/{guard.REPOSITORY}/actions/runs/{RUN_ID}/attempts/1/jobs"
        first = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[0])]}
        second = {"total_count": 3, "jobs": [job(guard.REQUIRED_JOBS[1], job_id=102)]}
        responses = [self.Response(first, f'<https://api.github.com{path}?page=2>; rel="next"'), self.Response(second)]
        with patch.object(guard, "urlopen", side_effect=responses):
            with self.assertRaisesRegex(guard.GateError, "count changed"):
                guard.fetch_jobs(RUN_ID, 1)

    def test_pagination_to_wrong_host_or_path_refuses(self) -> None:
        path = f"/repos/{guard.REPOSITORY}/actions/runs/{RUN_ID}/attempts/1/jobs"
        body = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[0])]}
        for link in ('<https://attacker.example' + path + '?page=2>; rel="next"',
                     '<https://api.github.com/repos/other/repo/jobs?page=2>; rel="next"'):
            with self.subTest(link=link), patch.object(guard, "urlopen", return_value=self.Response(body, link)):
                with self.assertRaisesRegex(guard.GateError, "outside the trusted API endpoint"):
                    guard.fetch_jobs(RUN_ID, 1)

    def test_pagination_page_cap_refuses(self) -> None:
        path = f"/repos/{guard.REPOSITORY}/actions/runs/{RUN_ID}/attempts/1/jobs"
        def response_for_page(page: int) -> "FetchApiTests.Response":
            body = {"total_count": guard.MAX_PAGES + 1, "jobs": [job(guard.REQUIRED_JOBS[0], job_id=100 + page)]}
            link = (f'<https://api.github.com{path}?page={page + 1}>; rel="next"'
                    if page < guard.MAX_PAGES + 1 else None)
            return self.Response(body, link)
        responses = [response_for_page(page) for page in range(1, guard.MAX_PAGES + 1)]
        with patch.object(guard, "urlopen", side_effect=responses) as open_url:
            with self.assertRaisesRegex(guard.GateError, "pagination exceeded"):
                guard.fetch_jobs(RUN_ID, 1)
        self.assertEqual(open_url.call_count, guard.MAX_PAGES)

    def test_fetch_jobs_refuses_incomplete_inventory(self) -> None:
        body = {"total_count": 2, "jobs": [job(guard.REQUIRED_JOBS[0])]}
        with patch.object(guard, "urlopen", return_value=self.Response(body)):
            with self.assertRaisesRegex(guard.GateError, "job inventory is incomplete"):
                guard.fetch_jobs(RUN_ID, 1)

    def test_fetch_jobs_refuses_duplicate_ids(self) -> None:
        duplicate = job(guard.REQUIRED_JOBS[0])
        body = {"total_count": 2, "jobs": [duplicate, duplicate]}
        with patch.object(guard, "urlopen", return_value=self.Response(body)):
            with self.assertRaisesRegex(guard.GateError, "duplicate job ids"):
                guard.fetch_jobs(RUN_ID, 1)

    def test_duplicate_json_keys_and_oversized_response_refuse(self) -> None:
        duplicate = self.Response(raw=b'{"total_count":1,"total_count":1,"workflow_runs":[]}')
        with patch.object(guard, "urlopen", return_value=duplicate):
            with self.assertRaisesRegex(guard.GateError, "duplicate JSON key"):
                guard.fetch_workflow_runs(SHA, BRANCH)
        oversized = self.Response(raw=b"x" * (guard.MAX_RESPONSE_BYTES + 1))
        with patch.object(guard, "urlopen", return_value=oversized):
            with self.assertRaisesRegex(guard.GateError, "exceeds the size limit"):
                guard.fetch_workflow_runs(SHA, BRANCH)

    def test_incomplete_read_through_main_fails_with_clean_diagnostic(self) -> None:
        stdout, stderr = io.StringIO(), io.StringIO()
        error = http.client.IncompleteRead(b"truncated")
        with patch.object(guard, "fetch_workflow_runs", side_effect=error), \
             patch.object(sys, "argv", ["deploy_guard.py", "--commit", SHA, "--branch", BRANCH]), \
             contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = guard.main()
        self.assertEqual(result, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("RELEASE CHECK BLOCKED:", stderr.getvalue())

    def test_in_progress_run_does_not_fetch_jobs(self) -> None:
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.object(guard, "fetch_workflow_runs", return_value=[workflow_run(status="in_progress")]), \
             patch.object(guard, "fetch_jobs") as fetch_jobs, \
             patch.object(sys, "argv", ["deploy_guard.py", "--commit", SHA, "--branch", BRANCH]), \
             contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = guard.main()
        self.assertEqual(result, 1)
        fetch_jobs.assert_not_called()
        self.assertIn("not completed", stderr.getvalue())


class DeployScriptContractTests(unittest.TestCase):
    def test_guard_test_is_not_copied_into_python_service_image(self) -> None:
        dockerfile = (ROOT / "python" / "Dockerfile").read_text(encoding="utf-8")
        workflow = (ROOT / ".github" / "workflows" / "validate.yml").read_text(encoding="utf-8")
        self.assertTrue((ROOT / "ops" / "vps" / "test_deploy_guard.py").is_file())
        self.assertFalse((ROOT / "python" / "tests" / "test_deploy_guard.py").exists())
        self.assertIn("COPY python/tests ./tests", dockerfile)
        self.assertIn("python -B -m unittest discover -s ops/vps -p 'test_deploy_guard.py'", workflow)

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
