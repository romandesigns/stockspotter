#!/usr/bin/env python3
"""Fail-closed exact-commit GitHub Actions check for VPS deployment."""

from __future__ import annotations

import argparse
import json
import re
import sys
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


REPOSITORY = "romandesigns/stockspotter"
GITHUB_ACTIONS_APP_ID = 15368
REQUIRED_CHECKS = ("Tests, lint and build", "Dependency advisories")
API_ROOT = "https://api.github.com"
MAX_PAGES = 20


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


def fetch_check_runs(commit: str) -> list[dict[str, Any]]:
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise GateError("commit must be a full lowercase Git object id")

    url: str | None = (
        f"{API_ROOT}/repos/{REPOSITORY}/commits/{commit}/check-runs?per_page=100"
    )
    seen_urls: set[str] = set()
    runs: list[dict[str, Any]] = []
    expected_total: int | None = None

    for _ in range(MAX_PAGES):
        if url is None:
            break
        if url in seen_urls:
            raise GateError("GitHub API pagination loop")
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
            raw = response.read()
            next_url = _next_link(response.headers.get("Link"))

        try:
            page = json.loads(raw, object_pairs_hook=_reject_duplicate_keys)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise GateError(f"GitHub returned malformed check-run JSON: {error}") from error

        if not isinstance(page, dict):
            raise GateError("GitHub check-run response is not an object")
        total = page.get("total_count")
        page_runs = page.get("check_runs")
        if isinstance(total, bool) or not isinstance(total, int) or total < 0:
            raise GateError("GitHub check-run total_count is missing or invalid")
        if not isinstance(page_runs, list) or any(not isinstance(run, dict) for run in page_runs):
            raise GateError("GitHub check_runs is missing or invalid")
        if expected_total is None:
            expected_total = total
        elif expected_total != total:
            raise GateError("GitHub check-run count changed during pagination")
        runs.extend(page_runs)
        url = next_url
    else:
        raise GateError("GitHub check-run pagination exceeded the safety limit")

    if expected_total is None or len(runs) != expected_total:
        raise GateError("GitHub check-run inventory is incomplete")
    return runs


def verify_check_runs(runs: list[dict[str, Any]], commit: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise GateError("commit must be a full lowercase Git object id")
    if not isinstance(runs, list) or any(not isinstance(run, dict) for run in runs):
        raise GateError("check-run inventory is missing or malformed")

    for run in runs:
        if run.get("head_sha") != commit:
            raise GateError("GitHub returned a check run for a different commit")

    for context in REQUIRED_CHECKS:
        matching = [
            run
            for run in runs
            if run.get("name") == context
            and isinstance(run.get("app"), dict)
            and run["app"].get("id") == GITHUB_ACTIONS_APP_ID
        ]
        if not matching:
            raise GateError(f"required check is missing: {context}")

        if any(isinstance(run.get("id"), bool) or not isinstance(run.get("id"), int) for run in matching):
            raise GateError(f"required check has an invalid run id: {context}")
        latest = max(matching, key=lambda run: run["id"])
        if latest.get("status") != "completed" or latest.get("conclusion") != "success":
            raise GateError(
                f"required check is not successful on {commit}: {context} "
                f"(status={latest.get('status')!r}, conclusion={latest.get('conclusion')!r})"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--branch", required=True)
    args = parser.parse_args()

    if args.branch != "master" and not args.branch.startswith("release/"):
        print(f"RELEASE CHECK BLOCKED: unsupported branch {args.branch!r}", file=sys.stderr)
        return 1

    try:
        runs = fetch_check_runs(args.commit)
        verify_check_runs(runs, args.commit)
    except (GateError, HTTPError, URLError, TimeoutError, OSError) as error:
        print(f"RELEASE CHECK BLOCKED: {error}", file=sys.stderr)
        return 1

    print(f"RELEASE CHECKS PASS: required server checks succeeded for {args.commit}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
