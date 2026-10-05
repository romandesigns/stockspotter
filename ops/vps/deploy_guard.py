#!/usr/bin/env python3
"""Fail-closed exact-commit GitHub Actions check for VPS deployment."""

from __future__ import annotations

import argparse
import http.client
import json
import re
import sys
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import Request, urlopen


REPOSITORY = "romandesigns/stockspotter"
# Pin the workflow identity as well as the job names: names and the Actions app
# ID alone are shared by every workflow in the repository.
VALIDATE_WORKFLOW_ID = 354398814
REQUIRED_JOBS = ("Tests, lint and build", "Dependency advisories")
API_ROOT = "https://api.github.com"
MAX_PAGES = 20
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
MOBILE_JOB = "Mobile (types and dependency advisories)"


class GateError(ValueError):
    """The API response cannot prove the required server checks passed."""


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise GateError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def _next_link(header: str | None) -> str | None:
    if not header:
        return None
    for item in header.split(","):
        match = re.fullmatch(r'\s*<([^>]+)>\s*;\s*rel="next"\s*', item)
        if match:
            return match.group(1)
    return None


def _validated_next_link(header: str | None, endpoint_path: str) -> str | None:
    url = _next_link(header)
    if url is None:
        return None
    try:
        parsed = urlsplit(url)
    except ValueError as error:
        raise GateError("GitHub pagination link is malformed") from error
    if (parsed.scheme != "https" or parsed.netloc != "api.github.com"
            or parsed.path != endpoint_path or parsed.username is not None
            or parsed.password is not None or parsed.fragment):
        raise GateError("GitHub pagination link is outside the trusted API endpoint")
    return url


def _read_response(response: Any, description: str) -> bytes:
    raw = response.read(MAX_RESPONSE_BYTES + 1)
    if len(raw) > MAX_RESPONSE_BYTES:
        raise GateError(f"GitHub {description} response exceeds the size limit")
    return raw


