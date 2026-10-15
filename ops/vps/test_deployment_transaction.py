import copy
from contextlib import contextmanager
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("deployment_transaction", Path(__file__).with_name("deployment_transaction.py"))
assert SPEC and SPEC.loader
core = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(core)
BEFORE = "a" * 40
AFTER = "b" * 40
ENV = "e" * 64


def images(character):
    return {role: "sha256:" + character * 64 for role in core.ROLES}


class Killed(BaseException):
    pass


class FakeHost:
    def __init__(self):
        self.current = {"source": BEFORE, "checkout_source": BEFORE, "branch": "master", "images": images("1"), "env_hash": ENV}
        self.deployed = BEFORE
        self.remote = AFTER
        self.green = True
        self.ff = True
        self.safe = True
        self.zero = True
        self.locked = False
        self.running = {role: True for role in core.ROLES}
        self.calls = []
        self.left = set()               # commits the checkout has moved past
        self.fail_up = None
        self.break_health = False
        self.fail_restore = False
        self.fail_marker = False

    @contextmanager
    def lock(self):
        assert not self.locked
        self.locked = True
        try:
            yield
        finally:
            self.locked = False

    def tip(self): return self.remote
    def ancestor(self, previous, candidate): return self.ff
    def ci_passed(self, source): return self.green
    def snapshot(self): return copy.deepcopy(self.current)
    def env_digest(self): return self.current["env_hash"]
    def marker(self): return self.deployed
    def allowed(self): return self.safe
    def exposure_zero(self): return self.zero
    def revisions(self, value): return {role: AFTER for role in core.ROLES}

    def build(self, source):
        assert not self.locked
        self.calls.append("build")
        return images("2")

    def checkout(self, source, branch):
        assert self.locked
        # Forward only, like the adapter's fast-forward-only move. A fake that
        # accepted a backwards target here is what let the real rollback defect
        # through; the transaction must reach for `restore_checkout` instead.
        assert source not in self.left, f"checkout({source}) is behind the checkout"
        self.calls.append("checkout:" + source)
        self.left.add(self.current["checkout_source"])
        self.current.update(source=source, checkout_source=source, branch=branch)

    def restore_checkout(self, source, branch, expect_head):
        assert self.locked
        self.calls.append("restore:" + source)
        self._authorize_rewind(source, expect_head)
        self.left.discard(source)
        self.current.update(source=source, checkout_source=source, branch=branch)

    def _authorize_rewind(self, source, expect_head):
        """The adapter's contract, enforced by the fake that stands in for it."""
        if self.current["checkout_source"] == source:
            return                      # already restored; nothing moves
        if expect_head != self.current["checkout_source"]:
            raise core.Rejected(
                f"refusing to rewind to {source}: the checkout is at "
                f"{self.current['checkout_source']}, not {expect_head}")

    def up(self, value):
        assert self.locked
        self.calls.append("up")
        self.current["images"] = copy.deepcopy(value)
        self.running = {role: True for role in core.ROLES}
        if self.fail_restore and value == images("1"):
            raise OSError("fixture restore failure")
        if self.fail_up and value == images("2"):
            error, self.fail_up = self.fail_up, None
            raise error

    def health(self):
        if self.break_health and self.current["source"] == AFTER:
            raise core.Rejected("fixture candidate health failed")
        return {"images": copy.deepcopy(self.current["images"]), "running": dict(self.running), "ws_commit": self.current["source"]}

    def write_marker(self, source):
        assert self.locked
        self.calls.append("marker:" + source)
        if self.fail_marker:
            self.fail_marker = False
            raise OSError("fixture marker failure")
        self.deployed = source


class MixedHost(FakeHost):
    """Runtime c28-like image, old root checkout; these are independent."""
    def __init__(self):
        super().__init__()
        self.current["checkout_source"] = "c" * 40
        self.current["branch"] = "release/legacy"

    def checkout(self, source, branch):
        assert self.locked
        assert source not in self.left, f"checkout({source}) is behind the checkout"
        self.calls.append("checkout:" + source)
        self.left.add(self.current["checkout_source"])
        self.current.update(checkout_source=source, branch=branch)

    def restore_checkout(self, source, branch, expect_head):
        assert self.locked
        self.calls.append("restore:" + source)
        self._authorize_rewind(source, expect_head)
        self.left.discard(source)
        self.current.update(checkout_source=source, branch=branch)

    def up(self, value):
        super().up(value)
        self.current["source"] = AFTER if value == images("2") else BEFORE


