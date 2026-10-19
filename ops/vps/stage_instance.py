#!/usr/bin/env python3
"""Prepare and dispose of one isolated stage instance. Never runs Docker.

A full-stack rehearsal needs a stage instance that exists *before* the
controller is pointed at it: an owned private root, an independent clone, a
synthetic `.env` the controller can actually adopt from, a throwaway data
directory in the same place production keeps its own, and a fixture certificate
so the preserved HTTPS exposure gate can answer. Assembling that by hand is how
a rehearsal ends up bound to the production checkout.

This module assembles it, and stops. It writes files and runs `git`; it does
**not** run `docker`, does not reach the network, and never touches
`/opt/apps/stockspotter`, the production `.env`, the production control store,
the production compose project or any systemd unit. `prepare` prints the exact
`docker compose` sequence an operator then runs; `commands` reprints it;
`teardown` removes only what the manifest recorded it created.

Two properties are worth stating because they are easy to lose:

* **The clone is independent.** `git clone` hardlinks the object store by
  default, so `--no-hardlinks` is passed explicitly and recorded. `origin` is
  re-pointed at the real remote the source checkout uses, so the controller's
  `tip()` fetches the genuinely published master rather than a local copy of
  it. No master SHA and no CI result is ever synthesized here.
* **Each case gets its own everything.** `Controller._require_rehearsal` binds
  a rehearsal receipt to one policy digest and one exact baseline, so two
  baseline tuples run through one control directory would silently invalidate
  each other's evidence. One instance is one project, one owner root and one
  control directory; run the cases sequentially.

The real clock is used throughout. Nothing here injects a time, and the
promotion window stays whatever the host's clock says it is.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import deployment_host as adapter
import staging_rehearsal as rehearsal
import staging_tls

MANIFEST_NAME = "stage-instance.json"
REPO_DIRECTORY = "repo"
CONTROL_DIRECTORY = "control"
CANDIDATE_DIRECTORY = "candidate"
CERTIFICATE_DIRECTORY = "certificates"
# Production keeps `data/` inside the checkout; `/data/` is gitignored there.
# The rehearsal mirrors that rather than hanging it beside the repository, so
# the checkout-move guards are exercised against the layout that actually
# exists.
DATA_RELATIVE = f"{REPO_DIRECTORY}/data"
BROKER_COPY = "staging_broker.py"
# `Transaction.promote` hardcodes this branch for the candidate checkout, so a
# stage clone has to have it locally whatever branch its source was sitting on.
PROMOTION_BRANCH = "master"
DOCUMENT_NAME = "stage-compose.json"
DOTENV_RELATIVE = f"{REPO_DIRECTORY}/.env"


def _git(argv, *, cwd=None, runner=None) -> str:
    """One `git` invocation, no shell, output returned."""
    if runner is not None:
        return runner(["git", *argv], cwd=cwd)
    result = subprocess.run(["git", *argv], cwd=None if cwd is None else str(cwd),
                            capture_output=True, text=True, timeout=600, check=False)
    if result.returncode:
        raise RuntimeError(f"git {' '.join(argv[:3])} failed: {result.stderr.strip()[-500:]}")
    return result.stdout


def _branch_tips(repo, *, namespace="refs/heads/", runner=None) -> dict:
    """`{short name: sha}` for one ref namespace.

    `for-each-ref` rather than `rev-parse --verify`: a branch that does not
    exist is an ordinary answer here, not a command failure.
    """
    listed = _git(["for-each-ref", "--format=%(refname:short) %(objectname)", namespace],
                  cwd=repo, runner=runner)
    tips = {}
    for line in listed.splitlines():
        name, _, sha = line.strip().partition(" ")
        if name and sha.strip():
            tips[name] = sha.strip()
    return tips


def _refuse_production_overlap(owner: Path) -> None:
    """Nothing this module creates or removes may sit in a production path."""
    production = adapter.PRODUCTION_ROOT.resolve()
    store = adapter.DEFAULT_STORE.resolve()
    for name, protected in (("the production checkout", production),
                            ("the production controller store", store)):
        if owner == protected or owner.is_relative_to(protected) or protected.is_relative_to(owner):
            raise ValueError(f"owner root {owner} overlaps {name}")


def _require_owned_private(path: Path, *, files, allow_missing=False) -> None:
    """An owned, private, non-symlink real directory, by `lstat`.

    Applied to the owner root's *parent* as well as to the root itself. A
    world-writable parent -- `/var/tmp` being the obvious one -- lets anyone
    pre-create the owner root as a symlink before this module looks at it, and
    every containment check afterwards would then be asked about the link's
    target. Put stage instances under a private directory you own.
    """
    failures = files.secure_directory_failures(path, allow_missing=allow_missing)
    if failures:
        raise ValueError(f"{path} " + "; ".join(failures))


def validate_target(owner: Path, project: str, *, files) -> Path:
    """The shared precondition for preparing and for disposing of an instance."""
    if not adapter.STAGE_PROJECT.fullmatch(project):
        raise ValueError(f"{project!r} is not a validated stage namespace "
                         "(stockspotter-stage-<name>)")
    if project == adapter.PROJECT:
        raise ValueError("refusing to drive the production compose project")
    owner = Path(owner)
    if owner.is_symlink():
        raise ValueError(f"owner root {owner} is a symlink")
    owner = owner.resolve()
    if owner.name != project:
        raise ValueError(f"owner root {owner.name!r} must be named for the project "
                         f"{project!r}, so a stray path cannot be mistaken for one")
    _refuse_production_overlap(owner)
    return owner


def prepare(*, owner, project, recipe_path, source, remote=None, checkout=None,
            branch=None, force_branch=False, openssl="openssl", faults=(),
            files=None, runner=None) -> dict:
    """Assemble one stage instance on disk. Runs `git`; never `docker`."""
    files = files or adapter.Files()
    owner_parent = Path(owner).parent
    owner = validate_target(owner, project, files=files)
    if owner.exists():
        raise ValueError(f"{owner} already exists; tear the previous instance down first")
    _require_owned_private(owner_parent.resolve(), files=files)
    recipe = rehearsal.load_recipe(recipe_path)

    source = Path(source).resolve()
    if not (source / ".git").exists():
        raise ValueError(f"{source} is not a git checkout to clone from")
    remote = remote or _git(["remote", "get-url", "origin"], cwd=source, runner=runner).strip()
    if not remote:
        raise ValueError("the source checkout has no origin; the controller's tip() would "
                         "have nothing genuine to fetch")

    created = []
    owner.mkdir(mode=0o700)
    created.append(".")
    try:
        repo = owner / REPO_DIRECTORY
        # `--no-hardlinks` explicitly: the default shares the object store with
        # the source, which is not the independence this instance claims.
        _git(["clone", "--no-hardlinks", str(source), str(repo)], runner=runner)
        created.append(REPO_DIRECTORY)
        _git(["remote", "set-url", "origin", remote], cwd=repo, runner=runner)
        # `git clone` creates only the source's *current* branch locally. The
        # production checkout sits on a release branch, so a clone of it has no
        # local `master` at all -- and `Transaction.promote` calls
        # `checkout(candidate, "master")`, which refuses to create a branch and
        # would reject every promotion. The branch is materialised from the
        # clone's own remote-tracking ref, so it is the source's genuine master:
        # nothing is invented, and no published tip is assumed.
        retained = []
        if PROMOTION_BRANCH not in _branch_tips(repo, runner=runner):
            origin_tip = _branch_tips(repo, namespace="refs/remotes/origin/",
                                      runner=runner).get(f"origin/{PROMOTION_BRANCH}")
            if origin_tip is None:
                raise ValueError(
                    f"the source has no {PROMOTION_BRANCH} to retain, so a promotion "
                    f"could never check one out"
                )
            _git(["branch", PROMOTION_BRANCH, origin_tip], cwd=repo, runner=runner)
            retained.append({"branch": PROMOTION_BRANCH, "at": origin_tip,
                             "from": f"refs/remotes/origin/{PROMOTION_BRANCH}",
                             "why": "the source's own branch, kept because a clone of a "
                                    "release HEAD would not have it and promotion needs it"})

        synthetic_branch = None
        if checkout:
            if not branch:
                raise ValueError("--checkout needs --branch: a detached stage checkout "
                                 "has no branch for the controller to record")
            tip = _branch_tips(repo, runner=runner).get(branch)
            resolved = _git(["rev-parse", "--verify", f"{checkout}^{{commit}}"],
                            cwd=repo, runner=runner).strip()
            if not force_branch and tip is not None and tip != resolved:
                raise ValueError(
                    f"{branch} is at {tip} in the clone, not {resolved}. Moving a branch "
                    "away from its own tip is a synthetic prior position, so it has to be "
                    "asked for with --force-branch and is recorded as synthetic."
                )
            # `git switch -C` creates *or* resets the branch and lands on it in
            # one step. `git branch --force` cannot be used: it refuses to move
            # the branch that is currently checked out, which for `master` is
            # exactly the case this exists for.
            _git(["switch", "--quiet", "--force-create", branch, resolved],
                 cwd=repo, runner=runner)
            if force_branch and tip is not None and tip != resolved:
                # The only synthetic thing about either case: a genuine
                # ancestor commit placed on a local branch, so a promotion has
                # to move the branch forward and a rollback has to move it
                # back. The candidate and its CI stay entirely real.
                synthetic_branch = {
                    "branch": branch, "moved_from": tip, "set_to": resolved,
                    "why": "synthetic prior-master position; genuine ancestor commit, "
                           "no fabricated candidate and no simulated CI",
                }
        head = _git(["rev-parse", "HEAD"], cwd=repo, runner=runner).strip()
        at_branch = _git(["rev-parse", "--abbrev-ref", "HEAD"], cwd=repo, runner=runner).strip()

        for relative in (CONTROL_DIRECTORY, CANDIDATE_DIRECTORY, DATA_RELATIVE):
            (owner / relative).mkdir(mode=0o700, parents=True)
            created.append(relative)

        broker = owner / BROKER_COPY
        shutil.copyfile(Path(__file__).with_name(BROKER_COPY), broker)
        os.chmod(broker, 0o600)
        created.append(BROKER_COPY)

        fixture = staging_tls.provision(owner / CERTIFICATE_DIRECTORY, openssl)
        created.append(CERTIFICATE_DIRECTORY)

        document = staging_tls.derive(
            recipe, stage_root=owner, project=project, broker_script=broker,
            certificate=Path(fixture["certificate"]), key=Path(fixture["key"]),
            data_dir=owner / DATA_RELATIVE, faults=faults,
        )
        document_path = owner / DOCUMENT_NAME
        rehearsal.write_private(document_path, json.dumps(document, indent=2, sort_keys=True))
        created.append(DOCUMENT_NAME)

        # Written from the finished document, so the file the controller reads
        # and the environment the containers carry cannot disagree. Without
        # this, `_role_env` reports every synthetic secret `unreproducible` and
        # adoption is `Held`.
        rehearsal.write_private(owner / DOTENV_RELATIVE, rehearsal.synthetic_dotenv(document))
        created.append(DOTENV_RELATIVE)

        # Asked here rather than discovered three commands later. `snapshot()`
        # refuses a dirty checkout outright, so if the cloned commit does not
        # ignore `data/` and `.env`, every controller call from `report` onwards
        # fails with "production checkout has local changes" -- about a stage
        # checkout, which is a confusing way to learn it.
        dirty = _git(["status", "--porcelain", "--untracked-files=all"],
                     cwd=repo, runner=runner)
        if dirty.strip():
            raise ValueError(
                "the stage checkout is dirty once .env and data/ exist: "
                + " ".join(dirty.split())[:200] + ". Both must be gitignored by the "
                "cloned commit, as they are in production, or the controller will "
                "refuse this checkout."
            )

        manifest = {
            "version": 1,
            "project": project,
            "owner_root": str(owner),
            "created": created,
            "clone": {"source": str(source), "origin": remote, "no_hardlinks": True,
                      "head": head, "branch": at_branch,
                      "retained_branches": retained,
                      "synthetic_branch": synthetic_branch},
            "paths": {"repo": str(owner / REPO_DIRECTORY),
                      "control_dir": str(owner / CONTROL_DIRECTORY),
                      "staging": str(owner / CANDIDATE_DIRECTORY),
                      "data": str(owner / DATA_RELATIVE),
                      "document": str(document_path),
                      "dotenv": str(owner / DOTENV_RELATIVE)},
            "certificate_sha256": fixture["certificate_sha256"],
            "faults": sorted(faults),
            "docker": "never run by this module",
            "clock": "the host's real clock; no synthetic promotion window",
            "compose": compose_commands(project, document_path),
            "controller": controller_commands(project, owner),
            "scope": ("one isolated stage instance; proves nothing about production and "
                      "activates no automatic promotion"),
        }
        created.append(MANIFEST_NAME)
        rehearsal.write_private(owner / MANIFEST_NAME,
                                json.dumps(manifest, indent=2, sort_keys=True))
        return manifest
    except BaseException:
        # A half-built instance is worse than none: it would be adopted.
        _remove_owner(owner, files=files)
        raise


def compose_commands(project: str, document_path) -> list:
    """What the operator runs. Printed, never executed here."""
    base = ["docker", "compose", "-p", project, "-f", str(document_path)]
    return [" ".join(base + ["up", "-d", "--no-build", "--wait", "--wait-timeout", "180"]),
            " ".join(base + ["ps"]),
            " ".join(base + ["logs", "--no-color", "--tail", "200"]),
            " ".join(base + ["down", "--remove-orphans"])]


def controller_commands(project: str, owner: Path) -> list:
    """The controller invocations this instance is shaped for, in order.

    Every flag here is one `deployment_host.build_parser` actually accepts, and
    every subcommand one it actually has -- an emitted plan that does not parse
    is worse than no plan, because it is copied before it is read. The store is
    `--store`, not a separate control directory; the owner root has to be named
    because a stage project without one is refused at construction.
    """
    common = (f"--project {project} "
              f"--stage-owner-root {owner} "
              f"--root {owner / REPO_DIRECTORY} "
              f"--store {owner / CONTROL_DIRECTORY} "
              f"--staging {owner / CANDIDATE_DIRECTORY}")
    return [f"deployment_host.py {common} inspect",
            f"deployment_host.py {common} adopt --confirm ...",
            f"deployment_host.py {common} prepare",
            f"deployment_host.py {common} rehearse --confirm",
            f"deployment_host.py {common} promote --confirm"]


def read_manifest(owner: Path) -> dict:
    path = Path(owner) / MANIFEST_NAME
    if path.is_symlink():
        raise ValueError(f"{path} is a symlink")
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise ValueError(f"no {MANIFEST_NAME} in {owner}; refusing to remove a directory "
                         "this module cannot show it created") from error
    except ValueError as error:
        raise ValueError(f"unreadable {MANIFEST_NAME} in {owner}") from error
    if not isinstance(manifest, dict) or manifest.get("version") != 1:
        raise ValueError(f"unrecognised {MANIFEST_NAME} version")
    return manifest


def _remove_owner(owner: Path, *, files) -> None:
    """`Files.remove_tree` refuses anything outside `inside`; this adds the rest."""
    owner = Path(owner)
    if not owner.exists():
        return
    _refuse_production_overlap(owner.resolve())
    files.remove_tree(owner, owner.parent)


def teardown(*, owner, project, confirm: bool, files=None) -> dict:
    """Remove one prepared instance, and refuse anything it cannot account for.

    Deliberately requires the manifest. The alternative -- trusting a path on
    the command line -- is how a mistyped `--owner-root` deletes something that
    matters, and the 145 GB of research and audit journals on this host are not
    recoverable.

    This does not stop containers, because it does not run Docker. Run the
    `docker compose ... down --remove-orphans` line it prints first; a stage
    container left behind would keep a bind on a directory about to vanish.
    """
    files = files or adapter.Files()
    if not confirm:
        raise ValueError("teardown requires --confirm")
    owner = validate_target(owner, project, files=files)
    manifest = read_manifest(owner)
    if manifest.get("project") != project:
        raise ValueError(f"{MANIFEST_NAME} records project {manifest.get('project')!r}, "
                         f"not {project!r}")
    if Path(manifest.get("owner_root", "")) != owner:
        raise ValueError(f"{MANIFEST_NAME} records owner root "
                         f"{manifest.get('owner_root')!r}, not {owner}")
    _require_owned_private(owner, files=files)
    # Token match, not `" down "`: the substring form missed a command ending
    # in `down`, so the one line an operator most needs to see was dropped.
    down = [line for line in manifest.get("compose") or () if "down" in line.split()]
    _remove_owner(owner, files=files)
    return {"removed": str(owner), "project": project,
            "run_first_if_the_stack_is_up": down,
            "docker": "never run by this module"}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="stage_instance.py",
        description="Prepare or dispose of one isolated stage instance. Never runs Docker.")
    commands = parser.add_subparsers(dest="command", required=True)

    create = commands.add_parser("prepare", help="assemble a stage instance on disk")
    create.add_argument("--owner-root", required=True, type=Path,
                        help="disposable root; must be named for --project")
    create.add_argument("--project", required=True, help="stockspotter-stage-<name>")
    create.add_argument("--recipe", required=True, type=Path,
                        help="a captured baseline recipe to derive the stack from")
    create.add_argument("--source", required=True, type=Path,
                        help="checkout to clone (read-only; --no-hardlinks)")
    create.add_argument("--remote", default=None,
                        help="origin URL for the clone; defaults to the source's own")
    create.add_argument("--checkout", default=None, help="commit the stage checkout starts at")
    create.add_argument("--branch", default=None, help="branch the stage checkout starts on")
    create.add_argument("--force-branch", action="store_true",
                        help="move --branch to --checkout (a synthetic prior position; "
                             "recorded as such in the manifest)")
    create.add_argument("--openssl", default="openssl")
    create.add_argument("--fault", action="append", default=[], choices=sorted(rehearsal.FAULTS))

    for name, help_text in (("commands", "reprint the command plan"),
                            ("teardown", "remove a prepared instance")):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("--owner-root", required=True, type=Path)
        command.add_argument("--project", required=True)
        if name == "teardown":
            command.add_argument("--confirm", action="store_true")
    return parser


def main(argv=None, out=None) -> int:
    arguments = build_parser().parse_args(argv)
    out = out if out is not None else sys.stdout
    try:
        if arguments.command == "prepare":
            manifest = prepare(
                owner=arguments.owner_root, project=arguments.project,
                recipe_path=arguments.recipe, source=arguments.source,
                remote=arguments.remote, checkout=arguments.checkout,
                branch=arguments.branch, force_branch=arguments.force_branch,
                openssl=arguments.openssl, faults=arguments.fault,
            )
            print(json.dumps(manifest, indent=2, sort_keys=True), file=out)
            return 0
        if arguments.command == "commands":
            owner = validate_target(arguments.owner_root, arguments.project,
                                    files=adapter.Files())
            manifest = read_manifest(owner)
            print(json.dumps({"compose": manifest.get("compose"),
                              "controller": manifest.get("controller")},
                             indent=2, sort_keys=True), file=out)
            return 0
        report = teardown(owner=arguments.owner_root, project=arguments.project,
                          confirm=arguments.confirm)
        print(json.dumps(report, indent=2, sort_keys=True), file=out)
        return 0
    except ValueError as error:
        print(json.dumps({"refused": str(error)}, indent=2), file=sys.stderr)
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