def fetch_workflow_runs(commit: str, branch: str) -> list[dict[str, Any]]:
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise GateError("commit must be a full lowercase Git object id")
    if branch != "master" and not re.fullmatch(r"release/[^/].*", branch):
        raise GateError(f"unsupported deploy branch {branch!r}")

    query = urlencode({
        "head_sha": commit,
        "branch": branch,
        "event": "push",
        "per_page": 100,
    })
    url: str | None = (
        f"{API_ROOT}/repos/{REPOSITORY}/actions/workflows/"
        f"{VALIDATE_WORKFLOW_ID}/runs?{query}"
    )
    seen_urls: set[str] = set()
    endpoint_path = f"/repos/{REPOSITORY}/actions/workflows/{VALIDATE_WORKFLOW_ID}/runs"
    runs: list[dict[str, Any]] = []
    expected_total: int | None = None

    for _ in range(MAX_PAGES):
        if url is None:
            break
        if url in seen_urls:
            raise GateError("GitHub workflow-run pagination loop")
        seen_urls.add(url)
        request = Request(
            url,
            headers={
                "Accept": "application/vnd.github+json",
                "User-Agent": "stockspotter-vps-deploy-check",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        with urlopen(request, timeout=15) as response:
            raw = _read_response(response, "workflow-run")
            next_url = _validated_next_link(response.headers.get("Link"), endpoint_path)
        try:
            page = json.loads(raw, object_pairs_hook=_reject_duplicate_keys)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise GateError(f"GitHub returned malformed workflow-run JSON: {error}") from error
        if not isinstance(page, dict):
            raise GateError("GitHub workflow-run response is not an object")
        total = page.get("total_count")
        page_runs = page.get("workflow_runs")
        if isinstance(total, bool) or not isinstance(total, int) or total < 0:
            raise GateError("GitHub workflow-run total_count is missing or invalid")
        if not isinstance(page_runs, list) or any(not isinstance(run, dict) for run in page_runs):
            raise GateError("GitHub workflow_runs is missing or invalid")
        if expected_total is None:
            expected_total = total
        elif expected_total != total:
            raise GateError("GitHub workflow-run count changed during pagination")
        runs.extend(page_runs)
        url = next_url
    else:
        raise GateError("GitHub workflow-run pagination exceeded the safety limit")

    if expected_total is None or len(runs) != expected_total:
        raise GateError("GitHub workflow-run inventory is incomplete")
    if not runs:
        raise GateError(f"trusted Validate workflow has no push run for {branch} at {commit}")
    run_ids = [run.get("id") for run in runs]
    if any(not _positive_int(run_id) for run_id in run_ids) or len(set(run_ids)) != len(run_ids):
        raise GateError("GitHub workflow-run inventory has invalid or duplicate run ids")
    return runs


def fetch_jobs(run_id: int, attempt: int) -> list[dict[str, Any]]:
    if isinstance(run_id, bool) or not isinstance(run_id, int) or run_id <= 0:
        raise GateError("workflow run has an invalid id")
    if not _positive_int(attempt):
        raise GateError("workflow run has an invalid attempt")
    url: str | None = (
        f"{API_ROOT}/repos/{REPOSITORY}/actions/runs/{run_id}/attempts/{attempt}/jobs"
        "?per_page=100"
    )
    seen_urls: set[str] = set()
    endpoint_path = f"/repos/{REPOSITORY}/actions/runs/{run_id}/attempts/{attempt}/jobs"
    jobs: list[dict[str, Any]] = []
    expected_total: int | None = None
    for _ in range(MAX_PAGES):
        if url is None:
            break
        if url in seen_urls:
            raise GateError("GitHub job pagination loop")
        seen_urls.add(url)
        request = Request(
            url,
            headers={
                "Accept": "application/vnd.github+json",
                "User-Agent": "stockspotter-vps-deploy-check",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        with urlopen(request, timeout=15) as response:
            raw = _read_response(response, "job")
            next_url = _validated_next_link(response.headers.get("Link"), endpoint_path)
        try:
            page = json.loads(raw, object_pairs_hook=_reject_duplicate_keys)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise GateError(f"GitHub returned malformed job JSON: {error}") from error
        if not isinstance(page, dict):
            raise GateError("GitHub jobs response is not an object")
        total = page.get("total_count")
        page_jobs = page.get("jobs")
        if isinstance(total, bool) or not isinstance(total, int) or total < 0:
            raise GateError("GitHub jobs total_count is missing or invalid")
        if not isinstance(page_jobs, list) or any(not isinstance(job, dict) for job in page_jobs):
            raise GateError("GitHub jobs is missing or invalid")
        if expected_total is None:
            expected_total = total
        elif expected_total != total:
            raise GateError("GitHub job count changed during pagination")
        jobs.extend(page_jobs)
        url = next_url
    else:
        raise GateError("GitHub job pagination exceeded the safety limit")

    if expected_total is None or len(jobs) != expected_total:
        raise GateError("GitHub job inventory is incomplete")
    job_ids = [job.get("id") for job in jobs]
    if any(not _positive_int(job_id) for job_id in job_ids) or len(set(job_ids)) != len(job_ids):
        raise GateError("GitHub job inventory has invalid or duplicate job ids")
    return jobs


def _positive_int(value: Any) -> bool:
    return not isinstance(value, bool) and isinstance(value, int) and value > 0


def _latest_run(runs: Any) -> dict[str, Any]:
    if not isinstance(runs, list) or not runs or any(not isinstance(run, dict) for run in runs):
        raise GateError("trusted workflow-run inventory is missing or malformed")
    for run in runs:
        if not _positive_int(run.get("id")):
            raise GateError("GitHub workflow run has an invalid id")
        if not _positive_int(run.get("run_number")) or not _positive_int(run.get("run_attempt")):
            raise GateError("GitHub workflow run has an invalid run number or attempt")
    run_ids = [run["id"] for run in runs]
    if len(set(run_ids)) != len(run_ids):
        raise GateError("GitHub workflow-run inventory has duplicate run ids")
    return max(runs, key=lambda run: (run["run_number"], run["run_attempt"]))


def verify_server_jobs(
    runs: list[dict[str, Any]], jobs_by_attempt: dict[tuple[int, int], list[dict[str, Any]]],
    commit: str, branch: str,
) -> None:
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise GateError("commit must be a full lowercase Git object id")
    if branch != "master" and not re.fullmatch(r"release/[^/].*", branch):
        raise GateError(f"unsupported deploy branch {branch!r}")
    _latest_run(runs)

    for run in runs:
        if run.get("workflow_id") != VALIDATE_WORKFLOW_ID:
            raise GateError("GitHub returned a run from an untrusted workflow")
        if run.get("head_sha") != commit or run.get("head_branch") != branch or run.get("event") != "push":
            raise GateError("GitHub returned a Validate run for a different branch, SHA, or event")

    latest = _latest_run(runs)
    if latest.get("status") != "completed":
        raise GateError(f"trusted Validate workflow is not completed (status={latest.get('status')!r})")
    if latest.get("conclusion") not in {"success", "failure"}:
        raise GateError(f"trusted Validate workflow has an unacceptable conclusion: {latest.get('conclusion')!r}")
    run_id = latest["id"]
    attempt = latest["run_attempt"]
    jobs = jobs_by_attempt.get((run_id, attempt))
    if not isinstance(jobs, list) or any(not isinstance(job, dict) for job in jobs):
        raise GateError("trusted Validate job inventory is missing or malformed")

    allowed_names = set(REQUIRED_JOBS) | {MOBILE_JOB}
    if any(job.get("name") not in allowed_names for job in jobs):
        raise GateError("trusted Validate run contains an unclassified job")
    for job in jobs:
        if job.get("run_id") != run_id or job.get("head_sha") != commit:
            raise GateError("Validate job is tied to a different run or SHA")

    for name in (*REQUIRED_JOBS, MOBILE_JOB):
        matching = [job for job in jobs if job.get("name") == name]
        if len(matching) != 1:
            raise GateError(f"required Validate job is missing or ambiguous: {name}")
        job = matching[0]
        if job.get("status") != "completed":
            raise GateError(
                f"Validate job is not completed for {commit}: {name} (status={job.get('status')!r})"
            )
        if name in REQUIRED_JOBS and job.get("conclusion") != "success":
            raise GateError(f"required server job is not successful on {commit}: {name}")
        if name == MOBILE_JOB and job.get("conclusion") not in {"success", "failure"}:
            raise GateError(f"mobile job has an unacceptable conclusion: {job.get('conclusion')!r}")

    mobile_conclusion = next(job["conclusion"] for job in jobs if job["name"] == MOBILE_JOB)
    expected_workflow_conclusion = "failure" if mobile_conclusion == "failure" else "success"
    if latest.get("conclusion") != expected_workflow_conclusion:
        raise GateError("Validate workflow conclusion is not explained by the mobile job result")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--branch", required=True)
    args = parser.parse_args()

    try:
        runs = fetch_workflow_runs(args.commit, args.branch)
        latest_run = _latest_run(runs)
        if latest_run.get("status") != "completed":
            raise GateError(f"trusted Validate workflow is not completed (status={latest_run.get('status')!r})")
        latest_id = latest_run["id"]
        latest_attempt = latest_run["run_attempt"]
        jobs = fetch_jobs(latest_id, latest_attempt)
        verify_server_jobs(runs, {(latest_id, latest_attempt): jobs}, args.commit, args.branch)
    except (GateError, HTTPError, URLError, TimeoutError, OSError, http.client.HTTPException) as error:
        print(f"RELEASE CHECK BLOCKED: {error}", file=sys.stderr)
        return 1

    print(f"RELEASE CHECKS PASS: trusted server jobs succeeded for {args.branch} at {args.commit}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
