"""Tests for the surface-scoped JavaScript advisory gate.

Run from the repository root: python -m unittest discover -s ops/ci -p 'test_*.py'
Kept beside the script, outside python/tests, on purpose: python/tests is run
inside the python Docker image, which does not (and should not) contain CI
tooling.
"""
import copy
import io
import json
import os
import pathlib
import re
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest import mock

import js_advisory_gate as gate

REPO = pathlib.Path(__file__).resolve().parents[2]


def adv(severity="high"):
    return [{"id": 1, "severity": severity, "url": "https://example.invalid/GHSA-test", "title": "t", "vulnerable_versions": "<=1"}]


HIGH = adv()


def lock(extra_packages=None, extra_workspaces=None):
    """client -> ui -> shared(1.0); mobile -> tool -> risky, and mobile's own shared(2.0) -> risky."""
    base = {
        "workspaces": {
            "": {"name": "root"},
            "apps/client": {"name": "@app/client", "dependencies": {"ui": "^1", "@app/types": "workspace:*"}},
            "apps/mobile": {"name": "@app/mobile", "dependencies": {"tool": "^1", "shared": "^2"}},
            "packages/shared-types": {"name": "@app/types"},
        },
        "packages": {
            "@app/types": ["@app/types@workspace:packages/shared-types"],
            "ui": ["ui@1.0.0", "", {"dependencies": {"shared": "^1"}}, "sha"],
            "shared": ["shared@1.0.0", "", {}, "sha"],
            "tool": ["tool@1.0.0", "", {"dependencies": {"risky": "^1"}, "peerDependencies": {"absent-peer": "*"}}, "sha"],
            "risky": ["risky@1.0.0", "", {}, "sha"],
            "@app/mobile/shared": ["shared@2.0.0", "", {"dependencies": {"risky": "^1"}}, "sha"],
        },
    }
    base["packages"].update(extra_packages or {})
    base["workspaces"].update(extra_workspaces or {})
    return base


class GateTests(unittest.TestCase):
    def test_advisory_reachable_only_from_mobile_blocks_mobile_not_server(self):
        audit = {"risky": HIGH}
        blocked, elsewhere = gate.decide(lock(), audit, "server")
        self.assertEqual(blocked, {})
        self.assertEqual(elsewhere, {"risky": ["mobile"]})
        blocked, _ = gate.decide(lock(), audit, "mobile")
        self.assertEqual(list(blocked), ["risky"])

    def test_advisory_reachable_from_server_blocks_server(self):
        blocked, _ = gate.decide(lock(), {"shared": HIGH}, "server")
        self.assertEqual(list(blocked), ["shared"])

    def test_workspace_nested_override_is_resolved_for_that_workspace_only(self):
        # The client resolves the root `shared`, which has no path to `risky`;
        # mobile resolves its own nested copy, which does.
        self.assertNotIn("risky", gate.names(lock(), gate.closure(lock(), "apps/client")))
        self.assertIn("@app/mobile/shared", gate.closure(lock(), "apps/mobile"))

    def test_name_match_blocks_even_a_different_copy(self):
        # `shared` is advised; mobile reaches a copy of it, so it is blocked
        # whatever the version (over-blocks, never under-blocks).
        blocked, _ = gate.decide(lock(), {"shared": HIGH}, "mobile")
        self.assertEqual(list(blocked), ["shared"])

    def test_moderate_advisories_do_not_block(self):
        blocked, elsewhere = gate.decide(lock(), {"shared": adv("moderate")}, "server")
        self.assertEqual((blocked, elsewhere), ({}, {}))

    def test_critical_blocks(self):
        blocked, _ = gate.decide(lock(), {"ui": adv("critical")}, "server")
        self.assertEqual(list(blocked), ["ui"])

    def test_unclassified_workspace_cannot_be_decided(self):
        with self.assertRaisesRegex(gate.CannotDecide, "not assigned to any surface"):
            gate.decide(lock(extra_workspaces={"apps/new": {"name": "@app/new"}}), {}, "server")

    def test_missing_surface_workspace_cannot_be_decided(self):
        l = lock()
        del l["workspaces"]["apps/mobile"]
        with self.assertRaisesRegex(gate.CannotDecide, "inconsistent"):
            gate.decide(l, {}, "server")

    def test_unresolvable_regular_dependency_cannot_be_decided(self):
        l = lock()
        del l["packages"]["shared"]
        with self.assertRaisesRegex(gate.CannotDecide, "not in the lockfile"):
            gate.decide(l, {}, "server")

    def test_advised_package_nobody_reaches_cannot_be_decided(self):
        with self.assertRaisesRegex(gate.CannotDecide, "reachability unknown"):
            gate.decide(lock(extra_packages={"orphan": ["orphan@1.0.0", "", {}, "sha"]}), {"orphan": HIGH}, "server")

    def test_absent_optional_peer_is_tolerated(self):
        self.assertIn("tool", gate.closure(lock(), "apps/mobile"))

    def test_scoped_names_split_correctly(self):
        self.assertEqual(gate.split_key("@a/b/c/@d/e"), ["@a/b", "c", "@d/e"])

    def test_malformed_audit_cannot_be_decided(self):
        with self.assertRaises(gate.CannotDecide):
            gate.decide(lock(), ["not", "an", "object"], "server")

    def test_main_exit_codes_with_a_saved_audit(self):
        with tempfile.TemporaryDirectory() as d:
            lp, ap = os.path.join(d, "bun.lock"), os.path.join(d, "audit.json")
            # A trailing comma, as bun writes them.
            with open(lp, "w") as f:
                f.write(json.dumps(lock())[:-1] + ",}")
            with open(ap, "w") as f:
                f.write(json.dumps({"risky": HIGH}))
            with redirect_stdout(io.StringIO()):
                self.assertEqual(gate.main(["--surface", "server", "--lock", lp, "--audit-json", ap]), 0)
                self.assertEqual(gate.main(["--surface", "mobile", "--lock", lp, "--audit-json", ap]), 1)
                self.assertEqual(gate.main(["--surface", "server", "--lock", os.path.join(d, "absent"), "--audit-json", ap]), 1)

    def test_real_lockfile_classifies_every_workspace_and_resolves(self):
        l = gate.load_lock(REPO / "bun.lock")
        blocked, _ = gate.decide(copy.deepcopy(l), {}, "server")
        self.assertEqual(blocked, {})
        server = set().union(*(gate.names(l, gate.closure(l, w)) for w in gate.SURFACES["server"]))
        mobile = gate.names(l, gate.closure(l, "apps/mobile"))
        # The mobile toolchain is where Metro and Expo live; the server surface has neither.
        self.assertIn("expo", mobile)
        self.assertNotIn("expo", server)
        self.assertNotIn("react-native", server)
        self.assertIn("react", server)


