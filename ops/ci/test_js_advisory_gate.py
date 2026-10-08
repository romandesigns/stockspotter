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
            "tool": ["tool@1.0.0", "", {"dependencies": {"risky": "^1"}, "peerDependencies": {"absent-peer": "*"}, "optionalPeers": ["absent-peer"]}, "sha"],
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

    def test_aliased_package_is_matched_by_its_real_name(self):
        # The server reaches `real-pkg` only under the alias `alias-cjs`; mobile
        # reaches it by its own name. An advisory against `real-pkg` must block
        # the server too, not read as mobile-only.
        l = lock(extra_packages={
            "alias-cjs": ["real-pkg@1.0.0", "", {}, "sha"],
            "real-pkg": ["real-pkg@2.0.0", "", {}, "sha"],
        })
        l["packages"]["ui"][2]["dependencies"]["alias-cjs"] = "npm:real-pkg@^1"
        l["packages"]["tool"][2]["dependencies"]["real-pkg"] = "^2"
        blocked, _ = gate.decide(l, {"real-pkg": HIGH}, "server")
        self.assertEqual(list(blocked), ["real-pkg"])
        blocked, _ = gate.decide(l, {"real-pkg": HIGH}, "mobile")
        self.assertEqual(list(blocked), ["real-pkg"])

    # --- workspace-to-workspace edges (review of af1f722, finding 1) ---------

    def test_dependencies_of_a_shared_workspace_reach_every_surface_that_uses_it(self):
        # mobile -> @app/types (assigned to the server surface) -> typedep.
        l = lock(extra_packages={"typedep": ["typedep@1.0.0", "", {}, "sha"]})
        l["workspaces"]["packages/shared-types"]["dependencies"] = {"typedep": "^1"}
        l["workspaces"]["apps/mobile"]["dependencies"]["@app/types"] = "workspace:*"
        self.assertIn("typedep", gate.names(l, gate.closure(l, "apps/mobile")))
        self.assertIn("typedep", gate.names(l, gate.closure(l, "apps/client")))
        for surface in ("server", "mobile"):
            blocked, _ = gate.decide(l, {"typedep": HIGH}, surface)
            self.assertEqual(list(blocked), ["typedep"], surface)

    def test_real_mobile_to_shared_types_edge_carries_an_advised_transitive_dependency(self):
        # The reviewer's probe on the real lockfile: mobile depends on
        # @stockspotter/shared-types, which is assigned to the server surface.
        l = gate.load_lock(REPO / "bun.lock")
        self.assertIn("@stockspotter/shared-types", l["workspaces"]["apps/mobile"]["dependencies"])
        l["packages"]["zz-synthetic-advised"] = ["zz-synthetic-advised@1.0.0", "", {}, "sha"]
        l["workspaces"]["packages/shared-types"].setdefault("dependencies", {})["zz-synthetic-advised"] = "^1"
        for surface in ("server", "mobile"):
            blocked, elsewhere = gate.decide(l, {"zz-synthetic-advised": HIGH}, surface)
            self.assertEqual(list(blocked), ["zz-synthetic-advised"], surface)
            self.assertEqual(elsewhere, {}, surface)

    def test_workspace_cycles_terminate_and_missing_workspaces_refuse(self):
        l = lock()
        l["workspaces"]["packages/shared-types"]["dependencies"] = {"@app/client": "workspace:*"}
        l["packages"]["@app/client"] = ["@app/client@workspace:apps/client"]
        self.assertIn("ui", gate.names(l, gate.closure(l, "packages/shared-types")))
        l["packages"]["@app/types"] = ["@app/types@workspace:packages/gone"]
        with self.assertRaisesRegex(gate.CannotDecide, "referenced but not in the lockfile"):
            gate.closure(l, "apps/client")

    # --- peer edges (review of af1f722, finding 2) ----------------------------

    def test_missing_required_peer_cannot_be_decided(self):
        l = lock()
        l["packages"]["tool"][2]["optionalPeers"] = []
        with self.assertRaisesRegex(gate.CannotDecide, "'absent-peer' of 'tool' is not in the lockfile"):
            gate.decide(l, {}, "mobile")
        # The server does not reach `tool`, but the gate computes every
        # surface's closure, so it refuses there as well.
        with self.assertRaises(gate.CannotDecide):
            gate.decide(l, {}, "server")

    def test_missing_optional_peer_is_tolerated_only_when_the_lock_says_optional(self):
        self.assertIn("tool", gate.closure(lock(), "apps/mobile"))
        l = lock()
        del l["packages"]["tool"][2]["optionalPeers"]
        with self.assertRaises(gate.CannotDecide):
            gate.closure(l, "apps/mobile")

    def test_missing_required_workspace_peer_cannot_be_decided(self):
        l = lock()
        l["workspaces"]["apps/client"]["peerDependencies"] = {"host-lib": "*"}
        with self.assertRaisesRegex(gate.CannotDecide, "host-lib"):
            gate.closure(l, "apps/client")
        l["workspaces"]["apps/client"]["optionalPeers"] = ["host-lib"]
        self.assertIn("ui", gate.closure(l, "apps/client"))

    def test_resolved_peer_under_a_workspace_nested_key_is_followed(self):
        # `tool` (mobile) has a required peer `shared`; from tool's position it
        # resolves to the root copy, but a peer declared by a package nested
        # under the mobile workspace resolves to mobile's own copy -> risky.
        l = lock(extra_packages={
            "@app/mobile/plugin": ["plugin@1.0.0", "", {"peerDependencies": {"shared": "^2"}}, "sha"],
            "peer-only": ["peer-only@1.0.0", "", {}, "sha"],
        })
        l["workspaces"]["apps/mobile"]["dependencies"] = {"plugin": "^1"}
        l["packages"]["@app/mobile/shared"] = ["shared@2.0.0", "", {"peerDependencies": {"peer-only": "*"}}, "sha"]
        reached = gate.closure(l, "apps/mobile")
        self.assertIn("@app/mobile/shared", reached)
        self.assertIn("peer-only", reached)
        blocked, _ = gate.decide(l, {"peer-only": HIGH}, "mobile")
        self.assertEqual(list(blocked), ["peer-only"])
        blocked, elsewhere = gate.decide(l, {"peer-only": HIGH}, "server")
        self.assertEqual((blocked, elsewhere), ({}, {"peer-only": ["mobile"]}))

    def test_real_lockfile_has_no_unresolved_required_edge(self):
        l = gate.load_lock(REPO / "bun.lock")
        for workspace in l["workspaces"]:
            gate.closure(l, workspace)  # raises CannotDecide on any

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

    def test_duplicate_member_names_fail_closed(self):
        # Found in review of b44a4e0: Python keeps the LAST of a repeated name,
        # so a later moderate entry hid a high one and the server gate passed.
        high = '{"id":1,"severity":"high","url":"https://x"}'
        moderate = '{"id":2,"severity":"moderate","url":"https://y"}'
        bodies = {
            "package key, high then moderate": '{"shared":[%s],"shared":[%s]}' % (high, moderate),
            "package key, moderate then high": '{"shared":[%s],"shared":[%s]}' % (moderate, high),
            "severity field, high then moderate": '{"shared":[{"id":1,"severity":"high","severity":"moderate","url":"https://x"}]}',
            "severity field, moderate then critical": '{"shared":[{"id":1,"severity":"moderate","severity":"critical","url":"https://x"}]}',
            "identical duplicate": '{"shared":[%s],"shared":[%s]}' % (high, high),
            "nested object": '{"shared":[{"id":1,"severity":"high","url":"https://x","cvss":{"score":1,"score":9}}]}',
        }
        for label, body in bodies.items():
            for surface in ("server", "mobile"):
                code, out = run_main(surface, 1, body, "")
                self.assertEqual(code, 1, label)
                self.assertIn("CANNOT DECIDE", out, label)
                self.assertIn("repeats the member name", out, label)
                self.assertNotIn("no high or critical advisory", out, label)
                self.assertNotIn("BLOCKED", out, label)

    def test_duplicate_package_key_fails_closed_on_the_real_lockfile(self):
        # The reviewer's exact probe.
        body = ('{"braces":[{"id":1,"severity":"high","url":"https://x"}],'
                '"braces":[{"id":2,"severity":"moderate","url":"https://y"}]}')
        proc = mock.Mock(returncode=1, stdout=body, stderr="")
        for surface in ("server", "mobile"):
            out = io.StringIO()
            with mock.patch.object(gate.subprocess, "run", return_value=proc), redirect_stdout(out):
                code = gate.main(["--surface", surface, "--lock", str(REPO / "bun.lock")])
            self.assertEqual(code, 1, surface)
            self.assertIn("repeats the member name 'braces'", out.getvalue())
            self.assertNotIn("no high or critical advisory", out.getvalue())

    def test_non_json_numbers_fail_closed(self):
        for body in ('{"shared":[{"id":NaN,"severity":"high","url":"u"}]}', '{"shared":[{"id":Infinity,"severity":"high","url":"u"}]}'):
            code, out = run_main("server", 1, body, "")
            self.assertEqual(code, 1, body)
            self.assertIn("CANNOT DECIDE", out)

    def test_saved_audit_and_lockfile_get_the_same_strict_parse(self):
        with tempfile.TemporaryDirectory() as d:
            lp, ap = os.path.join(d, "bun.lock"), os.path.join(d, "audit.json")
            good_lock = json.dumps(lock())
            for lock_text, audit_text, needle in (
                (good_lock, '{"risky":[],"risky":[]}', "repeats the member name"),
                (good_lock, '{"risky": "high"}', "unexpected entry"),
                ('{"workspaces":{},"workspaces":{},"packages":{}}', "{}", "repeats the member name"),
            ):
                with open(lp, "w") as f:
                    f.write(lock_text)
                with open(ap, "w") as f:
                    f.write(audit_text)
                out = io.StringIO()
                with redirect_stdout(out):
                    self.assertEqual(gate.main(["--surface", "server", "--lock", lp, "--audit-json", ap]), 1)
                self.assertIn(needle, out.getvalue())

    def test_ordinary_responses_are_unchanged_by_strict_parsing(self):
        two = json.dumps({"risky": adv() + [dict(adv("moderate")[0], id=2)], "ui": adv("low")})
        code, out = run_main("mobile", 1, two, "")
        self.assertEqual(code, 1)
        self.assertIn("BLOCKED mobile: risky high", out)
        code, out = run_main("server", 1, two, "")
        self.assertEqual(code, 0)

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


