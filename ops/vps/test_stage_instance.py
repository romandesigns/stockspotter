"""Tests for the stage instance driver. Real git, never Docker, never production.

The driver's job is to be impossible to point at something that matters, so most
of these are refusals. The one end-to-end case builds an instance from a
throwaway repository under a temporary directory and checks the properties the
rehearsal depends on: an independent clone, a `.env` the controller can adopt
from, a data directory where production keeps its own, and a teardown that will
not remove anything it cannot show it created.
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


def _load(name):
    path = Path(__file__).with_name(name + ".py")
    loaded = sys.modules.get(name)
    if loaded is not None and getattr(loaded, "__file__", None) == str(path):
        return loaded
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault(name, module)
    spec.loader.exec_module(module)
    return module


core = _load("deployment_transaction")
adapter = _load("deployment_host")
staging = _load("staging_rehearsal")
driver = _load("stage_instance")
fixture = _load("test_staging_rehearsal")

OPENSSL = shutil.which("openssl")
if not OPENSSL and os.name == "nt":
    candidate = Path(r"C:\Program Files\Git\usr\bin\openssl.exe")
    if candidate.is_file():
        OPENSSL = str(candidate)
GIT = shutil.which("git")
PROJECT = "stockspotter-stage-driverproof"
REMOTE = "https://github.example.invalid/stockspotter.git"


class TargetValidationTests(unittest.TestCase):
    """Refusals that need no filesystem and no git."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name).resolve()
        self.files = adapter.Files()

    def validate(self, owner, project=PROJECT):
        return driver.validate_target(owner, project, files=self.files)

    def test_the_production_project_is_refused(self):
        with self.assertRaises(ValueError):
            self.validate(self.base / adapter.PROJECT, adapter.PROJECT)

    def test_an_unvalidated_namespace_is_refused(self):
        for name in ("stockspotter-staging", "stockspotter-stage-", "staging", ""):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.validate(self.base / "whatever", name)

    def test_an_owner_root_not_named_for_the_project_is_refused(self):
        with self.assertRaises(ValueError) as caught:
            self.validate(self.base / "some-other-directory")
        self.assertIn("must be named for the project", str(caught.exception))

    def test_the_production_checkout_is_refused(self):
        with self.assertRaises(ValueError) as caught:
            self.validate(adapter.PRODUCTION_ROOT / PROJECT)
        self.assertIn("overlaps", str(caught.exception))

    def test_a_parent_of_the_production_checkout_is_refused(self):
        """Removing a directory that *contains* production must be unreachable."""
        with self.assertRaises(ValueError):
            driver._refuse_production_overlap(adapter.PRODUCTION_ROOT.parent.resolve())

    def test_the_production_control_store_is_refused(self):
        with self.assertRaises(ValueError):
            self.validate(adapter.DEFAULT_STORE / PROJECT)

    @unittest.skipUnless(hasattr(os, "symlink"), "needs symlink support")
    def test_a_symlinked_owner_root_is_refused_before_it_resolves(self):
        real = self.base / "real"
        real.mkdir()
        link = self.base / PROJECT
        try:
            link.symlink_to(real, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"cannot create a directory symlink here: {error}")
        with self.assertRaises(ValueError) as caught:
            self.validate(link)
        self.assertIn("symlink", str(caught.exception))