FINDINGS = json.dumps({"risky": adv()})
REGISTRY_ERROR = "error: POST https://registry.npmjs.org/-/npm/v1/security/advisories/bulk - ConnectionRefused"


def run_main(surface, returncode, stdout, stderr, extra=()):
    """Drives the real CLI path with a faked `bun audit` process result."""
    with tempfile.TemporaryDirectory() as d:
        lp = os.path.join(d, "bun.lock")
        with open(lp, "w") as f:
            f.write(json.dumps(lock()))
        proc = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
        out = io.StringIO()
        with mock.patch.object(gate.subprocess, "run", return_value=proc) as run, redirect_stdout(out):
            code = gate.main(["--surface", surface, "--lock", lp, *extra])
        assert run.call_args[0][0][:2] == ["bun", "audit"], run.call_args
        return code, out.getvalue()


class AuditInterpretationTests(unittest.TestCase):
    """A failed audit must never read as a clean one (false pass found in review of 93b2444)."""

    def test_empty_stdout_on_nonzero_exit_fails_closed(self):
        for surface in ("server", "mobile"):
            code, out = run_main(surface, 1, "", "")
            self.assertEqual(code, 1)
            self.assertIn("CANNOT DECIDE", out)
            self.assertIn("no output", out)
            self.assertNotIn("no high or critical advisory", out)

    def test_registry_failure_without_json_fails_closed_and_says_why(self):
        code, out = run_main("server", 1, "", REGISTRY_ERROR)
        self.assertEqual(code, 1)
        self.assertIn("CANNOT DECIDE", out)
        self.assertIn("ConnectionRefused", out)
        self.assertNotIn("no high or critical advisory", out)

    def test_registry_failure_with_json_output_still_fails_closed(self):
        # Even a well-formed (and here, clean-looking) body is not trusted
        # when the diagnostics say the request failed.
        for body in ("{}", FINDINGS):
            code, out = run_main("server", 1, body, REGISTRY_ERROR)
            self.assertEqual(code, 1, body)
            self.assertIn("printed diagnostics", out)

    def test_any_unfamiliar_stderr_fails_closed_even_with_a_valid_body(self):
        # Found in review of 16caa76: a diagnostic the gate did not recognise
        # ("registry unavailable; retry later") beside a well-formed,
        # mobile-only advisory passed the server gate. No stderr is benign.
        diagnostics = ("registry unavailable; retry later", "audit request failed", "warn: something new",
                       "note: cache refreshed", "⚠ partial results", "x")
        for stderr in diagnostics:
            for returncode, body in ((1, FINDINGS), (1, "{}"), (0, "{}"), (0, FINDINGS)):
                for surface in ("server", "mobile"):
                    code, out = run_main(surface, returncode, body, stderr)
                    case = (stderr, returncode, body, surface)
                    self.assertEqual(code, 1, case)
                    self.assertIn("CANNOT DECIDE", out, case)
                    self.assertIn(stderr, out, case)
                    self.assertNotIn("no high or critical advisory", out, case)
                    self.assertNotIn("not reachable from", out, case)

    def test_whitespace_only_stderr_is_not_a_diagnostic(self):
        # The one explicitly accepted stderr shape: nothing but whitespace.
        code, out = run_main("server", 1, FINDINGS, " \n\t\r\n")
        self.assertEqual(code, 0)
        code, out = run_main("mobile", 1, FINDINGS, "\n")
        self.assertEqual(code, 1)
        self.assertIn("BLOCKED mobile: risky", out)

    def test_unfamiliar_stderr_fails_closed_on_the_real_lockfile(self):
        # The reviewer's exact probe: real bun.lock, braces advised (mobile-only).
        body = json.dumps({"braces": adv()})
        proc = mock.Mock(returncode=1, stdout=body, stderr="registry unavailable; retry later")
        for surface in ("server", "mobile"):
            out = io.StringIO()
            with mock.patch.object(gate.subprocess, "run", return_value=proc), redirect_stdout(out):
                code = gate.main(["--surface", surface, "--lock", str(REPO / "bun.lock")])
            self.assertEqual(code, 1, surface)
            self.assertIn("CANNOT DECIDE", out.getvalue())
        # Control: the same body with a silent stderr is a normal finding.
        proc = mock.Mock(returncode=1, stdout=body, stderr="")
        out = io.StringIO()
        with mock.patch.object(gate.subprocess, "run", return_value=proc), redirect_stdout(out):
            self.assertEqual(gate.main(["--surface", "server", "--lock", str(REPO / "bun.lock")]), 0)
        self.assertIn("not reachable from server: braces (reachable from: mobile)", out.getvalue())

    def test_malformed_json_fails_closed(self):
        bodies = ("{", "<html>503</html>", "null", "[]", '{"risky": "high"}', '{"risky": [{"severity": "high"}]}',
                  '{"risky": [{"id": 1, "url": "u", "severity": "catastrophic"}]}', '{"risky": []}')
        for body in bodies:
            code, out = run_main("server", 1, body, "")
            self.assertEqual(code, 1, body)
            self.assertIn("CANNOT DECIDE", out, body)

    def test_exit_status_and_body_must_agree(self):
        code, out = run_main("server", 0, FINDINGS, "")
        self.assertEqual(code, 1)
        self.assertIn("exited 0 but listed advisories", out)
        code, out = run_main("server", 1, "{}", "")
        self.assertEqual(code, 1)
        self.assertIn("without listing any advisory", out)
        code, out = run_main("server", 2, "{}", "")
        self.assertEqual(code, 1)
        self.assertIn("exited 2", out)

    def test_valid_advisory_response_blocks_only_the_surface_that_reaches_it(self):
        code, out = run_main("mobile", 1, FINDINGS, "")
        self.assertEqual(code, 1)
        self.assertIn("BLOCKED mobile: risky high", out)
        code, out = run_main("server", 1, FINDINGS, "")
        self.assertEqual(code, 0)
        self.assertIn("not reachable from server: risky (reachable from: mobile)", out)
        code, out = run_main("server", 1, json.dumps({"shared": adv()}), "")
        self.assertEqual(code, 1)
        self.assertIn("BLOCKED server: shared high", out)

    def test_clean_valid_response_passes(self):
        for surface in ("server", "mobile"):
            code, out = run_main(surface, 0, "{}", "")
            self.assertEqual(code, 0)
            self.assertIn(f"{surface}: no high or critical advisory reachable", out)

    def test_audit_that_cannot_be_started_fails_closed(self):
        with tempfile.TemporaryDirectory() as d:
            lp = os.path.join(d, "bun.lock")
            with open(lp, "w") as f:
                f.write(json.dumps(lock()))
            out = io.StringIO()
            with mock.patch.object(gate.subprocess, "run", side_effect=FileNotFoundError("bun")), redirect_stdout(out):
                self.assertEqual(gate.main(["--surface", "server", "--lock", lp]), 1)
            self.assertIn("could not be run", out.getvalue())


