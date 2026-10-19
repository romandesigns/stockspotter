"""Actual Git checkout/rollback regressions; never Docker or production.

Every other test in this directory drives a fake `git`, and a fake `git` is
what hid the defect these tests exist for: `git merge --ff-only <ancestor>`
exits *zero* without moving HEAD, so the forward-only move could never bring
the checkout back once baseline and candidate shared a branch. Nothing here
fakes git. The repository is a throwaway `git init` under a temporary
directory, and only the git half of the host adapter is exercised.
"""
import contextlib
import hashlib
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest

import deployment_host as adapter


def images(source):
    """A deterministic, valid schema-2 image set for a commit."""
    return {role: "sha256:" + hashlib.sha256((role + source).encode()).hexdigest()
            for role in adapter.ROLES}


class RealGitFixture:
    """A throwaway repository: `baseline`, then `candidate`, both on master.

    Not a `TestCase` on purpose, so the suites below share the fixture without
    inheriting each other's tests.
    """

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="stockspotter-git-rollback-")
        self.addCleanup(self.directory.cleanup)
        self.owner = Path(self.directory.name).resolve()
        self.root = self.owner / "repo"
        self.root.mkdir()
        self.git("init", "-b", "master")
        self.git("config", "user.name", "Stockspotter fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        (self.root / ".gitignore").write_text(".env\ndata/\n")
        for name in adapter.FROZEN_PATHS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("frozen fixture\n")
        self.baseline = self.commit("baseline")
        self.git("branch", "release/fixture", self.baseline)
        self.candidate = self.commit("candidate")
        self.host = adapter.LinuxHost(root=self.root, control_dir=self.owner / "store",
                                      locker=lambda: contextlib.nullcontext())
        self.host.policy = SimpleNamespace(frozen={
            name: self.host.files.digest(self.root / name) for name in adapter.FROZEN_PATHS
        })

    # -- fixture helpers ---------------------------------------------------
    def git(self, *arguments):
        result = subprocess.run(["git", "-C", str(self.root), *arguments],
                                capture_output=True, text=True, timeout=15)
        if result.returncode:
            raise AssertionError(result.stderr)
        return result.stdout.strip()

    def commit(self, value, *, name="fixture.txt"):
        (self.root / name).parent.mkdir(parents=True, exist_ok=True)
        (self.root / name).write_text(value)
        self.git("add", "-A")
        self.git("commit", "-m", value)
        return self.git("rev-parse", "HEAD")

    def head(self):
        return self.git("rev-parse", "HEAD")

    def branch(self):
        return self.git("branch", "--show-current")

    def checkout(self, source, branch="master"):
        with self.host.lock():
            self.host.checkout(source, branch)

    def restore(self, source, branch="master", expect_head=None):
        with self.host.lock():
            self.host.restore_checkout(source, branch,
                                       self.candidate if expect_head is None else expect_head)


class RealGitCheckoutTests(RealGitFixture, unittest.TestCase):
    """The adapter's two checkout moves, against actual Git."""

    # -- the regression ----------------------------------------------------
    def test_second_master_promotion_can_be_rolled_back(self):
        """The defect: baseline and candidate both on master, rewind needed.

        `git switch master` is a no-op and `git merge --ff-only <baseline>`
        reports success without moving anything, so the forward move could
        never get here -- and the rejected candidate stayed live.
        """
        self.restore(self.baseline)
        self.assertEqual(self.head(), self.baseline)
        self.assertEqual(self.branch(), "master")
        self.assertEqual(self.git("rev-parse", "refs/heads/master"), self.baseline)

    def test_forward_move_alone_cannot_reach_an_earlier_commit(self):
        """Why `restore_checkout` has to exist at all, stated as a test."""
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.checkout(self.baseline)
        self.assertIn("does not fast-forward", str(caught.exception))
        self.assertEqual(self.head(), self.candidate)

    def test_merge_ff_only_to_an_ancestor_really_does_report_success(self):
        """The Git behaviour the whole correction rests on, pinned directly."""
        result = subprocess.run(["git", "-C", str(self.root), "merge", "--ff-only",
                                 self.baseline], capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0)
        self.assertIn("up to date", result.stdout.lower())
        self.assertEqual(self.head(), self.candidate)

    # -- the rollback that already worked, still works ---------------------
    def test_first_release_branch_restore_still_works(self):
        self.restore(self.baseline, "release/fixture")
        self.assertEqual(self.head(), self.baseline)
        self.assertEqual(self.branch(), "release/fixture")

    def test_already_restored_checkout_is_a_successful_no_op(self):
        """Reconciliation after a crash *before* the candidate was checked out.

        HEAD is the baseline already, so the candidate `expect_head` describes
        a move that never happened. That must not be an error, and nothing may
        be reset.
        """
        self.git("reset", "--hard", self.baseline)
        before = self.git("rev-parse", "master")
        self.restore(self.baseline)
        self.assertEqual(self.head(), self.baseline)
        self.assertEqual(self.git("rev-parse", "master"), before)
        self.assertEqual(self.git("rev-list", "--count", "HEAD"), "1")

    def test_restored_checkout_can_be_promoted_again(self):
        """A rollback must leave a checkout the next attempt can move forward."""
        self.restore(self.baseline)
        retry = self.git("commit-tree", self.git("rev-parse", self.candidate + "^{tree}"),
                         "-p", self.baseline, "-m", "retry")
        self.checkout(retry)
        self.assertEqual(self.head(), retry)
        self.assertEqual(self.branch(), "master")

    # -- refusals ----------------------------------------------------------
    def test_rewind_to_an_unexpected_head_is_refused(self):
        unexpected = self.commit("unrelated advancement")
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.restore(self.baseline)
        self.assertIn("not the candidate", str(caught.exception))
        self.assertEqual(self.head(), unexpected)

    def test_rewind_to_a_commit_that_is_not_an_ancestor_is_refused(self):
        """A sideways jump is not a rollback, even with the right HEAD."""
        self.git("checkout", "--quiet", "--orphan", "unrelated")
        sideways = self.commit("unrelated history", name="other.txt")
        self.git("checkout", "--quiet", "master")
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.restore(sideways)
        self.assertIn("not an ancestor", str(caught.exception))
        self.assertEqual(self.head(), self.candidate)

    def test_dirty_tracked_file_is_never_reset(self):
        (self.root / "fixture.txt").write_text("operator edit")
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.restore(self.baseline)
        self.assertIn("local changes", str(caught.exception))
        self.assertEqual((self.root / "fixture.txt").read_text(), "operator edit")
        self.assertEqual(self.head(), self.candidate)

    def test_untracked_file_blocks_the_rewind(self):
        (self.root / "operator-notes.txt").write_text("mid-incident notes")
        with self.assertRaises(adapter.core.Rejected):
            self.restore(self.baseline)
        self.assertTrue((self.root / "operator-notes.txt").exists())
        self.assertEqual(self.head(), self.candidate)

    def test_rewind_without_the_deployment_lock_is_refused(self):
        with self.assertRaises(adapter.core.Held):
            self.host.restore_checkout(self.baseline, "master", self.candidate)
        self.assertEqual(self.head(), self.candidate)

    def test_missing_branch_is_refused(self):
        with self.assertRaises(adapter.core.Rejected):
            self.restore(self.baseline, "release/absent")
        self.assertEqual(self.head(), self.candidate)

    def test_frozen_content_drift_in_the_target_is_refused(self):
        """A target commit that would rewrite the preregistration cannot land."""
        frozen = "OI-V2-PREREGISTRATION-2026-09-19.md"
        (self.root / frozen).write_text("rewritten preregistration\n")
        rewritten = self.commit("rewrites frozen content")
        self.host.policy = SimpleNamespace(frozen={
            name: self.host.files.digest(self.root / name) for name in adapter.FROZEN_PATHS
        })
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.restore(self.baseline, expect_head=rewritten)
        self.assertIn("frozen content", str(caught.exception))
        self.assertEqual(self.head(), rewritten)

    # -- ignored working files: .env and the live data mount ---------------
    def test_an_ignored_parent_file_is_not_deleted_to_make_a_target_directory(self):
        # A leaf-only collision check misses this: data/new.txt does not yet
        # exist because the ignored parent `data` is a regular capture file.
        (self.root / ".gitignore").write_text(".env\ndata\n")
        target_path = self.root / "data" / "new.txt"
        target_path.parent.mkdir()
        target_path.write_text("tracked fixture")
        self.git("add", "-f", "data/new.txt")
        target = self.commit("baseline with a tracked descendant")
        self.git("rm", "data/new.txt")
        self.git("commit", "-m", "candidate removes tracked descendant")
        self.candidate = self.head()
        parent = self.root / "data"
        if parent.exists(): parent.rmdir()
        parent.write_text("ignored recorder data")
        self.assertTrue(self.host._clean(), "the ignored collision must evade ordinary git status")
        with self.assertRaises(adapter.core.Rejected):
            self.restore(target)
        self.assertEqual(parent.read_text(), "ignored recorder data")
        self.assertEqual(self.head(), self.candidate)

    def test_ignored_env_is_not_overwritten_by_a_rewind(self):
        """`.env` is ignored, and git does not protect ignored files.

        A baseline that tracks `.env` plus a live ignored `.env` is a silent
        credential overwrite under `reset --hard`. It has to be a refusal.
        """
        self.git("checkout", "--quiet", "master")
        (self.root / ".env").write_text("STOCKSPOTTER_API_TOKEN=live-secret\n")
        tracked_env = self.owner / "tracked-env"
        tracked_env.write_text("STOCKSPOTTER_API_TOKEN=from-the-commit\n")
        blob = subprocess.run(["git", "-C", str(self.root), "hash-object", "-w",
                               str(tracked_env)], capture_output=True, text=True,
                              timeout=15, check=True).stdout.strip()
        self.git("update-index", "--add", "--cacheinfo", f"100644,{blob},.env")
        target = self.git("commit-tree", self.git("write-tree"), "-p", self.baseline,
                          "-m", "tracks .env")
        later = self.git("commit-tree", self.git("rev-parse", self.candidate + "^{tree}"),
                         "-p", target, "-m", "after")
        self.git("reset", "--hard", later)
        (self.root / ".env").write_text("STOCKSPOTTER_API_TOKEN=live-secret\n")
        self.host.policy = None
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.restore(target, expect_head=later)
        self.assertIn("refusing to destroy working paths", str(caught.exception))
        self.assertIn(".env", str(caught.exception))
        self.assertEqual((self.root / ".env").read_text(),
                         "STOCKSPOTTER_API_TOKEN=live-secret\n")
        self.assertEqual(self.head(), later)

    def test_ignored_data_directory_survives_a_rewind_that_does_not_touch_it(self):
        """The live `data/` mount is ignored; a clean rewind must leave it be."""
        (self.root / "data").mkdir()
        (self.root / "data" / "journal.ndjson").write_text("observation\n")
        self.restore(self.baseline)
        self.assertEqual(self.head(), self.baseline)
        self.assertEqual((self.root / "data" / "journal.ndjson").read_text(),
                         "observation\n")

    def introduce_descendant(self, path, *, ignore="data"):
        """A target tracking `path` and a candidate that does not.

        Returns the target commit. The leaf never exists on disk, which is what
        makes this family of collisions invisible to a leaf-only check.
        """
        (self.root / ".gitignore").write_text(f".env\n{ignore}\n")
        leaf = self.root / path
        leaf.parent.mkdir(parents=True, exist_ok=True)
        leaf.write_text("tracked fixture")
        self.git("add", "-f", path)
        target = self.commit("target tracking " + path)
        self.git("rm", path)
        self.git("commit", "-m", "candidate drops " + path)
        self.candidate = self.head()
        for parent in leaf.parents:
            if parent == self.root:
                break
            if not parent.is_relative_to(self.root):
                raise AssertionError('fixture cleanup escaped its repository')
            if parent.is_dir() and not any(parent.iterdir()):
                parent.rmdir()
        return target

    def test_an_ignored_grandparent_file_is_not_deleted_for_a_deep_target(self):
        """The same hazard two levels up, where only the top name collides."""
        target = self.introduce_descendant("data/nested/new.txt")
        parent = self.root / "data"
        parent.write_text("ignored recorder data")
        self.assertTrue(self.host._clean())
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.restore(target)
        self.assertIn("would be replaced by a directory", str(caught.exception))
        self.assertEqual(parent.read_text(), "ignored recorder data")
        self.assertEqual(self.head(), self.candidate)

    @unittest.skipUnless(hasattr(__import__("os"), "symlink"), "needs symlink support")
    def test_a_directory_symlink_ancestor_is_refused_rather_than_written_through(self):
        """Writing through a symlinked ancestor leaves the checkout entirely."""
        target = self.introduce_descendant("data/new.txt")
        outside = self.owner / "elsewhere"
        outside.mkdir()
        try:
            (self.root / "data").symlink_to(outside, target_is_directory=True)
        except OSError as error:                        # unprivileged Windows
            self.skipTest(f"cannot create a directory symlink here: {error}")
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.restore(target)
        self.assertIn("data", str(caught.exception))
        self.assertTrue((self.root / "data").is_symlink())
        self.assertEqual(list(outside.iterdir()), [])
        self.assertEqual(self.head(), self.candidate)

    @unittest.skipUnless(hasattr(__import__("os"), "symlink"), "needs symlink support")
    def test_a_dangling_symlink_ancestor_is_refused(self):
        """`exists()` says False for a broken link; `lexists` is the question."""
        target = self.introduce_descendant("data/new.txt")
        try:
            (self.root / "data").symlink_to(self.owner / "absent")
        except OSError as error:                        # unprivileged Windows
            self.skipTest(f"cannot create a symlink here: {error}")
        self.assertFalse((self.root / "data").exists())
        with self.assertRaises(adapter.core.Rejected):
            self.restore(target)
        self.assertTrue((self.root / "data").is_symlink())
        self.assertEqual(self.head(), self.candidate)

    def test_an_ordinary_existing_directory_does_not_block_the_rewind(self):
        """An introduced path inside a real ignored directory is just created."""
        target = self.introduce_descendant("data/new.txt")
        (self.root / "data").mkdir()
        (self.root / "data" / "journal.ndjson").write_text("observation\n")
        self.restore(target)
        self.assertEqual(self.head(), target)
        self.assertEqual((self.root / "data" / "new.txt").read_text(), "tracked fixture")
        self.assertEqual((self.root / "data" / "journal.ndjson").read_text(),
                         "observation\n")

    def test_a_tracked_file_may_still_become_a_directory(self):
        """Committed content, not operator data: git is entitled to replace it."""
        (self.root / "config").write_text("a tracked file today\n")
        self.git("add", "config")
        self.candidate = self.commit("candidate tracks config as a file")
        (self.root / "config").unlink()
        (self.root / "config").mkdir()
        (self.root / "config" / "nested.txt").write_text("tracked fixture")
        self.git("add", "-A")
        target = self.commit("target tracks config as a directory")
        self.git("reset", "--hard", self.candidate)
        with self.host.lock():
            self.host._reject_unsafe_collisions(target)      # must not raise
        self.assertIn("config/nested.txt", self.host._paths_in(target))
        self.assertIn("config", self.host._paths_tracked())

    # -- the same guard on the forward promotion path ----------------------
    def test_a_promotion_that_would_destroy_the_ignored_env_is_refused(self):
        """Explicitly authorized: a promotion is as destructive as a rewind."""
        (self.root / ".env").write_text("STOCKSPOTTER_API_TOKEN=live-secret\n")
        tracked_env = self.owner / "tracked-env"
        tracked_env.write_text("STOCKSPOTTER_API_TOKEN=from-the-commit\n")
        blob = subprocess.run(["git", "-C", str(self.root), "hash-object", "-w",
                               str(tracked_env)], capture_output=True, text=True,
                              timeout=15, check=True).stdout.strip()
        self.git("update-index", "--add", "--cacheinfo", f"100644,{blob},.env")
        candidate = self.git("commit-tree", self.git("write-tree"), "-p", self.candidate,
                             "-m", "a candidate that tracks .env")
        self.git("reset", "--hard", self.candidate)
        (self.root / ".env").write_text("STOCKSPOTTER_API_TOKEN=live-secret\n")
        self.host.policy = None
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.checkout(candidate)
        self.assertIn(".env", str(caught.exception))
        self.assertEqual((self.root / ".env").read_text(),
                         "STOCKSPOTTER_API_TOKEN=live-secret\n")
        self.assertEqual(self.head(), self.candidate)

    def test_a_promotion_that_would_destroy_the_ignored_data_capture_is_refused(self):
        """The ancestor form of the same refusal, on the forward path."""
        (self.root / ".gitignore").write_text(".env\ndata\n")
        self.git("add", "-A")
        self.candidate = self.commit("candidate ignoring data")
        leaf = self.root / "data" / "new.txt"
        leaf.parent.mkdir()
        leaf.write_text("tracked fixture")
        self.git("add", "-f", "data/new.txt")
        candidate = self.commit("a candidate tracking data/new.txt")
        self.git("reset", "--hard", self.candidate)
        if (self.root / "data").is_dir():
            for child in (self.root / "data").iterdir():
                child.unlink()
            (self.root / "data").rmdir()
        (self.root / "data").write_text("ignored recorder data")
        with self.assertRaises(adapter.core.Rejected) as caught:
            self.checkout(candidate)
        self.assertIn("would be replaced by a directory", str(caught.exception))
        self.assertEqual((self.root / "data").read_text(), "ignored recorder data")
        self.assertEqual(self.head(), self.candidate)

    def test_an_ordinary_promotion_is_unaffected_by_the_guard(self):
        """The guard must not start refusing the normal forward case."""
        advance = self.commit("a further candidate")
        self.git("reset", "--hard", self.candidate)
        self.checkout(advance)
        self.assertEqual(self.head(), advance)
        self.assertEqual(self.branch(), "master")

    # -- evidence ----------------------------------------------------------
    def test_rejected_candidate_stays_reachable_for_the_post_mortem(self):
        self.restore(self.baseline)
        reference = f"refs/stockspotter/rollback/{self.candidate}"
        self.assertEqual(self.git("rev-parse", reference), self.candidate)
        self.assertEqual(self.host.evidence["candidate_retention"], reference)

    def test_retention_failure_does_not_block_the_rollback(self):
        """Evidence is not allowed to gate an emergency rewind."""
        original = self.host._git
        def refuse(*args, **kwargs):
            if args[:1] == ("update-ref",):
                return adapter.Result(args, 128, "", "fatal: fixture refused")
            return original(*args, **kwargs)
        self.host._git = refuse
        self.restore(self.baseline)
        self.assertEqual(self.head(), self.baseline)
        self.assertIn("not retained", self.host.evidence["candidate_retention"])


class GitOnlyHost(adapter.LinuxHost):
    """Real git; stubbed Docker, broker and marker.

    Enough of the `Host` protocol to drive `Transaction.restore` end to end
    against an actual repository, and no more: this exists to prove the seam
    where the defect bit -- the transaction asking the adapter to move the
    checkout back -- not to re-test recreation or the runtime proof.
    """

    def __post_init__(self):
        super().__post_init__()
        self.runtime = {}
        self.marker_value = None
        self.exposure = True

    def exposure_zero(self): return self.exposure
    def env_digest(self): return "a" * 64
    def marker(self): return self.marker_value
    def write_marker(self, source): self.marker_value = source

    def up(self, value):
        self.runtime = adapter.image_set(value)

    def snapshot(self):
        head = self._head()
        return {"source": head, "checkout_source": head, "branch": self._branch(),
                "images": dict(self.runtime), "env_hash": "a" * 64}

    def health(self):
        return {"images": dict(self.runtime), "ws_commit": self._head(),
                "running": {role: True for role in adapter.ROLES}}


class RealGitRollbackTransactionTests(RealGitFixture, unittest.TestCase):
    """`Transaction.restore` over real Git: the defect's actual blast radius."""

    def setUp(self):
        super().setUp()
        self.host = GitOnlyHost(root=self.root, control_dir=self.owner / "store",
                                locker=lambda: contextlib.nullcontext())
        self.host.runtime = images(self.candidate)
        self.host.marker_value = self.candidate
        self.store = adapter.core.Store(self.owner / "store")
        self.tx = adapter.core.Transaction(self.store, self.host)

    def record(self, phase="promoting"):
        def deployed(source):
            return {"source": source, "checkout_source": source, "branch": "master",
                    "images": images(source), "env_hash": "a" * 64}
        return {"version": 2, "tx": "b" * 32, "phase": phase,
                "before": deployed(self.baseline), "candidate": deployed(self.candidate)}

    def test_interrupted_second_promotion_reconciles_back_to_the_baseline(self):
        self.store.write("journal.json", self.record())
        self.assertEqual(self.tx.reconcile(), "restored")
        self.assertEqual(self.head(), self.baseline)
        self.assertEqual(self.branch(), "master")
        self.assertEqual(self.host.runtime, images(self.baseline))
        self.assertEqual(self.host.marker_value, self.baseline)
        self.assertIsNone(self.store.read("journal.json"))
        self.assertEqual(self.store.read("committed.json")["state"]["checkout_source"],
                         self.baseline)

    def test_the_same_reconciliation_is_idempotent(self):
        """Running it twice must not need a second rewind to succeed."""
        self.store.write("journal.json", self.record())
        self.assertEqual(self.tx.reconcile(), "restored")
        self.store.write("journal.json", self.record())
        self.assertEqual(self.tx.reconcile(), "restored")
        self.assertEqual(self.head(), self.baseline)

    def test_a_checkout_the_journal_cannot_account_for_is_held(self):
        self.store.write("journal.json", self.record())
        drifted = self.commit("out-of-band advancement")
        self.host.runtime = images(drifted)
        with self.assertRaises(adapter.core.Held):
            self.tx.reconcile()
        self.assertEqual(self.head(), drifted)
        self.assertEqual(self.store.read("journal.json")["phase"], "held")

    def test_exposure_blocks_the_rewind_before_git_is_touched(self):
        self.store.write("journal.json", self.record())
        self.host.exposure = False
        with self.assertRaises(adapter.core.Held):
            self.tx.reconcile()
        self.assertEqual(self.head(), self.candidate)


if __name__ == "__main__":
    unittest.main()