class TeardownGuardTests(unittest.TestCase):
    """Teardown removes what the manifest records, and refuses the rest."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name).resolve()
        self.owner = self.base / PROJECT
        self.owner.mkdir(mode=0o700)

    def manifest(self, **changes):
        record = {"version": 1, "project": PROJECT, "owner_root": str(self.owner),
                  "created": ["."], "compose": [f"docker compose -p {PROJECT} down"]}
        record.update(changes)
        staging.write_private(self.owner / driver.MANIFEST_NAME,
                              json.dumps(record, indent=2))
        return record

    def teardown_now(self, **changes):
        options = {"owner": self.owner, "project": PROJECT, "confirm": True}
        options.update(changes)
        return driver.teardown(**options)

    def test_teardown_requires_confirmation(self):
        self.manifest()
        with self.assertRaises(ValueError):
            self.teardown_now(confirm=False)
        self.assertTrue(self.owner.is_dir())

    def test_a_directory_without_a_manifest_is_never_removed(self):
        (self.owner / "something-precious.json").write_text("not ours")
        with self.assertRaises(ValueError) as caught:
            self.teardown_now()
        self.assertIn("refusing to remove", str(caught.exception))
        self.assertTrue((self.owner / "something-precious.json").is_file())

    def test_a_manifest_for_another_project_is_refused(self):
        self.manifest(project="stockspotter-stage-somethingelse")
        with self.assertRaises(ValueError):
            self.teardown_now()
        self.assertTrue(self.owner.is_dir())

    def test_a_manifest_recording_another_owner_root_is_refused(self):
        self.manifest(owner_root=str(self.base / "elsewhere"))
        with self.assertRaises(ValueError) as caught:
            self.teardown_now()
        self.assertIn("records owner root", str(caught.exception))
        self.assertTrue(self.owner.is_dir())

    def test_an_unrecognised_manifest_version_is_refused(self):
        self.manifest(version=2)
        with self.assertRaises(ValueError):
            self.teardown_now()
        self.assertTrue(self.owner.is_dir())

    def test_a_recorded_instance_is_removed_and_reports_the_down_command(self):
        self.manifest()
        (self.owner / "repo").mkdir()
        report = self.teardown_now()
        self.assertFalse(self.owner.exists())
        self.assertEqual(report["removed"], str(self.owner))
        self.assertTrue(any("down" in line
                            for line in report["run_first_if_the_stack_is_up"]))
        self.assertEqual(report["docker"], "never run by this module")


@unittest.skipUnless(GIT, "git is required")
@unittest.skipUnless(OPENSSL, "OpenSSL CLI is required for the fixture certificate")
@unittest.skipUnless(
    os.name == "posix",
    "compose bind strings are `src:dst:mode`, so a Windows drive letter makes "
    "_bind_parts read `C` as the source; the driver targets the Linux VPS")
class PrepareTests(unittest.TestCase):
    """One real instance, from a throwaway repository.

    POSIX only, and that is a property of the artefact rather than of the test:
    `staging_tls.derive` emits colon-separated bind mounts and refuses any
    source it cannot show is inside the owner root, which a `C:\\...` path never
    is. The refusals in the two classes above are platform-independent and run
    everywhere.
    """

    @classmethod
    def git(cls, *arguments, cwd):
        result = subprocess.run([GIT, *arguments], cwd=str(cwd), capture_output=True,
                                text=True, timeout=60)
        if result.returncode:
            raise AssertionError(result.stderr)
        return result.stdout.strip()

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name).resolve()
        self.owner = self.base / PROJECT
        self.source = self.base / "source"
        self.source.mkdir()
        self.git("init", "-b", "master", cwd=self.source)
        self.git("config", "user.name", "Stage fixture", cwd=self.source)
        self.git("config", "user.email", "fixture@example.invalid", cwd=self.source)
        (self.source / ".gitignore").write_text("/data/\n.env\n")
        (self.source / "fixture.txt").write_text("baseline")
        self.git("add", "-A", cwd=self.source)
        self.git("commit", "-m", "baseline", cwd=self.source)
        self.baseline = self.git("rev-parse", "HEAD", cwd=self.source)
        (self.source / "fixture.txt").write_text("candidate")
        self.git("add", "-A", cwd=self.source)
        self.git("commit", "-m", "candidate", cwd=self.source)
        self.candidate = self.git("rev-parse", "HEAD", cwd=self.source)
        self.git("remote", "add", "origin", REMOTE, cwd=self.source)
        self.recipe = self.base / "baseline-recipe.json"
        self.recipe.write_text(json.dumps(fixture.recipe()), encoding="utf-8")

    def prepare(self, **changes):
        options = {"owner": self.owner, "project": PROJECT, "recipe_path": self.recipe,
                   "source": self.source, "openssl": OPENSSL}
        options.update(changes)
        return driver.prepare(**options)

    def test_an_instance_is_assembled_with_an_independent_clone(self):
        manifest = self.prepare()
        repo = self.owner / driver.REPO_DIRECTORY
        self.assertTrue(manifest["clone"]["no_hardlinks"])
        self.assertEqual(manifest["clone"]["origin"], REMOTE)
        self.assertEqual(self.git("remote", "get-url", "origin", cwd=repo), REMOTE)
        # No alternates file: the clone does not borrow the source's objects.
        self.assertFalse((repo / ".git" / "objects" / "info" / "alternates").exists())
        self.assertEqual(manifest["clone"]["head"], self.candidate)

    def test_a_clone_of_a_release_head_still_has_a_local_master(self):
        """`promote()` hardcodes `checkout(candidate, "master")`.

        Production sits on a release branch, so `git clone` gives the clone
        only that branch -- and `Host.checkout` refuses to create one. The
        branch is taken from the clone's own remote-tracking ref, so it is the
        source's genuine master and no published tip is assumed.
        """
        self.git("switch", "-c", "release/baseline", self.baseline, cwd=self.source)
        manifest = self.prepare(checkout=self.baseline, branch="release/baseline")
        repo = self.owner / driver.REPO_DIRECTORY
        self.assertEqual(self.git("rev-parse", "refs/heads/master", cwd=repo),
                         self.candidate)
        self.assertEqual(self.git("branch", "--show-current", cwd=repo),
                         "release/baseline")
        self.assertEqual(manifest["clone"]["head"], self.baseline)
        retained = manifest["clone"]["retained_branches"]
        self.assertEqual([entry["branch"] for entry in retained], ["master"])
        self.assertEqual(retained[0]["at"], self.candidate)
        self.assertIsNone(manifest["clone"]["synthetic_branch"])

    def test_a_clone_that_already_has_master_retains_nothing(self):
        manifest = self.prepare()
        self.assertEqual(manifest["clone"]["retained_branches"], [])

    def test_a_source_without_master_is_refused_rather_than_invented(self):
        self.git("switch", "-c", "release/only", cwd=self.source)
        self.git("branch", "-D", "master", cwd=self.source)
        with self.assertRaises(ValueError) as caught:
            self.prepare()
        self.assertIn("no master to retain", str(caught.exception))
        self.assertFalse(self.owner.exists())

    def test_the_data_directory_sits_where_production_keeps_its_own(self):
        manifest = self.prepare()
        data = Path(manifest["paths"]["data"])
        self.assertTrue(data.is_dir())
        self.assertTrue(data.is_relative_to(self.owner / driver.REPO_DIRECTORY))
        self.assertEqual(list(data.iterdir()), [])
        document = json.loads(Path(manifest["paths"]["document"]).read_text())
        for entry in document["services"]["ws"]["volumes"]:
            self.assertTrue(Path(entry.split(":")[0]).is_relative_to(self.owner))

    def test_the_generated_dotenv_matches_the_generated_document(self):
        manifest = self.prepare()
        document = json.loads(Path(manifest["paths"]["document"]).read_text())
        pairs = adapter.Files().env_pairs(Path(manifest["paths"]["dotenv"]))
        self.assertTrue(pairs)
        for role in core.ROLES:
            observed = adapter._env_map(document["services"][role]["environment"])
            for name, digest in pairs.items():
                with self.subTest(role=role, name=name):
                    self.assertEqual(digest, adapter._digest_text(observed[name]))

    def test_the_broker_script_is_copied_inside_the_owner_root(self):
        manifest = self.prepare()
        document = json.loads(Path(manifest["paths"]["document"]).read_text())
        sources = [entry.split(":")[0]
                   for entry in document["services"][staging.BROKER_SERVICE]["volumes"]]
        self.assertTrue(sources)
        for source in sources:
            self.assertTrue(Path(source).is_relative_to(self.owner), source)
        self.assertTrue((self.owner / driver.BROKER_COPY).is_file())

    def test_the_broker_answers_only_for_the_paper_endpoint(self):
        manifest = self.prepare()
        document = json.loads(Path(manifest["paths"]["document"]).read_text())
        broker = document["services"][staging.BROKER_SERVICE]
        self.assertEqual(broker["networks"]["default"]["aliases"],
                         ["paper-api.alpaca.markets"])
        for role in core.ROLES:
            self.assertNotIn("networks", document["services"][role])

    def test_docker_is_only_ever_printed_never_executed(self):
        output = __import__("io").StringIO()
        code = driver.main(["prepare", "--owner-root", str(self.owner),
                            "--project", PROJECT, "--recipe", str(self.recipe),
                            "--source", str(self.source), "--openssl", OPENSSL], out=output)
        self.assertEqual(code, 0)
        manifest = json.loads(output.getvalue())
        self.assertEqual(manifest["docker"], "never run by this module")
        self.assertTrue(any(line.startswith("docker compose ") and " up " in line
                            for line in manifest["compose"]))
        # Structural, not a grep for the word: the module starts exactly one
        # kind of process, and it is git.
        source = Path(driver.__file__).read_text(encoding="utf-8")
        self.assertEqual(source.count("subprocess.run"), 1)
        self.assertEqual(source.count('subprocess.run(["git", *argv]'), 1)

    def test_the_real_clock_is_used_and_no_window_is_synthesized(self):
        manifest = self.prepare()
        self.assertIn("real clock", manifest["clock"])
        source = Path(driver.__file__).read_text(encoding="utf-8")
        self.assertNotIn("clock=", source)

    def test_a_synthetic_prior_branch_position_must_be_asked_for(self):
        with self.assertRaises(ValueError) as caught:
            self.prepare(checkout=self.baseline, branch="master")
        self.assertIn("--force-branch", str(caught.exception))
        self.assertFalse(self.owner.exists(), "a refused instance must leave nothing")

    def test_a_forced_branch_is_recorded_as_synthetic(self):
        manifest = self.prepare(checkout=self.baseline, branch="master",
                                force_branch=True)
        repo = self.owner / driver.REPO_DIRECTORY
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=repo), self.baseline)
        self.assertEqual(self.git("rev-parse", "refs/heads/master", cwd=repo),
                         self.baseline)
        self.assertEqual(manifest["clone"]["synthetic_branch"]["moved_from"],
                         self.candidate)
        self.assertIn("no fabricated candidate",
                      manifest["clone"]["synthetic_branch"]["why"])

    def test_a_checkout_at_the_branch_tip_needs_no_force(self):
        manifest = self.prepare(checkout=self.candidate, branch="master")
        self.assertIsNone(manifest["clone"]["synthetic_branch"])
        self.assertEqual(manifest["clone"]["head"], self.candidate)

    def test_a_checkout_without_a_branch_is_refused(self):
        with self.assertRaises(ValueError) as caught:
            self.prepare(checkout=self.baseline)
        self.assertIn("--checkout needs --branch", str(caught.exception))

    def test_preparing_over_an_existing_instance_is_refused(self):
        self.prepare()
        with self.assertRaises(ValueError) as caught:
            self.prepare()
        self.assertIn("already exists", str(caught.exception))
        self.assertTrue((self.owner / driver.MANIFEST_NAME).is_file())

    def test_a_commit_that_does_not_ignore_data_or_env_is_refused(self):
        """Otherwise every later controller call blames the stage checkout."""
        (self.source / ".gitignore").write_text("# nothing ignored\n")
        self.git("add", "-A", cwd=self.source)
        self.git("commit", "-m", "stop ignoring data and .env", cwd=self.source)
        with self.assertRaises(ValueError) as caught:
            self.prepare()
        self.assertIn("dirty once .env and data/ exist", str(caught.exception))
        self.assertFalse(self.owner.exists())

    def test_a_source_that_is_not_a_checkout_is_refused(self):
        with self.assertRaises(ValueError) as caught:
            self.prepare(source=self.base)
        self.assertIn("not a git checkout", str(caught.exception))

    def test_an_instance_the_adapter_accepts_as_a_stage_namespace(self):
        """The paths this driver emits must satisfy the adapter's own checks."""
        manifest = self.prepare()
        host = adapter.LinuxHost(
            project=PROJECT, stage_owner_root=self.owner,
            root=Path(manifest["paths"]["repo"]),
            control_dir=Path(manifest["paths"]["control_dir"]),
            staging=Path(manifest["paths"]["staging"]),
            locker=__import__("contextlib").nullcontext,
            build_locker=__import__("contextlib").nullcontext,
        )
        self.assertTrue(host.stage)
        self.assertEqual(host._stage_containment_failures(), [])

    def test_a_prepared_instance_tears_down_completely(self):
        self.prepare()
        report = driver.teardown(owner=self.owner, project=PROJECT, confirm=True)
        self.assertEqual(report["removed"], str(self.owner))
        self.assertFalse(self.owner.exists())
        # The source it was cloned from is untouched.
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.source), self.candidate)
        self.assertTrue((self.source / "fixture.txt").is_file())


if __name__ == "__main__":
    unittest.main()
