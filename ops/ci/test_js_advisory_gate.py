"""Tests for the surface-scoped JavaScript advisory gate.

Run from the repository root: python -m unittest discover -s ops/ci -p 'test_*.py'
Kept beside the script, outside python/tests, on purpose: python/tests is run
inside the python Docker image, which does not (and should not) contain CI
tooling.
"""
import copy
import pathlib
import unittest

import js_advisory_gate as gate

HIGH = [{"severity": "high", "url": "https://example.invalid/GHSA-test", "title": "t"}]


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
        # `shared` is advised; the server reaches a copy of it, so it is blocked
        # whatever the version (over-blocks, never under-blocks).
        blocked, _ = gate.decide(lock(), {"shared": HIGH}, "mobile")
        self.assertEqual(list(blocked), ["shared"])

    def test_moderate_advisories_do_not_block(self):
        blocked, elsewhere = gate.decide(lock(), {"shared": [{"severity": "moderate"}]}, "server")
        self.assertEqual((blocked, elsewhere), ({}, {}))

    def test_critical_blocks(self):
        blocked, _ = gate.decide(lock(), {"ui": [{"severity": "critical"}]}, "server")
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

    def test_main_exit_codes(self):
        import json, os, tempfile
        with tempfile.TemporaryDirectory() as d:
            lp, ap = os.path.join(d, "bun.lock"), os.path.join(d, "audit.json")
            # A trailing comma, as bun writes them.
            with open(lp, "w") as f:
                f.write(json.dumps(lock())[:-1] + ",}")
            with open(ap, "w") as f:
                f.write(json.dumps({"risky": HIGH}))
            self.assertEqual(gate.main(["--surface", "server", "--lock", lp, "--audit-json", ap]), 0)
            self.assertEqual(gate.main(["--surface", "mobile", "--lock", lp, "--audit-json", ap]), 1)
            self.assertEqual(gate.main(["--surface", "server", "--lock", os.path.join(d, "absent"), "--audit-json", ap]), 1)

    def test_real_lockfile_classifies_every_workspace_and_resolves(self):
        real = pathlib.Path(__file__).resolve().parents[2] / "bun.lock"
        l = gate.load_lock(real)
        blocked, _ = gate.decide(copy.deepcopy(l), {}, "server")
        self.assertEqual(blocked, {})
        server = set().union(*(gate.names(l, gate.closure(l, w)) for w in gate.SURFACES["server"]))
        mobile = gate.names(l, gate.closure(l, "apps/mobile"))
        # The mobile toolchain is where Metro and Expo live; the server surface has neither.
        self.assertIn("expo", mobile)
        self.assertNotIn("expo", server)
        self.assertNotIn("react-native", server)
        self.assertIn("react", server)


if __name__ == "__main__":
    unittest.main()