def tree(root, *paths):
    for p in paths:
        os.makedirs(os.path.join(root, *p.split("/")))


class InstalledTreeTests(unittest.TestCase):
    """`--installed-root`: the boundary is whatever is really on disk."""

    def test_advised_package_installed_in_a_server_checkout_blocks_the_server(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/.bun/ui@1.0.0", "node_modules/.bun/risky@1.0.0+abc", "apps/client/node_modules/ui")
            code, out = run_main("server", 1, FINDINGS, "", ["--installed-root", d])
            self.assertEqual(code, 1)
            self.assertIn("BLOCKED server: risky", out)

    def test_server_only_install_passes(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/.bun/ui@1.0.0", "node_modules/.bun/@scope+pkg@2.0.0", "apps/client/node_modules/ui")
            code, out = run_main("server", 1, FINDINGS, "", ["--installed-root", d])
            self.assertEqual(code, 0)
            self.assertIn("reachable or installed", out)

    def test_both_layouts_are_seen(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/.bun/@scope+pkg@2.0.0", "node_modules/.bun/plain@1.0.0",
                 "apps/x/node_modules/hoisted/node_modules/nested", "apps/x/node_modules/@org/thing")
            self.assertEqual(gate.installed_packages(d), {"@scope/pkg", "plain", "hoisted", "nested", "@org/thing"})

    def test_nothing_installed_cannot_be_decided(self):
        with tempfile.TemporaryDirectory() as d:
            code, out = run_main("server", 0, "{}", "", ["--installed-root", d])
            self.assertEqual(code, 1)
            self.assertIn("nothing is installed to check", out)


SERVER_INSTALL = "bun install --frozen-lockfile --filter '@stockspotter/client' --filter '@stockspotter/shared-types'"


def jobs(workflow_text):
    """{job id: text} for a workflow whose jobs are two-space-indented keys under `jobs:`."""
    body = workflow_text.replace("\r\n", "\n").split("\njobs:\n", 1)[1]
    out, current = {}, None
    for line in body.split("\n"):
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            current = m.group(1)
            out[current] = ""
        elif current:
            out[current] += line + "\n"
    return out


def commands(text):
    """Executable lines only: comments must not satisfy (or trip) an assertion."""
    return "\n".join(l for l in text.replace("\r\n", "\n").split("\n") if not l.lstrip().startswith("#"))


def installs(text):
    return [m.strip() for m in re.findall(r"bun install[^\n]*", text)]


class BoundaryTests(unittest.TestCase):
    """The declared server surface is the one CI and the web image builder really install."""

    def setUp(self):
        workflow = (REPO / ".github/workflows/validate.yml").read_text(encoding="utf-8")
        self.jobs = {k: commands(v) for k, v in jobs(workflow).items()}
        self.dockerfile = commands((REPO / "apps/client/Dockerfile").read_text(encoding="utf-8"))

    def test_server_jobs_install_only_the_server_workspaces(self):
        for job in ("checks", "audit"):
            self.assertEqual(installs(self.jobs[job]), [SERVER_INSTALL], job)
            self.assertNotIn("apps/mobile/tsconfig.json", self.jobs[job], job)
            self.assertNotIn("expo", self.jobs[job].lower(), job)

    def test_web_image_builder_installs_only_the_server_workspaces(self):
        self.assertEqual(installs(self.dockerfile), [SERVER_INSTALL])

    def test_filters_are_exactly_the_server_surface_workspaces(self):
        l = gate.load_lock(REPO / "bun.lock")
        named = {l["workspaces"][w]["name"] for w in gate.SURFACES["server"] if w}
        self.assertEqual(set(re.findall(r"--filter '([^']+)'", SERVER_INSTALL)), named)

    def test_server_gate_checks_the_installed_tree_and_is_not_softened(self):
        audit = self.jobs["audit"]
        self.assertIn("python3 ops/ci/js_advisory_gate.py --surface server --installed-root .", audit)
        for forbidden in ("--ignore", "continue-on-error", "|| true", "--audit-json"):
            self.assertNotIn(forbidden, audit, forbidden)

    def test_mobile_is_the_only_full_install_and_stays_blocking(self):
        full = {j for j, t in self.jobs.items() if any("--filter" not in i for i in installs(t))}
        self.assertEqual(full, {"mobile"})
        mobile = self.jobs["mobile"]
        self.assertIn("bun x tsc --noEmit -p apps/mobile/tsconfig.json", mobile)
        gate_line = next(l for l in mobile.split("\n") if "js_advisory_gate.py" in l)
        self.assertIn("--surface mobile --installed-root .", gate_line)
        self.assertNotIn("|| true", gate_line)
        self.assertNotIn("continue-on-error", mobile)
        self.assertNotIn("--ignore", mobile)

    def test_every_job_is_accounted_for(self):
        # A new job that installs JavaScript must be classified here first.
        self.assertEqual(set(self.jobs), {"checks", "audit", "mobile"})


if __name__ == "__main__":
    unittest.main()