def link(target, path):
    """A directory link: a symlink, or a junction where Windows refuses symlinks."""
    try:
        os.symlink(target, path, target_is_directory=True)
    except OSError:
        import _winapi
        _winapi.CreateJunction(target, path)


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

    # --- links (review of af1f722, finding 3) ---------------------------------

    def test_link_inside_the_checkout_is_accepted_and_its_target_is_still_scanned(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/.bun/ui@1.0.0/node_modules/ui", "packages/local/node_modules/inner", "apps/client/node_modules")
            link(os.path.join(d, "node_modules", ".bun", "ui@1.0.0", "node_modules", "ui"), os.path.join(d, "apps", "client", "node_modules", "ui"))
            link(os.path.join(d, "packages", "local"), os.path.join(d, "node_modules", "local"))
            found = gate.installed_packages(d)
            self.assertLessEqual({"ui", "local", "inner"}, found)

    def test_link_outside_the_checkout_cannot_be_decided(self):
        with tempfile.TemporaryDirectory() as outside, tempfile.TemporaryDirectory() as d:
            tree(outside, "linked-workspace/node_modules/braces")
            tree(d, "node_modules/.bun/ui@1.0.0")
            link(os.path.join(outside, "linked-workspace"), os.path.join(d, "node_modules", "linked-workspace"))
            with self.assertRaisesRegex(gate.CannotDecide, "links outside the checkout"):
                gate.installed_packages(d)
            code, out = run_main("server", 0, "{}", "", ["--installed-root", d])
            self.assertEqual(code, 1)
            self.assertIn("links outside the checkout", out)
            self.assertNotIn("no high or critical advisory", out)

    def test_scoped_link_outside_the_checkout_cannot_be_decided(self):
        with tempfile.TemporaryDirectory() as outside, tempfile.TemporaryDirectory() as d:
            tree(outside, "pkg/node_modules/braces")
            tree(d, "node_modules/@scope")
            link(os.path.join(outside, "pkg"), os.path.join(d, "node_modules", "@scope", "pkg"))
            with self.assertRaisesRegex(gate.CannotDecide, "links outside the checkout"):
                gate.installed_packages(d)

    def test_link_cycle_terminates(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/a/node_modules", "node_modules/b/node_modules")
            link(os.path.join(d, "node_modules", "b"), os.path.join(d, "node_modules", "a", "node_modules", "b"))
            link(os.path.join(d, "node_modules", "a"), os.path.join(d, "node_modules", "b", "node_modules", "a"))
            link(d, os.path.join(d, "node_modules", "self"))
            self.assertLessEqual({"a", "b", "self"}, gate.installed_packages(d))

    # --- nothing the walk does not read (review of a2573bc) -------------------

    def test_node_modules_linked_into_dot_git_is_still_inventoried(self):
        # The reviewer's probe: `node_modules` is a link to `.git`, with the
        # advised package under `.git/node_modules`. The walk used to prune
        # `.git`, so the scan came back empty and the server passed.
        with tempfile.TemporaryDirectory() as d:
            tree(d, ".git/node_modules/risky", "apps")
            link(os.path.join(d, ".git"), os.path.join(d, "node_modules"))
            self.assertIn("risky", gate.installed_packages(d))
            code, out = run_main("server", 1, FINDINGS, "", ["--installed-root", d])
            self.assertEqual(code, 1)
            self.assertIn("BLOCKED server: risky", out)

    def test_package_linked_into_any_in_checkout_directory_is_inventoried(self):
        for hidden in (".git/store", ".cache/store", "target/store", "dist/store"):
            with tempfile.TemporaryDirectory() as d:
                tree(d, f"{hidden}/pkg/node_modules/risky", "node_modules")
                link(os.path.join(d, *hidden.split("/"), "pkg"), os.path.join(d, "node_modules", "pkg"))
                found = gate.installed_packages(d)
                self.assertLessEqual({"pkg", "risky"}, found, hidden)

    def test_empty_inventory_cannot_be_decided(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules", "apps/client/src")
            with self.assertRaisesRegex(gate.CannotDecide, "empty inventory"):
                gate.installed_packages(d)
            code, out = run_main("server", 0, "{}", "", ["--installed-root", d])
            self.assertEqual(code, 1)
            self.assertNotIn("no high or critical advisory", out)

    def test_unreadable_directory_cannot_be_decided(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/ui", "node_modules/locked")
            locked = os.path.join(d, "node_modules", "locked")
            real_scandir = os.scandir

            def scandir(path="."):
                if os.path.normcase(os.fspath(path)) == os.path.normcase(locked):
                    raise PermissionError(13, "Permission denied", locked)
                return real_scandir(path)

            with mock.patch.object(gate.os, "scandir", scandir):
                with self.assertRaisesRegex(gate.CannotDecide, "cannot read .*locked"):
                    gate.installed_packages(d)

    def test_unreadable_scope_directory_cannot_be_decided(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/ui", "node_modules/@scope/pkg")
            scope = os.path.join(d, "node_modules", "@scope")
            real_listdir = os.listdir

            def listdir(path="."):
                if os.path.normcase(os.fspath(path)) == os.path.normcase(scope):
                    raise PermissionError(13, "Permission denied", scope)
                return real_listdir(path)

            with mock.patch.object(gate.os, "listdir", listdir):
                with self.assertRaisesRegex(gate.CannotDecide, "cannot read"):
                    gate.installed_packages(d)

    @unittest.skipIf(os.name == "nt", "dangling symlinks need symlink privilege on Windows; covered on Linux CI")
    def test_dangling_link_in_node_modules_cannot_be_decided(self):
        with tempfile.TemporaryDirectory() as d:
            tree(d, "node_modules/ui")
            os.symlink(os.path.join(d, "does-not-exist"), os.path.join(d, "node_modules", "ghost"))
            with self.assertRaisesRegex(gate.CannotDecide, "does not resolve"):
                gate.installed_packages(d)

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
        server_workflow = (REPO / ".github/workflows/validate-server.yml").read_text(encoding="utf-8")
        self.server_jobs = {k: commands(v) for k, v in jobs(server_workflow).items()}
        self.desktop_workflow = commands((REPO / ".github/workflows/desktop-release.yml").read_text(encoding="utf-8"))
        self.dockerfile = commands((REPO / "apps/client/Dockerfile").read_text(encoding="utf-8"))

    def test_server_jobs_install_only_the_server_workspaces(self):
        for job in ("checks", "audit"):
            self.assertEqual(installs(self.jobs[job]), [SERVER_INSTALL], job)
            self.assertNotIn("apps/mobile/tsconfig.json", self.jobs[job], job)
            self.assertNotIn("expo", self.jobs[job].lower(), job)

    def test_reusable_server_validation_matches_server_install_surface(self):
        self.assertEqual(set(self.server_jobs), {"checks", "audit"})
        for job in ("checks", "audit"):
            self.assertEqual(installs(self.server_jobs[job]), [SERVER_INSTALL], job)
        self.assertIn("ops/ci/js_advisory_gate.py --surface server --installed-root .", self.server_jobs["audit"])
        self.assertIn("./.github/workflows/validate-server.yml", self.desktop_workflow)
        self.assertIn("bun install --frozen-lockfile --filter '@stockspotter/client' --filter '@stockspotter/shared-types'", self.desktop_workflow)

    def test_mobile_gate_is_required_only_for_mobile_surface_changes(self):
        scope = self.jobs["mobile_scope"]
        self.assertIn("BASE_SHA", scope)
        self.assertIn("git diff --name-only", scope)
        self.assertIn("echo \"changed=true\"", scope)  # fail closed when the diff cannot be classified
        self.assertIn("apps/mobile/", scope)
        self.assertIn("ops/ci/js_advisory_gate", scope)
        self.assertIn("needs: mobile_scope", self.jobs["mobile"])
        self.assertIn("needs.mobile_scope.outputs.changed == 'true'", self.jobs["mobile"])

    def test_mobile_eas_build_is_gated_on_full_validation_and_exact_master_sha(self):
        workflow = commands((REPO / ".github/workflows/mobile-eas-build.yml").read_text(encoding="utf-8"))
        self.assertIn("workflows: [Validate]", workflow)
        self.assertIn("github.event.workflow_run.conclusion == 'success'", workflow)
        self.assertIn("github.event.workflow_run.head_branch == 'master'", workflow)
        self.assertIn("ref: ${{ github.event.workflow_run.head_sha }}", workflow)
        self.assertIn("secrets.EXPO_TOKEN", workflow)
        self.assertIn("--platform all --profile production", workflow)
        self.assertNotIn("submit", workflow.lower())

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
        self.assertEqual(set(self.jobs), {"mobile_scope", "checks", "audit", "mobile"})


if __name__ == "__main__":
    unittest.main()