class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.store = core.Store(Path(self.directory.name))
        self.host = FakeHost()
        self.tx = core.Transaction(self.store, self.host)

    def test_mixed_baseline_restores_actual_checkout_and_actual_runtime_separately(self):
        self.host = MixedHost()
        self.tx = core.Transaction(self.store, self.host)
        before = copy.deepcopy(self.host.current)
        candidate = self.tx.prepare()
        self.host.break_health = True
        with self.assertRaises(core.Rejected):
            self.tx.promote(candidate)
        self.assertEqual(self.host.current, before)
        self.assertIn("restore:" + "c" * 40, self.host.calls)
        self.assertEqual(self.host.marker(), BEFORE)
        self.assertIsNone(self.store.read("journal.json"))

    def test_checkout_or_image_drift_after_preparation_is_deferred(self):
        for field, value in [("checkout_source", "d" * 40), ("images", images("3"))]:
            with self.subTest(field=field):
                host = FakeHost()
                tx = core.Transaction(self.store, host)
                candidate = tx.prepare()
                host.current[field] = value
                with self.assertRaises(core.Deferred):
                    tx.promote(candidate)
                self.assertNotIn("up", host.calls)

    def test_schema_one_receipt_is_not_silently_adopted(self):
        self.store.write("committed.json", {"version": 1, "state": self.host.current})
        with self.assertRaises(core.Held):
            self.tx.prepare()
        self.assertEqual(self.host.calls, [])

    def test_prepare_builds_outside_lock_and_never_moves_source(self):
        candidate = self.tx.prepare()
        self.assertEqual(candidate["state"]["source"], AFTER)
        self.assertEqual(self.host.current["source"], BEFORE)
        self.assertEqual(self.host.calls, ["build"])

    def test_success_requires_full_proof_before_marker(self):
        self.assertEqual(self.tx.promote(self.tx.prepare()), AFTER)
        self.assertEqual(self.host.marker(), AFTER)
        self.assertIsNone(self.store.read("journal.json"))
        self.assertEqual(self.host.calls[-1], "marker:" + AFTER)

    def test_non_ff_and_unready_ci_never_build_or_checkout(self):
        self.host.ff = False
        with self.assertRaises(core.Rejected): self.tx.prepare()
        self.host.ff, self.host.green = True, False
        with self.assertRaises(core.Deferred): self.tx.prepare()
        self.assertEqual(self.host.calls, [])

    def test_new_tip_is_deferred_without_poisoning_candidate(self):
        candidate = self.tx.prepare()
        self.host.remote = "c" * 40
        with self.assertRaises(core.Deferred): self.tx.promote(candidate)
        self.assertIsNone(self.store.read("journal.json"))
        self.assertEqual(self.host.current["source"], BEFORE)

    def test_window_or_exposure_hold_has_no_live_mutation(self):
        candidate = self.tx.prepare()
        for attribute in ("safe", "zero"):
            setattr(self.host, attribute, False)
            with self.assertRaises(core.Deferred): self.tx.promote(candidate)
            setattr(self.host, attribute, True)
        self.assertEqual(self.host.calls, ["build"])

    def test_health_failure_restores_source_images_and_marker(self):
        candidate = self.tx.prepare()
        self.host.break_health = True
        with self.assertRaises(core.Rejected): self.tx.promote(candidate)
        self.assertEqual(self.host.current["source"], BEFORE)
        self.assertEqual(self.host.current["images"], images("1"))
        self.assertEqual(self.host.marker(), BEFORE)
        self.assertIsNone(self.store.read("journal.json"))

    def test_kill_leaves_intent_then_reconcile_restores(self):
        self.host.fail_up = Killed()
        with self.assertRaises(Killed): self.tx.promote(self.tx.prepare())
        self.assertEqual(self.store.read("journal.json")["phase"], "promoting")
        self.assertEqual(self.tx.reconcile(), "restored")
        self.assertEqual(self.host.current["images"], images("1"))

    def test_stopped_service_after_interruption_is_recreated(self):
        self.host.fail_up = Killed()
        with self.assertRaises(Killed): self.tx.promote(self.tx.prepare())
        self.host.running["ws"] = False
        self.assertEqual(self.tx.reconcile(), "restored")
        self.assertTrue(all(self.host.running.values()))

    def test_verified_interruption_finishes_only_after_fresh_proof(self):
        self.host.fail_marker = True
        with self.assertRaises(OSError): self.tx.promote(self.tx.prepare())
        self.assertEqual(self.store.read("journal.json")["phase"], "verified")
        self.assertEqual(self.tx.reconcile(), "committed")
        self.assertEqual(self.host.marker(), AFTER)

    def test_restore_failure_records_hold_and_never_loops(self):
        self.host.break_health, self.host.fail_restore = True, True
        with self.assertRaises(core.Held): self.tx.promote(self.tx.prepare())
        self.assertEqual(self.store.read("journal.json")["phase"], "held")
        previous = list(self.host.calls)
        with self.assertRaises(core.Held): self.tx.reconcile()
        self.assertEqual(self.host.calls, previous)

    def test_environment_change_after_kill_does_not_restore_or_overwrite_it(self):
        self.host.fail_up = Killed()
        with self.assertRaises(Killed): self.tx.promote(self.tx.prepare())
        self.host.current["env_hash"] = "f" * 64
        previous = list(self.host.calls)
        with self.assertRaises(core.Held): self.tx.reconcile()
        self.assertEqual(self.host.calls, previous)
        self.assertEqual(self.host.current["env_hash"], "f" * 64)

    def test_malformed_journal_refuses_all_host_mutation(self):
        self.store.write("journal.json", {"phase": "promoting"})
        with self.assertRaises(core.Held): self.tx.reconcile()
        self.assertEqual(self.host.calls, [])

    def test_missing_or_wrong_role_image_identity_is_rejected(self):
        for value in ({}, {**images("1"), "web": "latest"}, {**images("1"), "unexpected": "sha256:" + "1" * 64}):
            with self.assertRaises(core.Held): core.image_set(value)

    def test_late_proof_failure_is_held_after_candidate_is_live(self):
        candidate = self.tx.prepare()
        original = self.tx.finish
        def changed(record):
            self.host.break_health = True
            return original(record)
        self.tx.finish = changed
        with self.assertRaises(core.Held): self.tx.promote(candidate)
        self.assertEqual(self.store.read("journal.json")["phase"], "held")
        self.assertEqual(self.host.current["source"], AFTER)
        self.assertEqual(self.host.marker(), BEFORE)
        previous = list(self.host.calls)
        with self.assertRaises(core.Held): self.tx.reconcile()
        self.assertEqual(self.host.calls, previous)

    def test_committed_receipt_divergence_refuses_reconciliation(self):
        self.tx.promote(self.tx.prepare())
        self.host.current["images"]["web"] = "sha256:" + "3" * 64
        previous = list(self.host.calls)
        with self.assertRaises(core.Held): self.tx.reconcile()
        self.assertEqual(self.host.calls, previous)

    def test_malformed_committed_receipt_refuses_build(self):
        self.store.write("committed.json", {"version": True, "state": self.host.snapshot()})
        with self.assertRaises(core.Held): self.tx.prepare()
        self.assertEqual(self.host.calls, [])

    def test_failed_serialization_removes_private_temporary_file(self):
        with self.assertRaises(TypeError): self.store.write("invalid.json", {"invalid": object()})
        self.assertEqual(list(Path(self.directory.name).iterdir()), [])

    def test_rollback_uses_the_authorized_rewind_not_the_forward_move(self):
        """The promotion path may never be the thing that rewinds the checkout."""
        candidate = self.tx.prepare()
        self.host.break_health = True
        with self.assertRaises(core.Rejected):
            self.tx.promote(candidate)
        self.assertEqual(self.host.calls.count("checkout:" + AFTER), 1)
        self.assertIn("restore:" + BEFORE, self.host.calls)
        self.assertNotIn("checkout:" + BEFORE, self.host.calls)

    def test_rewind_is_named_with_the_journal_candidate_checkout_source(self):
        seen = []
        original = self.host.restore_checkout
        def record(source, branch, expect_head):
            seen.append((source, branch, expect_head))
            return original(source, branch, expect_head)
        self.host.restore_checkout = record
        candidate = self.tx.prepare()
        self.host.break_health = True
        with self.assertRaises(core.Rejected):
            self.tx.promote(candidate)
        self.assertEqual(seen, [(BEFORE, "master", AFTER)])

    def test_rewind_against_an_unexpected_checkout_is_held_not_forced(self):
        """A checkout that is not where the journal says holds for a human."""
        self.host.fail_up = Killed()
        with self.assertRaises(Killed):
            self.tx.promote(self.tx.prepare())
        self.host.current["checkout_source"] = "d" * 40
        with self.assertRaises(core.Held):
            self.tx.reconcile()
        self.assertEqual(self.store.read("journal.json")["phase"], "held")
        self.assertEqual(self.host.current["checkout_source"], "d" * 40)

    def test_already_restored_checkout_reconciles_without_a_rewind(self):
        """The crash-before-checkout case: nothing to move, and it must pass."""
        self.host.fail_up = Killed()
        with self.assertRaises(Killed):
            self.tx.promote(self.tx.prepare())
        self.host.current.update(source=BEFORE, checkout_source=BEFORE)
        self.assertEqual(self.tx.reconcile(), "restored")
        self.assertEqual(self.host.current["checkout_source"], BEFORE)
        self.assertIsNone(self.store.read("journal.json"))

    def test_marker_drift_between_prepare_and_promote_refuses_mutation(self):
        candidate = self.tx.prepare()
        self.host.deployed = "c" * 40
        previous = list(self.host.calls)
        with self.assertRaises(core.Held): self.tx.promote(candidate)
        self.assertEqual(self.host.calls, previous)
        self.assertIsNone(self.store.read("journal.json"))


if __name__ == "__main__":
    unittest.main()
