#!/usr/bin/env python3
"""Real Linux host adapter for the schema-2 deployment transaction.

`deployment_transaction.py` decides; this module performs. It implements the
`Host` protocol against the actual VPS: the existing deployment `flock`, the
existing `stockspotter-vps` compose project, the existing
`ops/vps/deploy_guard.py` exact-SHA CI gate, the existing `ops/check_health.py`
probes and the existing read-only broker checks in `ops/release_preflight.py`.

Every host effect goes through one injected command runner and one injected
file helper, so the tests drive the real argument vectors without a host. No
function here shells out through a shell, and no credential is ever placed on
a command line, written to a controller file or printed: environment values are
reduced to SHA-256 digests, secrets stay in the host's own `.env` and reach
containers only through compose's `env_file`, and the in-container probes read
the API token from the container's own environment.

Three design decisions dominate this file and are easy to get wrong:

1. **The legacy marker is read-only.** `ops/vps/.deployed-commit` is the state
   file of `deploy.sh`, which `stockspotter-deploy.timer` runs every two
   minutes. `deploy.sh` rebuilds whenever `HEAD != <marker>` -- so writing the
   *running* build's commit into that file would make the legacy timer rebuild
   the checkout's commit from the *base* compose file within two minutes,
   destroying the live overlay configuration. This controller therefore keeps
   its own private marker in its control directory and never writes the legacy
   one. Promotion additionally refuses to run while the legacy timer is enabled
   or active.
2. **Recreation uses a private, fully resolved compose recipe.** The live stack
   was launched with overlay compose files that are not in this repository, so
   `docker compose -f ops/vps/docker-compose.yml up` would silently drop their
   effective environment -- on promotion *and* on rollback. Adoption captures
   the observed effective configuration of all five containers into a private
   immutable recipe (exact image IDs, effective per-role environment, binds,
   ports, logging, restart policy, command). Promotion runs that recipe with
   only the image IDs replaced; rollback runs it verbatim. No shared mutable
   tag is ever written, so nothing the legacy path reads is disturbed.
3. **`env_hash` binds the whole operator-controlled environment.** Not a flag
   subset: every per-role effective environment variable is bound by name with
   its value's digest, excluding only build identity and image-provided
   variables on a validated allowlist (those are recorded as drift instead,
   because an image rebuild legitimately changes them).

What this module deliberately does NOT do (see
`docs/MASTER-DEPLOYMENT-ADAPTER-2026-10-14.md`):

* it does not touch `deploy.sh`, `deploy_guard.py`, `deploy-chart-web.sh`, the
  existing `stockspotter-deploy` units or `ops/vps/.deployed-commit`, and it
  never falls back to them;
* it does not create, enable or modify a buildx builder, and never prunes;
* it does not promote outside the 20:00-04:00 America/New_York window, and it
  rolls back rather than finishing a transaction that crosses 04:00;
* it does not promote a subset of the five roles: holding a role is an explicit
  adoption decision that *blocks* promotion rather than silently unpinning it.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass, field
from datetime import datetime, time as clock_time, timedelta, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time as time_module
from typing import Callable, Optional, Protocol
from zoneinfo import ZoneInfo


def _load_core():
    """Load the sibling transaction module by path.

    Imported this way rather than by name so the controller works from an
    installation directory that is not on `sys.path` (the immutable
    installation this adapter is meant to run from) and from the repository
    checkout, with identical behaviour.
    """
    path = Path(__file__).with_name("deployment_transaction.py")
    # One module object per process: a second execution would create a second
    # set of Rejected/Deferred/Held classes, and `except Rejected` would then
    # silently miss the other copy's exceptions.
    loaded = sys.modules.get("deployment_transaction")
    if loaded is not None and getattr(loaded, "__file__", None) == str(path):
        return loaded
    spec = importlib.util.spec_from_file_location("deployment_transaction", path)
    if spec is None or spec.loader is None:  # pragma: no cover - packaging error
        raise SystemExit(f"deployment_host: cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault("deployment_transaction", module)
    spec.loader.exec_module(module)
    return module


core = _load_core()
ROLES = core.ROLES
Deferred, Held, Rejected = core.Deferred, core.Held, core.Rejected
image_set, source_sha, state = core.image_set, core.source_sha, core.state

# ---------------------------------------------------------------------------
# The real project's own names. These are facts about the running host, not
# configuration: they match ops/vps/docker-compose.yml, deploy.sh and
# deploy-chart-web.sh exactly (`stockspotter-vps-web-1`, ...).
# ---------------------------------------------------------------------------
PROJECT = "stockspotter-vps"
# Read-only reference. The controller never recreates *from* this file; it is
# parsed by `docker compose config` to preflight the private recipe against the
# compose contract of the commit being promoted.
BASE_COMPOSE_FILE = "ops/vps/docker-compose.yml"
# deploy.sh's state file. READ-ONLY here, always. See the module docstring.
LEGACY_MARKER_PATH = "ops/vps/.deployed-commit"
LEGACY_TIMER = "stockspotter-deploy.timer"
# This controller's own marker, inside its private 0700 control directory.
CONTROLLER_MARKER_NAME = "deployed-commit"
BASELINE_RECIPE_NAME = "baseline-recipe.json"
# The compose document handed to `docker compose -f`. Written 0600 next to the
# other control files, regenerated for every recreation, never shared.
ACTIVE_RECIPE_NAME = "controller-compose.json"
REHEARSAL_NAME = "rehearsal.json"
PREPARED_NAME = "prepared.json"
AUTO_NAME = "auto.json"
LOCK_PATH = ".git/stockspotter-deploy.lock"
# Builds do not take the deployment lock (recreating under it is what that lock
# protects, and a build must not hold it for an hour), so they need their own.
# This is what keeps the two-minute watch timer and a manual `prepare` from
# building the same five images at the same time.
BUILD_LOCK_PATH = ".git/stockspotter-prepare.lock"
BOOT_ID_PATH = "/proc/sys/kernel/random/boot_id"
CONTAINERS = {role: f"{PROJECT}-{role}-1" for role in ROLES}
# One `docker inspect` answering both bounds of the buildkit container.
BUILDER_LIMITS_FORMAT = ("{{.HostConfig.Memory}} {{.HostConfig.NanoCpus}} "
                         "{{.HostConfig.CpuQuota}} {{.HostConfig.CpuPeriod}}")
# A compose project name this controller may drive *besides* the production
# one. Isolated by construction: the prefix cannot collide with
# `stockspotter-vps`, so no production container name can be recreated or
# removed through a stage project. The default stays exactly `PROJECT`.
STAGE_PROJECT = re.compile(r"stockspotter-stage-[a-z0-9][a-z0-9-]{2,30}")
PRODUCTION_ROOT = Path("/opt/apps/stockspotter")
DOCKERFILES = {
    "web": "apps/client/Dockerfile",
    "ws": "crates/ws-server/Dockerfile",
    "qualify": "python/Dockerfile",
    "auto-trader": "crates/auto-trader/Dockerfile",
    "discovery-review": "python/Dockerfile",
}
# Static ordering from ops/vps/docker-compose.yml. Carried into the private
# recipe so recreation ordering is the compose project's own, not alphabetical.
DEPENDS_ON = {
    "web": (),
    "qualify": (),
    "ws": ("qualify",),
    "auto-trader": ("ws",),
    "discovery-review": ("ws", "auto-trader"),
}
REVISION_LABEL = "org.opencontainers.image.revision"
EXPERIMENTAL_LABEL = "io.stockspotter.experimental-strategies"
COMPOSE_PROJECT_LABEL = "com.docker.compose.project"
COMPOSE_SERVICE_LABEL = "com.docker.compose.service"
COMPOSE_CONFIG_FILES_LABEL = "com.docker.compose.project.config_files"
COMPOSE_WORKING_DIR_LABEL = "com.docker.compose.project.working_dir"

# Content that a candidate commit may not change. `.env` is untracked, so a
# checkout move cannot alter it; the other two are tracked and a branch move
# can, which is exactly what this freeze exists to catch.
FROZEN_PATHS = (".env", "OI-V2-PREREGISTRATION-2026-09-19.md", "ops/qualify/session.sh")

# Non-secret effective flags reported verbatim by `inspect` and compared for
# parity. `env_hash` binds *every* variable (by digest); this list only exists
# so drift in the flags an operator actually reasons about is named in the
# error message instead of showing up as an opaque hash mismatch.
PARITY_FLAGS = (
    "IAC_ENABLED",
    "EXTREME_MOMENTUM_ENABLED",
    "OPPORTUNITY_INTELLIGENCE_SHADOW",
    "OPPORTUNITY_OBSERVATION",
    "OPPORTUNITY_OBSERVATION_ROOT",
    "OPPORTUNITY_OBSERVATION_NAMESPACE",
    "ORDER_FLOW_ENABLED",
    "AUTO_TRADER_EXECUTION_MODE",
    "IGNITION_UNIVERSE_MODE",
)
# Order-flow ingestion must stay off regardless of what the baseline said.
MUST_BE_OFF = ("ORDER_FLOW_ENABLED",)
OFF_VALUES = (None, "", "0", "false", "False", "off")
# Build identity is verified by label, by the compiled stamp and by the baked
# artifact digest, so it must not enter `env_hash`: ws's Dockerfile sets
# `ENV STOCKSPOTTER_COMMIT=<arg>`, which differs for every candidate by design.
IDENTITY_ENV = ("STOCKSPOTTER_COMMIT",)
# Set by the container runtime per container, not by the operator.
VOLATILE_ENV = ("HOSTNAME",)
# Variables an *image* may legitimately provide and change when its base image
# is rebuilt. Only exact names, never prefixes: anything else an image provides
# is bound by `env_hash`, so an unexpected image-provided variable fails the
# promotion proof instead of being quietly forgiven. Their digests are recorded
# per role in every receipt, so this exclusion hides nothing.
IMAGE_ENV_ALLOWLIST = frozenset({
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LANGUAGE", "TZ", "PWD", "SHLVL",
    "DEBIAN_FRONTEND", "SSL_CERT_DIR", "SSL_CERT_FILE",
    "PYTHONUNBUFFERED", "PYTHONDONTWRITEBYTECODE", "PYTHON_VERSION",
    "PYTHON_PIP_VERSION", "PYTHON_SETUPTOOLS_VERSION", "PYTHON_GET_PIP_URL",
    "PYTHON_GET_PIP_SHA256", "PYTHON_SHA256", "GPG_KEY",
    "NODE_VERSION", "YARN_VERSION", "BUN_INSTALL", "BUN_RUNTIME_TRANSPILER_CACHE_PATH",
    "RUST_VERSION", "RUSTUP_HOME", "CARGO_HOME", "LD_LIBRARY_PATH",
})
# A name that may carry a credential. Such a variable is never written into a
# controller file: it must reach the container through the host's own `.env`
# via compose's `env_file`, or the controller refuses (fail closed).
SECRET_NAME = re.compile(
    r"TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|PRIVATE|SESSION|COOKIE"
    r"|API_KEY|ACCESS_KEY|_KEY$|^KEY$|^ALPACA_|WEBHOOK|SENTRY|DSN",
    re.IGNORECASE,
)

# Only these bind destinations exist in the compose file. Anything else -- in
# particular a bind over /app or over the compiled binary -- is refused.
ALLOWED_BINDS = {
    "web": frozenset(),
    "qualify": frozenset(),
    "ws": frozenset({"/app/data"}),
    "auto-trader": frozenset({"/app/data"}),
    "discovery-review": frozenset({"/app/data"}),
}
SHADOWED_PATHS = ("/", "/app", "/app/dist", "/app/ws-server", "/app/auto-trader")

# The single baked artifact whose bytes identify each role's build. Probed with
# `sha256sum` inside the running container; a role whose image has no
# `sha256sum` (or no such path) records `null` rather than a guess, and then
# cannot be enforced -- which the adoption receipt states explicitly.
ARTIFACT_PATHS = {
    "web": "/app/dist/index.html",
    "ws": "/app/ws-server",
    "qualify": "/app/check_health.py",
    "auto-trader": "/app/auto-trader",
    "discovery-review": "/app/discovery_review.py",
}

# Invariants that an *adopted baseline* may be exempted from by explicit,
# recorded operator decision -- because the live stack predates them and a
# candidate's stricter checks must not make adoption impossible. The exemption
# applies to a role only while it runs the adopted baseline image; a candidate
# image is always held to the strict rule. Bind shadowing, public port exposure
# and the order-flow kill switch are never exemptable.
EXEMPTABLE = ("unbounded-logs", "restart-policy")
ROLE_DECISIONS = ("rebuild", "hold")

ZONE = "America/New_York"
WINDOW_OPEN = clock_time(20, 0)
WINDOW_CLOSE = clock_time(4, 0)

SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
IMAGE_ID = re.compile(r"sha256:[0-9a-f]{64}")
ENV_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

# deploy.sh's own pre-recreation assertion, reused verbatim so the two paths
# cannot disagree about what "configured" means.
TOKEN_ASSERT = (
    'import os; assert len(os.environ.get("STOCKSPOTTER_API_TOKEN", "")) >= 32, '
    '"Configure STOCKSPOTTER_API_TOKEN before deployment"'
)

# Read the compiled commit of the *running* ws binary. `report.commit` is
# `option_env!("STOCKSPOTTER_COMMIT")`, baked in at build time, which is the
# only identity the process cannot rewrite (crates/ws-server/src/provenance.rs).
COMPLETENESS_PROBE = """
import json, os, urllib.request
request = urllib.request.Request(
    "http://ws:8788/research/completeness",
    headers={"Authorization": "Bearer " + os.environ["STOCKSPOTTER_API_TOKEN"]},
)
with urllib.request.urlopen(request, timeout=20) as response:
    document = json.load(response)
report = document.get("report") or {}
print(json.dumps({"commit": report.get("commit")}))
"""

# The served web bundle, fetched from *inside* an isolated stage project.
# Production keeps using the loopback `curl` the health script has always used;
# a stage project publishes no port at all, so there is nothing on 127.0.0.1 to
# fetch and the bundle has to be read over the project's own network.
STAGE_BUNDLE_PROBE = """
import sys, urllib.request
with urllib.request.urlopen("http://web:3000/", timeout=20) as response:
    sys.stdout.write(response.read().decode("utf-8", "replace"))
"""

# Simulated (journal-mode) exposure. The broker half comes from the
# repository's own ops/release_preflight.py, piped in unchanged.
SIMULATED_EXPOSURE_PROBE = """
import json, os, urllib.request
request = urllib.request.Request(
    "http://ws:8788/auto-trader/status",
    headers={"Authorization": "Bearer " + os.environ["STOCKSPOTTER_API_TOKEN"]},
)
with urllib.request.urlopen(request, timeout=20) as response:
    document = json.load(response)
print(json.dumps({"openPositions": len(document["openPositions"]),
                  "executionMode": document.get("executionMode")}))
"""


# ---------------------------------------------------------------------------
# Injected effects
# ---------------------------------------------------------------------------
@dataclass(frozen=True)
class Result:
    argv: tuple
    code: int
    out: str
    err: str


class Runner(Protocol):
    def __call__(self, argv: tuple, *, cwd: Optional[str] = None,
                 stdin: Optional[str] = None, timeout: int = 120) -> Result: ...


def subprocess_runner(argv, *, cwd=None, stdin=None, timeout=120):
    """The only place this module starts a process. No shell, ever."""
    completed = subprocess.run(
        list(argv), cwd=cwd, input=stdin, capture_output=True, text=True,
        timeout=timeout, check=False, shell=False,
    )
    return Result(tuple(argv), completed.returncode, completed.stdout, completed.stderr)


class Files:
    """Controller-side filesystem effects, kept injectable for the tests."""

    def exists(self, path) -> bool:
        return Path(path).exists()

    def read_text(self, path) -> Optional[str]:
        try:
            return Path(path).read_text(encoding="utf-8")
        except OSError:
            return None

    def digest(self, path) -> Optional[str]:
        """SHA-256 of a file's bytes. Returns a digest, never content: `.env`
        is one of the files this is used on."""
        try:
            with open(path, "rb") as handle:
                sha = hashlib.sha256()
                for block in iter(lambda: handle.read(1024 * 1024), b""):
                    sha.update(block)
                return sha.hexdigest()
        except OSError:
            return None

    def env_pairs(self, path) -> Optional[dict]:
        """`.env` as `{NAME: sha256(value)}` -- digests, never values.

        Quoting follows compose's `env_file` handling (one matching pair of
        surrounding quotes is stripped). A variable this parser reads
        differently from compose will simply fail to match the observed
        container value, which the classifier then reports rather than
        silently accepting -- see `_role_env`.
        """
        text = self.read_text(path)
        if text is None:
            return None
        pairs = {}
        for line in text.splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            name, _, value = line.partition("=")
            name = name.strip()
            if name.startswith("export "):
                name = name[len("export "):].strip()
            if not ENV_NAME.fullmatch(name):
                continue
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            pairs[name] = hashlib.sha256(value.encode("utf-8")).hexdigest()
        return pairs

    def write_private(self, path, text) -> None:
        path = Path(path)
        temporary = path.with_name(path.name + ".pending")
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)

    def make_private_dir(self, path) -> None:
        Path(path).mkdir(mode=0o700, parents=True, exist_ok=True)

    def is_private(self, path) -> bool:
        """0700/0600 and owned by this user. Non-POSIX hosts cannot answer."""
        if os.name != "posix":
            return True
        try:
            info = Path(path).stat()
        except OSError:
            return False
        return info.st_uid == os.getuid() and not info.st_mode & 0o077

    def remove_tree(self, path, inside) -> None:
        path, inside = Path(path).resolve(), Path(inside).resolve()
        if inside not in path.parents:
            raise Held(f"refusing to remove {path} outside {inside}")
        shutil.rmtree(path, ignore_errors=True)

    def free_bytes(self, path) -> int:
        return shutil.disk_usage(str(path)).free


class Flock:
    """The *existing* deployment lock, taken the way deploy.sh takes it.

    `exec 9>.git/stockspotter-deploy.lock; flock -w 30 9` in shell is an
    exclusive advisory lock on that file; `fcntl.flock` on the same path is the
    same lock, so a timer run, `deploy-chart-web.sh` and this controller
    serialize against each other.
    """

    def __init__(self, path, seconds=30, sleeper=time_module.sleep,
                 clock=time_module.monotonic):
        self.path, self.seconds, self.sleeper, self.clock = str(path), seconds, sleeper, clock

    @contextmanager
    def __call__(self):
        try:
            import fcntl
        except ImportError as error:  # pragma: no cover - POSIX hosts only
            raise Held("POSIX deployment lock is unavailable on this platform") from error
        descriptor = os.open(self.path, os.O_WRONLY | os.O_CREAT, 0o600)
        deadline = self.clock() + self.seconds
        try:
            while True:
                try:
                    fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except OSError:
                    if self.clock() >= deadline:
                        raise Deferred("another deployment holds the lock")
                    self.sleeper(1)
            try:
                yield
            finally:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)


# ---------------------------------------------------------------------------
# Adopted policy
# ---------------------------------------------------------------------------
@dataclass(frozen=True)
class Policy:
    """Everything adoption pinned, as one explicitly versioned record.

    `baseline_images` and `artifacts` make the baseline-only exemptions and the
    baked-artifact checks *precise*: they apply to a role exactly while it runs
    the adopted image, so a candidate is never forgiven a stricter rule.
    `decisions` and `provenance_digest` are what keep a promotion from silently
    unpinning a deliberately old protected image.
    """

    frozen: dict
    flags: dict
    experimental_strategies: bool
    baseline_images: dict
    decisions: dict
    exemptions: dict
    artifacts: dict
    provenance_digest: str

    def as_json(self) -> dict:
        return {
            "version": 2,
            "frozen": dict(self.frozen),
            "flags": {role: dict(values) for role, values in self.flags.items()},
            "experimental_strategies": bool(self.experimental_strategies),
            "baseline_images": dict(self.baseline_images),
            "decisions": dict(self.decisions),
            "exemptions": {role: sorted(values) for role, values in self.exemptions.items()},
            "artifacts": dict(self.artifacts),
            "provenance_digest": self.provenance_digest,
        }

    def digest(self) -> str:
        return _digest_value(self.as_json())

    def exempt(self, role, check, image) -> bool:
        """Is `check` waived for `role`? Only on the adopted baseline image."""
        if check not in self.exemptions.get(role, ()):
            return False
        return self.baseline_images.get(role) == image

    @staticmethod
    def from_json(value) -> "Policy":
        if (not isinstance(value, dict) or value.get("version") != 2
                or not isinstance(value.get("frozen"), dict)
                or not isinstance(value.get("flags"), dict)
                or not isinstance(value.get("experimental_strategies"), bool)
                or not isinstance(value.get("baseline_images"), dict)
                or not isinstance(value.get("decisions"), dict)
                or not isinstance(value.get("exemptions"), dict)
                or not isinstance(value.get("artifacts"), dict)
                or not isinstance(value.get("provenance_digest"), str)):
            raise Held("Malformed adoption policy")
        if (set(value["flags"]) != ROLES or set(value["decisions"]) != ROLES
                or set(value["exemptions"]) != ROLES or set(value["artifacts"]) != ROLES
                or sorted(value["frozen"]) != sorted(FROZEN_PATHS)):
            raise Held("Adoption policy does not describe this deployment")
        image_set(value["baseline_images"])
        if any(decision not in ROLE_DECISIONS for decision in value["decisions"].values()):
            raise Held("Adoption policy carries an unknown role decision")
        for values in value["exemptions"].values():
            if not isinstance(values, list) or any(name not in EXEMPTABLE for name in values):
                raise Held("Adoption policy carries an unknown baseline exemption")
        for digest in value["artifacts"].values():
            if digest is not None and not (isinstance(digest, str) and DIGEST.fullmatch(digest)):
                raise Held("Adoption policy carries a malformed artifact digest")
        if not DIGEST.fullmatch(value["provenance_digest"]):
            raise Held("Adoption policy carries a malformed provenance digest")
        return Policy(value["frozen"], value["flags"], value["experimental_strategies"],
                      value["baseline_images"], value["decisions"], value["exemptions"],
                      value["artifacts"], value["provenance_digest"])


def _canonical(value) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _digest_value(value) -> str:
    return hashlib.sha256(_canonical(value).encode("utf-8")).hexdigest()


def _digest_text(value) -> str:
    return hashlib.sha256((value or "").encode("utf-8")).hexdigest()


def _sha(text, description) -> str:
    candidate = (text or "").strip()
    if not SHA.fullmatch(candidate):
        raise Held(f"{description} is not a full lowercase object id")
    return candidate


def _env_map(entries) -> dict:
    """`["A=1", "B=2"]` -> `{"A": "1", "B": "2"}`, last occurrence winning."""
    values = {}
    for entry in entries or []:
        if isinstance(entry, str) and "=" in entry:
            name, value = entry.split("=", 1)
            if ENV_NAME.fullmatch(name) and name not in VOLATILE_ENV:
                values[name] = value
    return values


def _secretish(name) -> bool:
    return bool(SECRET_NAME.search(name))


def _bind_parts(bind) -> tuple:
    """`/src:/dst:rw` -> `("/src", "/dst", "rw")`, tolerating a missing mode."""
    pieces = str(bind).split(":")
    if len(pieces) == 2:
        return pieces[0], pieces[1], ""
    if len(pieces) >= 3:
        return pieces[0], pieces[1], ":".join(pieces[2:])
    raise Rejected(f"malformed bind mount {bind!r}")


# ---------------------------------------------------------------------------
# Conformance: observed containers against a compose document
# ---------------------------------------------------------------------------
def conformance_failures(compose, documents, dotenv) -> list:
    """Every way the observed containers differ from a compose document.

    Shared deliberately. The production adapter turns the first failures into a
    `Rejected` proof, so the transaction rolls back; `staging_rehearsal.py`
    reports all of them, because finding them in an isolated stack *before*
    production is its entire purpose. One implementation means a staging
    rehearsal cannot pass something production would refuse, or vice versa.

    A service named in `compose` but missing from `documents` is a failure, not
    a crash.
    """
    failures = []
    for role in sorted(compose["services"]):
        service = compose["services"][role]
        document = documents.get(role)
        if not isinstance(document, dict):
            failures.append(f"{role}: no such container")
            continue
        host_config = document.get("HostConfig") or {}
        config = document.get("Config") or {}
        if document.get("Image") != service["image"]:
            failures.append(f"{role}: runs {document.get('Image')}, "
                            f"not the pinned {service['image']}")
        observed = _env_map(config.get("Env"))
        declared = _env_map(service.get("environment"))
        for name, value in sorted(declared.items()):
            if observed.get(name) != value:
                failures.append(f"{role}: lost the configured {name}")
        # Nothing beyond what the document declares, the host's `.env`
        # supplies, build identity, or the image allowlist. This is what
        # closes the gap `env_hash` cannot: a rebuilt image that bakes a new
        # provider or auth variable changes the effective environment without
        # changing any operator-controlled input, and must be a failed proof
        # -- therefore a rollback -- not a silent acceptance.
        permitted = set(declared) | set(IDENTITY_ENV) | set(IMAGE_ENV_ALLOWLIST)
        if service.get("env_file"):
            permitted |= set(dotenv)
        undeclared = sorted(name for name in observed if name not in permitted)
        if undeclared:
            failures.append(f"{role}: carries undeclared environment "
                            f"{', '.join(undeclared)}; the image or launch path added it")
        if service.get("env_file"):
            missing = sorted(name for name in dotenv
                             if _digest_text(observed.get(name)) != dotenv[name]
                             and name not in declared)
            if missing:
                failures.append(f"{role}: does not carry .env "
                                f"({len(missing)} variables differ)")
        expected_binds = {_bind_parts(entry)[:2] for entry in service.get("volumes") or []}
        observed_binds = {_bind_parts(entry)[:2] for entry in host_config.get("Binds") or []}
        if expected_binds != observed_binds:
            failures.append(f"{role}: bind mounts are {sorted(observed_binds)}, "
                            f"not {sorted(expected_binds)}")
        expected_ports = set()
        for entry in service.get("ports") or []:
            pieces = str(entry).split(":")
            expected_ports.add((pieces[0], pieces[1], pieces[2].partition("/")[0]))
        observed_ports = set()
        for port, bindings in (host_config.get("PortBindings") or {}).items():
            for binding in bindings or []:
                observed_ports.add(((binding or {}).get("HostIp"),
                                    (binding or {}).get("HostPort"),
                                    port.partition("/")[0]))
        if expected_ports != observed_ports:
            failures.append(f"{role}: port publication is {sorted(observed_ports)}, "
                            f"not {sorted(expected_ports)}")
        if (host_config.get("RestartPolicy") or {}).get("Name") != service["restart"]:
            failures.append(f"{role}: restart policy is not {service['restart']}")
        log_config = host_config.get("LogConfig") or {}
        if (log_config.get("Type") != service["logging"]["driver"]
                or dict(log_config.get("Config") or {}) != service["logging"]["options"]):
            failures.append(f"{role}: logging differs from the recipe")
        for key, name in (("command", "Cmd"), ("entrypoint", "Entrypoint")):
            if key in service and list(config.get(name) or []) != list(service[key]):
                failures.append(f"{role}: {key} differs from the recipe")
        if service.get("working_dir") and config.get("WorkingDir") != service["working_dir"]:
            failures.append(f"{role}: working directory differs from the recipe")
        if service.get("user") and config.get("User") != service["user"]:
            failures.append(f"{role}: user differs from the recipe")
    return failures


# ---------------------------------------------------------------------------
# Isolated stage namespaces
# ---------------------------------------------------------------------------
# Every synthetic value a stage stack carries. Grepping a stage document for
# this marker answers "could a real credential have reached it?".
STAGE_MARKER = "staging-not-a-real-credential"


def _inside(path, other) -> bool:
    path, other = Path(path), Path(other)
    return path == other or other in path.parents or path in other.parents


def stage_paths_failures(*, root, control_dir, staging) -> list:
    """Every way a stage host's paths could reach into production.

    A stage project is only isolated if its *paths* are too: a stage host that
    kept `--root /opt/apps/stockspotter` would move the production checkout and
    read the production `.env` while merely naming its containers differently.
    """
    problems = []
    for name, path in (("root", root), ("control dir", control_dir), ("staging", staging)):
        if _inside(path, PRODUCTION_ROOT):
            problems.append(f"{name} {path} is inside the production checkout "
                            f"{PRODUCTION_ROOT}")
    if Path(control_dir) == DEFAULT_STORE:
        problems.append(f"control dir {control_dir} is the production controller's own store")
    if Path(staging) == Path("/var/tmp/stockspotter-candidate"):
        problems.append(f"staging {staging} is the production candidate staging directory")
    if Path(root) == Path(control_dir):
        problems.append("root and control dir must not be the same directory")
    return problems


def stage_recipe_failures(compose, project) -> list:
    """Every way a stage compose document could stop being isolated.

    Enforced on the document the adapter is about to hand to `docker compose`,
    so a stage recreation cannot publish a port, read the host's real `.env`,
    reach the internet, or carry a value that is not visibly synthetic.
    """
    problems = []
    if compose.get("name") != project:
        problems.append(f"document names project {compose.get('name')!r}, not {project!r}")
    network = (compose.get("networks") or {}).get("default") or {}
    if network.get("internal") is not True:
        problems.append("the stage network is not internal: the stack could reach the "
                        "real broker or the real market-data feed")
    if network.get("name") not in (None, f"{project}_default"):
        problems.append(f"the stage network is named {network.get('name')!r}")
    for role, service in sorted((compose.get("services") or {}).items()):
        if service.get("ports"):
            problems.append(f"{role}: publishes {service['ports']}; a stage stack must be "
                            "unreachable from the host")
        if service.get("env_file"):
            problems.append(f"{role}: references an env_file; the production .env must "
                            "never reach a stage stack")
        if service.get("container_name"):
            problems.append(f"{role}: pins a container name")
        for entry in service.get("environment") or []:
            name, _, value = str(entry).partition("=")
            if _secretish(name) and STAGE_MARKER not in value:
                problems.append(f"{role}: {name} carries no stage marker; it may be a "
                                "real credential")
    return problems


# ---------------------------------------------------------------------------
# The adapter
# ---------------------------------------------------------------------------
@dataclass
class LinuxHost:
    """The `Host` protocol, implemented against the real VPS."""

    root: Path = Path("/opt/apps/stockspotter")
    controller_root: Path = Path("/opt/stockspotter-controller")
    control_dir: Path = Path.home() / ".stockspotter-deploy"
    source_repo: Optional[Path] = None          # defaults to `root`
    staging: Path = Path("/var/tmp/stockspotter-candidate")
    # The compose project this host drives. Defaults to the real one and must
    # be either exactly that or a validated `stockspotter-stage-*` namespace,
    # so the same Transaction/Host proof can be driven on an isolated stack
    # without any chance of touching `stockspotter-vps`.
    project: str = PROJECT
    runner: Runner = subprocess_runner
    files: Files = field(default_factory=Files)
    locker: Optional[Callable] = None
    build_locker: Optional[Callable] = None
    policy: Optional[Policy] = None
    clock: Callable[[], datetime] = lambda: datetime.now(timezone.utc)
    sleeper: Callable[[float], None] = time_module.sleep
    dwell_iterations: int = 3
    dwell_seconds: int = 20
    window_minutes_required: int = 45
    builder: str = "stockspotter-ui-guarded-20261012"
    builder_memory_bytes: int = 1024 * 1024 * 1024
    # The CPU bound the existing builder was provisioned with: one CPU in the
    # cgroup quota form. Verified exactly, and recorded in every build receipt.
    builder_cpu_quota: int = 100_000
    builder_cpu_period: int = 100_000
    free_bytes_required: int = 10 * 1024 * 1024 * 1024
    compose_timeout: int = 180
    evidence: dict = field(default_factory=dict)
    # Set by the controller for the duration of one promotion: the hard stop,
    # and the commit whose liveness that hard stop applies to.
    deadline: Optional[datetime] = None
    candidate_source: Optional[str] = None
    # Set only by `rehearse`: fails the candidate's proof on purpose so the
    # transaction performs a real rollback and the evidence is real.
    force_rollback: bool = False

    def __post_init__(self):
        self.root = Path(self.root)
        self.controller_root = Path(self.controller_root)
        self.control_dir = Path(self.control_dir)
        self.source_repo = Path(self.source_repo or self.root)
        self.staging = Path(self.staging)
        self.project = str(self.project)
        if self.project != PROJECT and not STAGE_PROJECT.fullmatch(self.project):
            raise Held(
                f"compose project {self.project!r} is neither the production project "
                f"{PROJECT!r} nor a validated isolated namespace "
                "(stockspotter-stage-<name>)"
            )
        if self.project != PROJECT:
            problems = stage_paths_failures(root=self.root, control_dir=self.control_dir,
                                            staging=self.staging)
            if problems:
                raise Held(f"stage project {self.project}: " + "; ".join(problems))
        self.containers = {role: f"{self.project}-{role}-1" for role in ROLES}
        if self.locker is None:
            self.locker = Flock(self.root / LOCK_PATH, sleeper=self.sleeper)
        if self.build_locker is None:
            # Short wait on purpose: a two-minute watch tick that finds a build
            # already running must defer and come back, not queue up behind it.
            self.build_locker = Flock(self.root / BUILD_LOCK_PATH, seconds=5,
                                      sleeper=self.sleeper)
        self._locked = False
        self._build_locked = False

    @property
    def stage(self) -> bool:
        """Is this host driving an isolated namespace rather than production?"""
        return self.project != PROJECT

    # -- command plumbing ---------------------------------------------------
    def _run(self, argv, *, cwd=None, stdin=None, timeout=120, allow=(0,), quiet=False):
        try:
            result = self.runner(tuple(argv), cwd=str(cwd or self.root), stdin=stdin,
                                 timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise Deferred(f"{argv[0]} timed out after {timeout}s") from error
        except OSError as error:
            raise Deferred(f"{argv[0]} could not be executed: {error}") from error
        if result.code not in allow:
            # `quiet` marks commands whose output may carry credentials (anything
            # executed inside a container, and `compose config`). Those report
            # the exit status only.
            detail = "" if quiet else " " + " ".join((result.err or result.out).split())[:200]
            raise Deferred(f"{' '.join(argv[:3])} exited {result.code}:{detail}")
        return result

    def _git(self, *args, repo=None, allow=(0,)):
        return self._run(["git", "-C", str(repo or self.root), *args], allow=allow)

    def _in_container(self, role, argv, *, stdin=None, timeout=120, allow=(0,)):
        """Run something inside a running role container.

        `docker exec` rather than `docker compose exec` on purpose: a probe must
        not depend on a compose file resolving, so `inspect` works before any
        recipe exists and no probe can be influenced by the compose file in the
        checkout being deployed.
        """
        return self._run(["docker", "exec", "-i", self.containers[role], *argv],
                         stdin=stdin, timeout=timeout, allow=allow, quiet=True)

    def _compose_recipe(self, path, *args, stdin=None, timeout=120, allow=(0,)):
        """Compose, driven by the private resolved recipe and nothing else."""
        return self._run(
            ["docker", "compose", "-p", self.project, "-f", str(path), *args],
            stdin=stdin, timeout=timeout, allow=allow, quiet=True,
        )

    def _require_lock(self, action):
        if not self._locked:
            raise Held(f"{action} attempted without the deployment lock")

    def _require_policy(self, action) -> Policy:
        if self.policy is None:
            raise Held(f"{action} requires an adopted policy")
        return self.policy

    # -- Host: serialization ------------------------------------------------
    @contextmanager
    def lock(self):
        with self.locker():
            self._locked = True
            try:
                yield
            finally:
                self._locked = False

    @contextmanager
    def build_lock(self):
        """Serialize *builds* against each other, not against recreation.

        The two-minute watch and a manual `prepare` must never build the same
        five images concurrently on a 16 GB-free host, and the second one to
        arrive must defer rather than queue: `Flock` here raises `Deferred`
        after a short wait, which is the watch timer's ordinary "nothing to do
        this tick" outcome. This is a different lock from the deployment one on
        purpose -- holding the deployment lock across an hour-long build would
        block the rollback path this controller exists to guarantee.
        """
        if self._locked:
            raise Held("the build lock must not be taken under the deployment lock")
        with self.build_locker():
            self._build_locked = True
            try:
                yield
            finally:
                self._build_locked = False

    # -- Host: git ----------------------------------------------------------
    def tip(self) -> str:
        """`origin/master`, freshly fetched. Never merges into the checkout."""
        self._git("fetch", "--quiet", "origin", "master")
        return _sha(self._git("rev-parse", "FETCH_HEAD").out, "origin/master tip")

    def ancestor(self, previous: str, candidate: str) -> bool:
        result = self._git("merge-base", "--is-ancestor", source_sha(previous),
                           source_sha(candidate), allow=(0, 1))
        return result.code == 0

    def ci_passed(self, source: str) -> bool:
        """The preserved exact-SHA gate, from the immutable installation.

        Deliberately not the copy in the checkout being deployed: a gate whose
        implementation ships with the candidate is not an independent gate
        (ops/vps/README.md's own trust-boundary note). Absent installation is
        fail-closed, with no fallback.
        """
        guard = self.controller_root / "deploy_guard.py"
        if not self.files.exists(guard):
            raise Held(f"controller CI gate is not installed at {guard}")
        result = self._run(["python3", str(guard), "--commit", source_sha(source),
                            "--branch", "master"], timeout=180, allow=(0, 1))
        lines = [line for line in result.out.splitlines() if line.strip()]
        self.evidence["server_ci_gate"] = lines[-1][:300] if lines else ""
        self.evidence["android_gate"] = (
            "not-evaluated-by-this-controller; a skipped mobile job is not an Android pass"
        )
        return result.code == 0

    def _head(self) -> str:
        return _sha(self._git("rev-parse", "HEAD").out, "checkout HEAD")

    def _branch(self) -> str:
        branch = self._git("rev-parse", "--abbrev-ref", "HEAD").out.strip()
        if not branch or branch == "HEAD":
            raise Held("production checkout is detached; no branch to record")
        return branch

    def _clean(self) -> bool:
        return not self._git("status", "--porcelain", "--untracked-files=all").out.strip()

    def _frozen_parity(self, source: str) -> None:
        """Would this commit change frozen tracked content? Asked before moving.

        Blob object ids are compared rather than content: no file bytes cross a
        pipe, and the answer is available without touching the working tree, so
        a candidate that would rewrite the preregistration or the qualify
        session script is refused *before* the checkout moves rather than
        after. `.env` is untracked and cannot be carried by a commit at all.
        """
        if self.policy is None:
            return
        for name in FROZEN_PATHS:
            if name == ".env":
                continue
            current = self._git("hash-object", "--", name, allow=(0, 1))
            candidate = self._git("rev-parse", f"{source}:{name}", allow=(0, 1))
            if current.code != 0 or candidate.code != 0:
                raise Rejected(f"frozen file cannot be compared in the candidate: {name}")
            if current.out.strip() != candidate.out.strip():
                raise Rejected(f"candidate would change frozen content: {name}")

    # -- Host: the legacy deployment path ----------------------------------
    def legacy_marker(self) -> str:
        """`ops/vps/.deployed-commit`, READ-ONLY. Never written here.

        This is `deploy.sh`'s state file: it rebuilds whenever
        `HEAD != <this value>`, from the *base* compose file. Writing the
        running build's commit into it would make the two-minute legacy timer
        rebuild the checkout's commit and destroy the live overlay
        configuration. Adoption records it; nothing changes it.
        """
        return (self.files.read_text(self.root / LEGACY_MARKER_PATH) or "").strip()

    def legacy_timer(self) -> dict:
        """Is the two-minute legacy deploy timer armed?

        Inability to answer is not "no": `systemctl` failing to run at all is
        reported as unknown, and the promotion gate treats unknown as armed.
        """
        status = {}
        for query in ("is-enabled", "is-active"):
            try:
                result = self._run(["systemctl", query, LEGACY_TIMER], timeout=30,
                                   allow=(0, 1, 2, 3, 4))
            except Deferred:
                status[query] = "unknown"
                continue
            status[query] = (result.out.strip().splitlines() or [""])[-1].strip() or "unknown"
        return status

    def _require_legacy_timer_disarmed(self, action) -> dict:
        status = self.legacy_timer()
        if (status.get("is-enabled") not in ("disabled", "masked", "not-found")
                or status.get("is-active") not in ("inactive", "failed", "not-found")):
            raise Deferred(
                f"{action} refused while {LEGACY_TIMER} is "
                f"{status.get('is-enabled')}/{status.get('is-active')}: it runs deploy.sh "
                "every two minutes, which would rebuild the checkout from the base compose "
                "file and discard the live effective configuration. Disable and stop it "
                f"first (sudo systemctl disable --now {LEGACY_TIMER})."
            )
        return status

    # -- Host: runtime observation -----------------------------------------
    def _containers(self) -> dict:
        """One `docker inspect` for the five role containers, in role order."""
        names = [self.containers[role] for role in sorted(ROLES)]
        result = self._run(["docker", "inspect", "--format", "{{json .}}", *names],
                           timeout=60)
        lines = [line for line in result.out.splitlines() if line.strip()]
        if len(lines) != len(names):
            raise Deferred("docker inspect did not describe all five services")
        try:
            documents = [json.loads(line) for line in lines]
        except ValueError as error:
            raise Deferred("docker inspect returned malformed JSON") from error
        return dict(zip(sorted(ROLES), documents))

    @staticmethod
    def _image(document) -> str:
        image = document.get("Image")
        if not isinstance(image, str) or not IMAGE_ID.fullmatch(image):
            raise Held("a running service has no immutable image identity")
        return image

    @staticmethod
    def _labels(document) -> dict:
        return ((document.get("Config") or {}).get("Labels") or {})

    @staticmethod
    def _flags(document) -> dict:
        values = _env_map((document.get("Config") or {}).get("Env"))
        return {name: values.get(name) for name in PARITY_FLAGS}

    def _image_env(self, image) -> dict:
        """What the *image* itself provides, so an overlay's own environment can
        be told apart from the image's defaults."""
        result = self._run(["docker", "image", "inspect", "--format",
                            "{{json .Config.Env}}", image], timeout=60, quiet=True)
        try:
            entries = json.loads(result.out or "null")
        except ValueError as error:
            raise Deferred(f"docker image inspect returned malformed env for {image}") from error
        return _env_map(entries)

    def _dotenv(self) -> dict:
        pairs = self.files.env_pairs(self.root / ".env")
        if pairs is None:
            raise Held(f"{self.root / '.env'} is unreadable; refusing to reason about secrets")
        return pairs

    def _role_env(self, role, document, image_env, dotenv, declared=None) -> dict:
        """Split a container's effective environment into reproducible parts.

        A container's environment is `image ENV` overlaid with compose's
        `env_file` and `environment:`. To reproduce it without ever copying a
        credential, each observed variable is attributed to one of:

        * `dotenv`  -- supplied by the host's own `.env` through `env_file`;
        * `literal` -- supplied by the overlay's `environment:`, reproduced with
          its real (non-secret) value in the private recipe;
        * `image`   -- supplied by the image itself, on the validated allowlist.

        A variable that must be literal but may be a credential is refused
        rather than written to a controller file. `declared` is the set of names
        the adopted recipe already attributes to `literal`, which keeps the
        attribution stable when a rebuilt image starts baking the same value.
        """
        observed = _env_map((document.get("Config") or {}).get("Env"))
        secret_digests = {digest for name, digest in dotenv.items() if _secretish(name)}
        # `env_file` is in use when every credential `.env` carries reaches the
        # container unchanged. Non-secret variables may legitimately be
        # overridden by the overlay, and show up as `literal` below.
        secret_names = [name for name in dotenv if _secretish(name)]
        env_file = bool(dotenv) and any(name in observed for name in dotenv) and all(
            _digest_text(observed.get(name)) == dotenv[name]
            for name in secret_names if name in observed
        ) and all(name in observed for name in secret_names)
        dropped = sorted(name for name in dotenv if name not in observed) if env_file else []
        if dropped:
            raise Held(
                f"{role}: env_file cannot reproduce a container that lacks "
                f"{', '.join(dropped)}; the live launch path and .env disagree"
            )
        covered, literal, provided, identity = {}, {}, {}, {}
        unreproducible = []
        for name, value in sorted(observed.items()):
            if name in IDENTITY_ENV:
                identity[name] = _digest_text(value)
                continue
            if env_file and name in dotenv and _digest_text(value) == dotenv[name]:
                covered[name] = dotenv[name]
                continue
            if (declared is None or name not in declared) and name in IMAGE_ENV_ALLOWLIST \
                    and image_env.get(name) == value:
                provided[name] = _digest_text(value)
                continue
            if _secretish(name) or _digest_text(value) in secret_digests:
                unreproducible.append(name)
                continue
            literal[name] = value
        if unreproducible:
            raise Held(
                f"{role}: {', '.join(sorted(unreproducible))} would have to be copied into a "
                "controller file to be reproduced. Move it into the host's .env so compose's "
                "env_file supplies it, then adopt again."
            )
        unexpected = sorted(name for name in provided if name not in IMAGE_ENV_ALLOWLIST)
        if unexpected:  # pragma: no cover - guarded by the allowlist test above
            raise Held(f"{role}: image provides unexpected variables {unexpected}")
        return {"env_file": env_file, "dotenv": covered, "literal": literal,
                "image": provided, "identity": identity}

    def _effective(self, documents, declared=None) -> dict:
        """Per-role effective environment, for every role, in one pass."""
        dotenv = self._dotenv()
        cache = {}
        effective = {}
        for role, document in sorted(documents.items()):
            image = self._image(document)
            if image not in cache:
                cache[image] = self._image_env(image)
            effective[role] = self._role_env(role, document, cache[image], dotenv,
                                             declared=(declared or {}).get(role))
        return effective

    @staticmethod
    def _env_binding(effective) -> dict:
        """The part of the effective environment `env_hash` binds.

        Values never appear: `literal` is reduced to digests here even though
        the recipe holds the real values, so no hash input is a cleartext
        configuration value. The image-provided part is excluded on purpose --
        rebuilding an image legitimately changes `PATH` or `PYTHON_VERSION` --
        and is recorded separately as drift instead.
        """
        return {
            role: {
                "env_file": values["env_file"],
                "dotenv": dict(values["dotenv"]),
                "literal": {name: _digest_text(value)
                            for name, value in values["literal"].items()},
            }
            for role, values in sorted(effective.items())
        }

    @staticmethod
    def _config_digest(document) -> str:
        """Full per-role config digest for the receipts. Never the content: a
        container's environment carries ALPACA_* and STOCKSPOTTER_API_TOKEN."""
        config = document.get("Config") or {}
        host_config = document.get("HostConfig") or {}
        environment = sorted(
            f"{name}={_digest_text(value)}"
            for name, value in _env_map(config.get("Env")).items()
        )
        return _digest_value({
            "env": environment,
            "cmd": config.get("Cmd"),
            "entrypoint": config.get("Entrypoint"),
            "user": config.get("User"),
            "workingDir": config.get("WorkingDir"),
            "exposedPorts": sorted((config.get("ExposedPorts") or {})),
            "healthcheck": bool(config.get("Healthcheck")),
            "binds": sorted(host_config.get("Binds") or []),
            "portBindings": host_config.get("PortBindings"),
            "restartPolicy": host_config.get("RestartPolicy"),
            "logConfig": host_config.get("LogConfig"),
            "networkMode": host_config.get("NetworkMode"),
            "memory": host_config.get("Memory"),
        })

    def _reject_unsafe_runtime(self, documents: dict) -> None:
        """Bind shadowing, public port exposure, unbounded logs, restart policy.

        These are invariants rather than equalities: a candidate commit may
        legitimately change container configuration, but it may not shadow the
        compiled binary with a bind mount, publish a port off loopback, drop
        log rotation or drop `unless-stopped`.

        The last two are waivable for the *adopted baseline image only*, by an
        explicit recorded exemption: the live stack predates this controller,
        and a stricter candidate rule must not make adopting reality
        impossible. Bind shadowing, loopback-only publication and the
        order-flow kill switch are never waivable.
        """
        for role, document in sorted(documents.items()):
            image = self._image(document)
            for mount in document.get("Mounts") or []:
                if not isinstance(mount, dict):
                    raise Rejected(f"{role}: malformed mount record")
                destination = mount.get("Destination")
                if mount.get("Type") == "bind" and destination not in ALLOWED_BINDS[role]:
                    raise Rejected(f"{role}: unexpected bind mount at {destination}")
                if destination in SHADOWED_PATHS:
                    raise Rejected(f"{role}: a mount shadows the compiled binary at {destination}")
            host_config = document.get("HostConfig") or {}
            for port, bindings in (host_config.get("PortBindings") or {}).items():
                for binding in bindings or []:
                    if (binding or {}).get("HostIp") not in ("127.0.0.1", "::1"):
                        raise Rejected(f"{role}: {port} is published off loopback")
            log_config = host_config.get("LogConfig") or {}
            if not (log_config.get("Config") or {}).get("max-size"):
                if not (self.policy and self.policy.exempt(role, "unbounded-logs", image)):
                    raise Rejected(f"{role}: logging is unbounded")
            if (host_config.get("RestartPolicy") or {}).get("Name") != "unless-stopped":
                if not (self.policy and self.policy.exempt(role, "restart-policy", image)):
                    raise Rejected(f"{role}: restart policy is not unless-stopped")
            flags = self._flags(document)
            for name in MUST_BE_OFF:
                if flags.get(name) not in OFF_VALUES:
                    raise Rejected(f"{role}: {name} must stay off, found {flags[name]!r}")
            if self.policy is not None and flags != self.policy.flags.get(role):
                raise Rejected(f"{role}: effective flags differ from the adopted policy")

    def _ws_commit(self) -> str:
        # A failing probe is a failed runtime proof, not a transient host
        # condition: it is classified `Rejected` so the transaction restores
        # rather than leaving a half-promoted stack for the next attempt.
        result = self._in_container("qualify", ["python", "-"], stdin=COMPLETENESS_PROBE,
                                    timeout=90, allow=(0, 1))
        if result.code != 0:
            raise Rejected("the running ws build did not answer the completeness probe")
        try:
            commit = json.loads(result.out).get("commit")
        except ValueError as error:
            raise Rejected("completeness probe returned malformed JSON") from error
        if commit is None:
            raise Rejected("the running ws build carries no commit stamp")
        return _sha(commit, "compiled ws commit")

    def _artifact(self, role) -> Optional[str]:
        """SHA-256 of the role's single baked artifact, read inside the image.

        This is the identity a label cannot forge. A role whose image has no
        `sha256sum` reports `None`, which is recorded honestly as "not
        measurable here" rather than guessed.
        """
        result = self._in_container(role, ["sha256sum", ARTIFACT_PATHS[role]],
                                    timeout=120, allow=tuple(range(0, 128)))
        if result.code != 0:
            return None
        token = (result.out or "").split()
        return token[0] if token and DIGEST.fullmatch(token[0]) else None

    def artifacts(self) -> dict:
        return {role: self._artifact(role) for role in sorted(ROLES)}

    def _verify_artifacts(self, documents) -> dict:
        """Compare the baked artifacts against what adoption pinned.

        Enforced per role exactly while that role runs the adopted baseline
        image, which is what makes a *rollback* prove it really restored the
        baseline bytes (and not merely a tag that points somewhere else). A
        candidate image is expected to differ, so it is recorded, not compared.
        """
        observed = self.artifacts()
        if self.policy is None:
            return observed
        for role in sorted(ROLES):
            expected = self.policy.artifacts.get(role)
            if expected is None:
                continue
            if self.policy.baseline_images.get(role) != self._image(documents[role]):
                continue
            if observed[role] != expected:
                raise Rejected(
                    f"{role}: baked artifact {ARTIFACT_PATHS[role]} is "
                    f"{observed[role] or 'unreadable'}, not the adopted {expected}"
                )
        return observed

    def snapshot(self) -> dict:
        documents = self._containers()
        self._reject_unsafe_runtime(documents)
        # Refused here, before any journal exists, for the same reason
        # deploy.sh refuses it ("checkout has local changes"): a dirty
        # production checkout is not a baseline anything may be built on, and
        # discovering it later would leave a transaction to reconcile.
        if not self._clean():
            raise Rejected("production checkout has local changes")
        return {
            "source": self._ws_commit(),
            "checkout_source": self._head(),
            "branch": self._branch(),
            "images": {role: self._image(document) for role, document in documents.items()},
            "env_hash": self._env_digest(documents),
        }

    def flags(self) -> dict:
        """The effective non-secret policy flags of every running service."""
        return {role: self._flags(document)
                for role, document in self._containers().items()}

    def _declared_literals(self) -> Optional[dict]:
        recipe = self._recipe_record(required=False)
        if recipe is None:
            return None
        return {role: set(values.get("literal") or {})
                for role, values in (recipe.get("effective") or {}).items()}

    def _env_digest(self, documents=None) -> str:
        """The operator-controlled environment, in full, by digest.

        Binds, per role, *every* effective application environment variable --
        `.env`-supplied and overlay-supplied, secret or not, values reduced to
        SHA-256 -- plus the frozen-file digests. Not a flag subset: the whole
        `.env` name/value-digest map and the whole overlay `environment:` block
        are inputs here, so provider, auth or feature-flag drift changes this
        hash.

        Two deliberate exclusions, both recorded as drift in the receipts
        rather than forgiven: build identity (`STOCKSPOTTER_COMMIT`, different
        in every candidate by design) and image-provided variables on
        `IMAGE_ENV_ALLOWLIST` (a rebuilt base image legitimately moves `PATH`).
        An image-provided variable that is *not* on that allowlist is bound, so
        it fails the promotion proof instead of slipping through.

        Once a baseline recipe exists, the binding is read from the *declared*
        recipe rather than recomputed from whatever containers happen to be
        running. That is not a weakening -- the recipe's binding is exactly the
        observed baseline's, captured at adoption, and conformance of the live
        containers to it is re-proved strictly in `up()` and on every `health()`
        iteration. It matters because schema 2 uses `env_digest()` as the
        *pre-restore* guard: if this hash tracked the live containers, a
        candidate whose environment drifted would make the guard disagree with
        the baseline and the transaction would end `Held` with the bad
        candidate still running, instead of rolling back. Drift in what is
        running must be a `Rejected` proof (rollback), and drift in what the
        operator configured must be this hash.
        """
        frozen = {}
        for name in FROZEN_PATHS:
            digest = self.files.digest(self.root / name)
            if digest is None:
                raise Held(f"frozen file is missing: {name}")
            frozen[name] = digest
        record = self._recipe_record(required=False)
        if record is not None:
            binding = record["env_binding"]
        else:
            documents = documents if documents is not None else self._containers()
            binding = self._env_binding(self._effective(documents))
        return _digest_value({"version": 2, "frozen": frozen, "env": binding})

    def env_digest(self) -> str:
        return self._env_digest()

    def marker(self) -> str:
        """This controller's own private marker.

        Deliberately NOT `ops/vps/.deployed-commit`: see `legacy_marker`. A
        missing or malformed marker reads as an invalid identity, which the
        transaction treats as "reconcile first". There is no legacy fallback
        and no legacy write.
        """
        return (self.files.read_text(self.control_dir / CONTROLLER_MARKER_NAME) or "").strip()

    def write_marker(self, source: str) -> None:
        self._require_lock("writing the deployed marker")
        self._require_private_control_dir()
        self.files.write_private(self.control_dir / CONTROLLER_MARKER_NAME,
                                 source_sha(source) + "\n")

    def _require_private_control_dir(self) -> None:
        self.files.make_private_dir(self.control_dir)
        if not self.files.is_private(self.control_dir):
            raise Held(f"control directory {self.control_dir} is not private (0700, owned)")

    # -- the private resolved recipe ---------------------------------------
    def _recipe_record(self, required=True) -> Optional[dict]:
        """The immutable baseline recipe captured at adoption."""
        text = self.files.read_text(self.control_dir / BASELINE_RECIPE_NAME)
        if text is None:
            if required:
                raise Held(
                    f"no baseline recipe at {self.control_dir / BASELINE_RECIPE_NAME}; "
                    "adopt first -- recreating from the base compose file alone would "
                    "discard the live effective configuration"
                )
            return None
        try:
            record = json.loads(text)
        except ValueError as error:
            raise Held("baseline recipe is not valid JSON") from error
        if (not isinstance(record, dict) or record.get("version") != 1
                or not isinstance(record.get("compose"), dict)
                or not isinstance(record.get("effective"), dict)
                or not isinstance(record.get("env_binding"), dict)
                or not isinstance(record.get("images"), dict)
                or not isinstance(record.get("policy_digest"), str)):
            raise Held("baseline recipe record is malformed")
        if set(record["env_binding"]) != ROLES:
            raise Held("baseline recipe does not bind all five services' environments")
        if set((record["compose"].get("services") or {})) != ROLES:
            raise Held("baseline recipe does not describe exactly the five services")
        image_set(record["images"])
        if required:
            policy = self._require_policy("using the baseline recipe")
            if record["policy_digest"] != policy.digest():
                raise Held(
                    "baseline recipe was captured under a different adoption policy; "
                    "adopt again to recapture it"
                )
        return record

    def capture_recipe(self, documents=None) -> dict:
        """Derive the private full-fidelity compose recipe from reality.

        This is the heart of the rollback guarantee. The live stack was created
        by overlay compose files outside this repository, so the only authority
        on its effective configuration is the running containers themselves.
        Everything needed to recreate them -- exact image IDs, per-role
        environment, binds with their real sources, loopback port publication,
        logging, restart policy, command, user, working directory -- is read
        from `docker inspect` and written as a standalone compose document.

        It is standalone on purpose: recreation never merges the base compose
        file, so no commit being promoted can change how the stack is launched,
        and no shared mutable tag is written for the legacy path to pick up.
        """
        documents = documents if documents is not None else self._containers()
        self._reject_unsafe_runtime(documents)
        effective = self._effective(documents)
        services, inventory = {}, {}
        for role, document in sorted(documents.items()):
            labels = self._labels(document)
            if labels.get(COMPOSE_PROJECT_LABEL) != self.project:
                raise Held(
                    f"{role}: container is in compose project "
                    f"{labels.get(COMPOSE_PROJECT_LABEL)!r}, not {self.project!r}; recreating "
                    "would create a second stack beside the live one"
                )
            if labels.get(COMPOSE_SERVICE_LABEL) != role:
                raise Held(f"{role}: container's compose service label is "
                           f"{labels.get(COMPOSE_SERVICE_LABEL)!r}")
            host_config = document.get("HostConfig") or {}
            network = host_config.get("NetworkMode")
            if network != f"{self.project}_default":
                raise Held(
                    f"{role}: network mode {network!r} is not this project's default network; "
                    "this controller cannot faithfully reproduce it"
                )
            config = document.get("Config") or {}
            image = self._image(document)
            service = {
                "image": image,
                "restart": (host_config.get("RestartPolicy") or {}).get("Name") or "no",
                "logging": {
                    "driver": (host_config.get("LogConfig") or {}).get("Type") or "json-file",
                    "options": dict((host_config.get("LogConfig") or {}).get("Config") or {}),
                },
            }
            literal = effective[role]["literal"]
            if literal:
                service["environment"] = [f"{name}={value}"
                                          for name, value in sorted(literal.items())]
            if effective[role]["env_file"]:
                # `as_posix()`, not `str()`: the recipe is a document consumed
                # by a Linux docker daemon, so its paths must be POSIX whatever
                # platform happened to render it.
                service["env_file"] = [(self.root / ".env").as_posix()]
            ports = []
            for port, bindings in sorted((host_config.get("PortBindings") or {}).items()):
                container_port, _, protocol = port.partition("/")
                for binding in bindings or []:
                    host_ip = (binding or {}).get("HostIp") or "127.0.0.1"
                    host_port = (binding or {}).get("HostPort") or container_port
                    ports.append(f"{host_ip}:{host_port}:{container_port}"
                                 + (f"/{protocol}" if protocol else ""))
            if ports:
                service["ports"] = ports
            volumes = []
            for bind in host_config.get("Binds") or []:
                source, destination, mode = _bind_parts(bind)
                if destination not in ALLOWED_BINDS[role]:
                    raise Rejected(f"{role}: unexpected bind mount at {destination}")
                if not source.startswith("/"):
                    raise Held(f"{role}: bind source {source!r} is not an absolute path")
                volumes.append(f"{source}:{destination}" + (f":{mode}" if mode else ""))
            if volumes:
                service["volumes"] = sorted(volumes)
            if config.get("Cmd"):
                service["command"] = list(config["Cmd"])
            if config.get("Entrypoint"):
                service["entrypoint"] = list(config["Entrypoint"])
            if config.get("User"):
                service["user"] = config["User"]
            if config.get("WorkingDir"):
                service["working_dir"] = config["WorkingDir"]
            if DEPENDS_ON[role]:
                service["depends_on"] = list(DEPENDS_ON[role])
            services[role] = service
            inventory[role] = {
                "image": image,
                "compose_config_files": labels.get(COMPOSE_CONFIG_FILES_LABEL),
                "compose_working_dir": labels.get(COMPOSE_WORKING_DIR_LABEL),
                "network": network,
                "healthcheck": bool(config.get("Healthcheck")),
                "config_digest": self._config_digest(document),
            }
        compose_document = {"name": self.project, "services": services}
        if self.stage:
            # A stage project's own network, declared `internal` so the stack
            # has no egress at all: whatever a stage candidate does, it cannot
            # reach the real broker or the real market-data feed.
            compose_document["networks"] = {
                "default": {"name": f"{self.project}_default", "internal": True}}
            self._require_stage_isolation(compose_document)
        record = {
            "version": 1,
            "captured_at": self.clock().isoformat(),
            "project": self.project,
            "provenance": "observed-not-deployed",
            "images": {role: service["image"] for role, service in services.items()},
            "inventory": inventory,
            "effective": {role: {"env_file": values["env_file"],
                                 "dotenv": sorted(values["dotenv"]),
                                 "literal": dict(values["literal"]),
                                 "image": sorted(values["image"]),
                                 "identity": sorted(values["identity"])}
                          for role, values in sorted(effective.items())},
            "env_binding": self._env_binding(effective),
            "policy_digest": self._require_policy("capturing the baseline recipe").digest(),
            # `name: <project>` keeps compose's own container naming
            # (`stockspotter-vps-<role>-1`) identical to what is running, so a
            # recreation replaces those containers instead of adding new ones.
            "compose": compose_document,
        }
        self._require_private_control_dir()
        self.files.write_private(self.control_dir / BASELINE_RECIPE_NAME,
                                 json.dumps(record, indent=2, sort_keys=True))
        return record

    def _target_recipe(self, images) -> dict:
        """The baseline recipe with exactly these five image IDs substituted.

        Everything else -- the overlay's effective environment, the binds, the
        loopback ports, the command -- is carried over unchanged, which is what
        makes a promotion preserve the live configuration and a rollback
        restore it.
        """
        record = self._recipe_record()
        target = json.loads(json.dumps(record["compose"]))
        for role, image in sorted(images.items()):
            target["services"][role]["image"] = image
        if self.stage:
            self._require_stage_isolation(target)
        return target

    def _require_stage_isolation(self, compose) -> None:
        """A stage recreation is refused unless the document is still isolated.

        Checked on the document itself rather than trusted from the driver that
        generated it, and checked again on every recreation: this is what makes
        "the same Transaction and the same Host proof, on a namespace that
        cannot touch production" a property of the adapter instead of a promise
        in a runbook.
        """
        problems = stage_recipe_failures(compose, self.project)
        problems += stage_paths_failures(root=self.root, control_dir=self.control_dir,
                                         staging=self.staging)
        if problems:
            raise Rejected(f"stage project {self.project} is not isolated: "
                           + "; ".join(problems))

    def _write_active_recipe(self, compose) -> Path:
        self._require_private_control_dir()
        path = self.control_dir / ACTIVE_RECIPE_NAME
        self.files.write_private(path, json.dumps(compose, indent=2, sort_keys=True))
        return path

    def _base_compose(self) -> dict:
        """`ops/vps/docker-compose.yml` as the checkout currently resolves it.

        Read with `--no-interpolate` so no `${...}` is expanded from `.env` or
        the environment: this is parsed for structure only, and nothing from it
        is ever used to recreate anything.
        """
        result = self._run(["docker", "compose", "-p", self.project, "-f", BASE_COMPOSE_FILE,
                            "config", "--format", "json", "--no-interpolate"],
                           timeout=120, allow=(0, 1), quiet=True)
        # Classified `Rejected`, not `Deferred`: this is only ever read to
        # preflight a *candidate*, so a compose file that does not resolve is a
        # defect in the commit being promoted, and the transaction must restore
        # rather than retry it later.
        if result.code != 0:
            raise Rejected(f"the candidate's {BASE_COMPOSE_FILE} does not resolve")
        try:
            document = json.loads(result.out or "null")
        except ValueError as error:
            raise Rejected("docker compose config returned malformed JSON") from error
        if not isinstance(document, dict) or not isinstance(document.get("services"), dict):
            raise Rejected("docker compose config described no services")
        return document

    @staticmethod
    def _base_ports(service) -> set:
        published = set()
        for entry in service.get("ports") or []:
            if isinstance(entry, dict):
                host_port = str(entry.get("published") or "")
                target = str(entry.get("target") or "")
            else:
                pieces = str(entry).split(":")
                host_port = pieces[-2] if len(pieces) >= 2 else ""
                target = pieces[-1].partition("/")[0]
            if host_port:
                published.add(f"{host_port}:{target}")
        return published

    @staticmethod
    def _base_binds(service) -> set:
        destinations = set()
        for entry in service.get("volumes") or []:
            if isinstance(entry, dict):
                if entry.get("type") in (None, "bind") and entry.get("target"):
                    destinations.add(str(entry["target"]))
            else:
                pieces = str(entry).split(":")
                if len(pieces) >= 2:
                    destinations.add(pieces[1])
        return destinations

    def _base_compatibility(self, compose) -> dict:
        """Does the commit being promoted still describe this stack?

        The recipe, not the base compose file, is what gets recreated -- so the
        base file cannot silently change the launch. But a candidate that
        *redefines* the contract (a new published port, a new bind, a changed
        command, a new declared variable) must not be promoted onto a recipe
        that predates it: the promotion would quietly ignore the change. Such a
        candidate is rejected with the specific difference named, so the
        operator re-adopts a baseline deliberately.
        """
        base = self._base_compose()
        services = base["services"]
        if set(services) != ROLES:
            raise Rejected(
                f"the candidate's {BASE_COMPOSE_FILE} describes {sorted(services)}, "
                "not the five live services"
            )
        report = {}
        for role in sorted(ROLES):
            service, recipe = services[role], compose["services"][role]
            recipe_ports = set()
            for entry in recipe.get("ports") or []:
                pieces = str(entry).split(":")
                recipe_ports.add(f"{pieces[-2]}:{pieces[-1].partition('/')[0]}")
            base_ports = self._base_ports(service)
            if recipe_ports != base_ports:
                raise Rejected(
                    f"{role}: the candidate publishes {sorted(base_ports) or 'nothing'} "
                    f"but the live stack publishes {sorted(recipe_ports) or 'nothing'}"
                )
            recipe_binds = {_bind_parts(entry)[1] for entry in recipe.get("volumes") or []}
            base_binds = self._base_binds(service)
            if recipe_binds != base_binds:
                raise Rejected(
                    f"{role}: the candidate mounts {sorted(base_binds) or 'nothing'} "
                    f"but the live stack mounts {sorted(recipe_binds) or 'nothing'}"
                )
            if service.get("command") and list(service["command"]) != list(recipe.get("command") or []):
                raise Rejected(f"{role}: the candidate changes the service command")
            declared = {str(entry).split("=", 1)[0] for entry in service.get("environment") or []} \
                if isinstance(service.get("environment"), list) else set(service.get("environment") or {})
            known = set(_env_map(recipe.get("environment")))
            if (compose["services"][role].get("env_file")):
                known |= set(self._dotenv())
            missing = sorted(name for name in declared
                             if name not in known and name not in IDENTITY_ENV)
            if missing:
                raise Rejected(
                    f"{role}: the candidate declares {', '.join(missing)}, which the live "
                    "effective configuration does not carry; adopt a new baseline instead "
                    "of promoting a change the recipe would ignore"
                )
            report[role] = {"published": sorted(recipe_ports), "binds": sorted(recipe_binds),
                            "declared": sorted(declared)}
        return report

    def _verify_recipe_applied(self, compose, documents) -> None:
        """Did the daemon give us exactly the configuration we asked for?

        Without this, the recipe would be an intention. With it, a recreation
        that lost an overlay variable, a bind source or the log rotation is a
        failed proof, which the transaction turns into a rollback.
        """
        failures = conformance_failures(compose, documents, self._dotenv())
        if failures:
            raise Rejected("; ".join(failures[:3]))

    # -- Host: build --------------------------------------------------------
    def boot_id(self) -> str:
        value = (self.files.read_text(BOOT_ID_PATH) or "").strip()
        if not value:
            raise Held("kernel boot id is unavailable; cannot bind a preparation")
        return value

    def _verify_builder(self) -> dict:
        """The existing bounded builder, verified -- never created or edited.

        Both bounds are verified and both are recorded. Memory alone was not
        enough: a builder with a 1 GiB memory cap and no CPU cap can still
        starve the five live containers on a small VPS, and the CPU bound this
        builder was provisioned with is the *quota* form
        (`CpuQuota=100000`/`CpuPeriod=100000`, `NanoCpus=0`, i.e. one CPU), so
        that exact form is what is asserted. A builder bounded some other way
        -- `NanoCpus`, a different quota, or no quota at all -- defers with the
        observed numbers named rather than being silently accepted.
        """
        inspected = self._run(["docker", "buildx", "inspect", self.builder], timeout=60,
                              allow=(0, 1))
        if inspected.code != 0:
            raise Deferred(f"bounded builder {self.builder} is not provisioned")
        if "docker-container" not in inspected.out:
            raise Deferred(f"builder {self.builder} is not the docker-container driver")
        limits = self._run(["docker", "inspect", "--format", BUILDER_LIMITS_FORMAT,
                            f"buildx_buildkit_{self.builder}0"], timeout=60, allow=(0, 1))
        fields = (limits.out or "").split()
        if limits.code != 0 or len(fields) != 4:
            raise Deferred(f"builder {self.builder}'s resource limits are unreadable")
        try:
            memory, nano_cpus, quota, period = (int(field) for field in fields)
        except ValueError:
            raise Deferred(f"builder {self.builder} reported non-numeric resource limits")
        observed = {
            "builder": self.builder,
            "memory_bytes": memory,
            "nano_cpus": nano_cpus,
            "cpu_quota": quota,
            "cpu_period": period,
            "cpus": round(quota / period, 4) if period > 0 else None,
        }
        if memory != self.builder_memory_bytes:
            raise Deferred(
                f"builder {self.builder} is not bounded to {self.builder_memory_bytes} bytes "
                f"(Memory={memory})"
            )
        if nano_cpus != 0 or quota != self.builder_cpu_quota or period != self.builder_cpu_period:
            raise Deferred(
                f"builder {self.builder} is not CPU-bounded in the provisioned quota form "
                f"(expected NanoCpus=0, CpuQuota={self.builder_cpu_quota}, "
                f"CpuPeriod={self.builder_cpu_period}; observed NanoCpus={nano_cpus}, "
                f"CpuQuota={quota}, CpuPeriod={period})"
            )
        return observed

    def _retain_rollback_images(self, documents, source) -> dict:
        """Tag the live images so a restoration uses real retained images.

        Private tags in this controller's own namespace, never a compose
        service tag: `deploy.sh` and `deploy-chart-web.sh` read
        `stockspotter-vps-<role>:latest`, and this controller must not write
        anything the legacy path consumes. Nothing is ever pruned here -- the
        deliberately old protected images stay exactly as they are.
        """
        retained = {}
        for role, document in sorted(documents.items()):
            image = self._image(document)
            tag = f"stockspotter-rollback-{role}:{source}"
            self._run(["docker", "tag", image, tag], timeout=60)
            retained[role] = {"image": image, "tag": tag}
        return retained

    def _materialize_source(self, source: str) -> Path:
        """Tracked-only candidate tree from the exact commit.

        `git archive` carries tracked content at that commit and nothing else:
        no `.git`, no untracked `.env`, no build output. (`.dockerignore`
        excludes `.git` from every build context anyway, which is why the
        frontend's git-derived build identity is `unknown` in every Docker
        image this project has ever produced -- see the design note.)
        """
        directory = self.staging / source
        self.files.make_private_dir(self.staging)
        if self.files.exists(directory):
            self.files.remove_tree(directory, inside=self.staging)
        self.files.make_private_dir(directory)
        archive = self.staging / f"{source}.tar"
        self._git("archive", "--format=tar", "-o", str(archive), source, repo=self.source_repo)
        self._run(["tar", "-xf", str(archive), "-C", str(directory)], timeout=300)
        for forbidden in (".git", ".env"):
            if self.files.exists(directory / forbidden):
                raise Rejected(f"candidate tree contains {forbidden}")
        self.evidence["candidate_archive_sha256"] = self.files.digest(archive)
        return directory

    def provenance(self, documents=None) -> dict:
        """Per-role build provenance, read from the images and the containers.

        Labels alone are not provenance: a label is metadata the builder wrote.
        This records the immutable image ID, the image's creation time and
        repository digests, the compose files the live container was actually
        created from, the project working directory, and the SHA-256 of the
        role's baked artifact read from inside the running container.
        """
        documents = documents if documents is not None else self._containers()
        report = {}
        for role, document in sorted(documents.items()):
            image = self._image(document)
            result = self._run(["docker", "image", "inspect", "--format", "{{json .}}", image],
                               timeout=60, quiet=True)
            try:
                described = json.loads((result.out or "").strip() or "{}")
            except ValueError as error:
                raise Deferred(f"docker image inspect returned malformed JSON for {role}") from error
            labels = (described.get("Config") or {}).get("Labels") or {}
            container_labels = self._labels(document)
            report[role] = {
                "image": image,
                "image_created": described.get("Created"),
                "image_repo_digests": sorted(described.get("RepoDigests") or []),
                "image_revision_label": labels.get(REVISION_LABEL),
                "image_experimental_label": labels.get(EXPERIMENTAL_LABEL),
                "artifact_path": ARTIFACT_PATHS[role],
                "artifact_sha256": self._artifact(role),
                "compose_config_files": container_labels.get(COMPOSE_CONFIG_FILES_LABEL),
                "compose_working_dir": container_labels.get(COMPOSE_WORKING_DIR_LABEL),
            }
        report["compiled_ws_commit"] = self._ws_commit()
        report["served_bundle_sha256"] = _digest_text(self._served_bundle())
        report["decision_binding"] = _digest_value(self.decision_binding(report))
        report["frontend_build_identity"] = (
            "unknown-by-construction: .dockerignore excludes .git from every build context, "
            "so apps/client's git-derived build identity is 'unknown' in every Docker-built "
            "web image, including the live one. Identity is the image ID, the revision label "
            "and the served-bundle digest -- not a compiled git sha."
        )
        return report

    @staticmethod
    def decision_binding(provenance: dict) -> dict:
        """The part of provenance the per-role decisions are bound to.

        Deliberately not the whole report. The compose launch files and project
        working directory are recorded as evidence but excluded here, because
        the first recreation through this controller legitimately replaces the
        untracked overlay paths with the private recipe -- binding them would
        invalidate the decisions as a side effect of the controller doing its
        job. What is bound is what the decisions were actually *about*: the
        immutable image identity of each role, its baked artifact bytes, its
        declared revision, the compiled ws commit and the served bundle.
        """
        return {
            "roles": {
                role: {
                    "image": values["image"],
                    "image_created": values["image_created"],
                    "image_repo_digests": values["image_repo_digests"],
                    "image_revision_label": values["image_revision_label"],
                    "artifact_path": values["artifact_path"],
                    "artifact_sha256": values["artifact_sha256"],
                }
                for role, values in sorted(provenance.items()) if role in ROLES
            },
            "compiled_ws_commit": provenance["compiled_ws_commit"],
            "served_bundle_sha256": provenance["served_bundle_sha256"],
        }

    def build(self, source: str) -> dict:
        """Build all five roles outside the deployment lock."""
        if self._locked:
            raise Held("building under the deployment lock is not allowed")
        policy = self._require_policy("building a candidate")
        source = source_sha(source)
        held = sorted(role for role, decision in policy.decisions.items() if decision != "rebuild")
        if held:
            raise Held(
                f"adoption holds {', '.join(held)} pinned; this controller promotes all five "
                "roles together, so promotion is refused rather than silently unpinning them. "
                "Re-adopt with --role-decision <role>=rebuild once the provenance of each held "
                "role has been reviewed."
            )
        free = self.files.free_bytes(self.root)
        if free < self.free_bytes_required:
            raise Deferred(
                f"insufficient build headroom: {free} bytes free, "
                f"{self.free_bytes_required} required (nothing is pruned)"
            )
        self._frozen_parity(source)
        builder = self._verify_builder()
        documents = self._containers()
        self._reject_unsafe_runtime(documents)
        provenance = self.provenance(documents)
        if _digest_value(self.decision_binding(provenance)) != policy.provenance_digest:
            raise Held(
                "baseline provenance differs from what the per-role decisions were recorded "
                "against (image IDs, baked artifacts, compose launch files or the served "
                "bundle changed); adopt again before promoting"
            )
        retained = self._retain_rollback_images(documents, provenance["compiled_ws_commit"])
        directory = self._materialize_source(source)

        flag = "true" if policy.experimental_strategies else "false"
        groups: dict[tuple, list] = {}
        for role in sorted(ROLES):
            arguments = []
            if role == "ws":
                arguments.append(f"STOCKSPOTTER_COMMIT={source}")
            if role == "web":
                arguments.append(f"VITE_EXPERIMENTAL_STRATEGIES={flag}")
            groups.setdefault((DOCKERFILES[role], tuple(arguments)), []).append(role)

        tags = {}
        for (dockerfile, arguments), roles in sorted(groups.items()):
            argv = ["docker", "buildx", "build", "--builder", self.builder, "--load",
                    "--progress", "plain", "--file", str(directory / dockerfile),
                    "--label", f"{REVISION_LABEL}={source}"]
            if "web" in roles:
                argv += ["--label", f"{EXPERIMENTAL_LABEL}={flag}"]
            for argument in arguments:
                argv += ["--build-arg", argument]
            for role in roles:
                tag = f"stockspotter-candidate-{role}:{source}"
                tags[role] = tag
                argv += ["--tag", tag]
            argv.append(str(directory))
            self._run(argv, timeout=3600)

        images = {}
        for role in sorted(ROLES):
            identity = self._run(["docker", "image", "inspect", "--format", "{{.Id}}",
                                  tags[role]], timeout=60).out.strip()
            if not IMAGE_ID.fullmatch(identity):
                raise Rejected(f"{role}: built image has no immutable identity")
            images[role] = identity
        self.evidence["build"] = {
            "source": source,
            "builder": builder,
            "tags": tags,
            "images": images,
            "rollback": retained,
            "decisions": dict(policy.decisions),
            "baseline_provenance_digest": policy.provenance_digest,
            "free_bytes_before": free,
            "free_bytes_after": self.files.free_bytes(self.root),
            "staging": str(directory),
        }
        return image_set(images)

    def revisions(self, images: dict) -> dict:
        """What each candidate image says it was built from."""
        images = image_set(images)
        found = {}
        for role, image in sorted(images.items()):
            label = self._run(["docker", "image", "inspect", "--format",
                               f'{{{{index .Config.Labels "{REVISION_LABEL}"}}}}',
                               image], timeout=60).out.strip()
            found[role] = label
            if role == "web" and self.policy is not None:
                experimental = self._run(
                    ["docker", "image", "inspect", "--format",
                     f'{{{{index .Config.Labels "{EXPERIMENTAL_LABEL}"}}}}', image],
                    timeout=60).out.strip()
                expected = "true" if self.policy.experimental_strategies else "false"
                if experimental != expected:
                    raise Rejected(
                        "web candidate's experimental-strategies build flag "
                        "differs from the adopted policy"
                    )
        return found

    # -- Host: safety windows ----------------------------------------------
    def _local_now(self) -> datetime:
        return self.clock().astimezone(ZoneInfo(ZONE))

    @staticmethod
    def _inside_window(moment: datetime) -> bool:
        return moment.time() >= WINDOW_OPEN or moment.time() < WINDOW_CLOSE

    @staticmethod
    def _window_close(moment: datetime) -> datetime:
        day = moment.date() + timedelta(days=1) if moment.time() >= WINDOW_OPEN else moment.date()
        return datetime.combine(day, WINDOW_CLOSE, tzinfo=moment.tzinfo)

    def allowed(self) -> bool:
        """20:00-04:00 America/New_York, with enough window left to finish.

        Consulted by the transaction for *candidate promotion* only. Baseline
        inspection, adoption, reconciliation and rollback proof are never
        gated on the clock -- see `health()`.
        """
        moment = self._local_now()
        if not self._inside_window(moment):
            return False
        remaining = self._window_close(moment) - moment
        return remaining >= timedelta(minutes=self.window_minutes_required)

    @staticmethod
    def _exact_zero(value) -> bool:
        """Exactly the integer 0. `type(...) is int` on purpose.

        `isinstance(False, int)` is True in Python, so an `isinstance` check
        here would read a boolean `False` -- which is what a broker client
        returns for "I could not count" far more often than a real zero -- as
        proof of zero exposure. A count must be a true `int`, and a negative
        count is not a count at all.
        """
        return type(value) is int and value == 0

    def exposure_zero(self) -> bool:
        """Real broker-confirmed zero exposure, plus zero simulated positions.

        Anything that cannot be *proved* zero is false, not true: an
        unreachable broker, a non-paper endpoint, a non-ACTIVE or blocked
        account, a probe that could not authenticate, malformed output, a
        report that is not even an object, or a count that is a boolean rather
        than an integer.

        Deliberately independent of the market window: `market_open` and
        `next_open` are recorded by the preflight and ignored here, because the
        promotion window is enforced by `allowed()` and this is a question
        about *positions*, not about the clock.
        """
        preflight = self.files.read_text(self.controller_root / "release_preflight.py")
        if not preflight:
            raise Held(
                f"controller exposure check is not installed at "
                f"{self.controller_root / 'release_preflight.py'}"
            )
        broker = self._in_container("qualify", ["python", "-"], stdin=preflight,
                                    timeout=120, allow=(0, 1))
        if broker.code != 0:
            return False
        try:
            report = json.loads(broker.out)
        except ValueError:
            return False
        # A list, a string or `null` would make `.get` raise, and an exception
        # out of this method is not a safe answer: the exposure gate must
        # return False for anything it cannot read.
        if not isinstance(report, dict):
            return False
        # `account_active` and `api_token_present_and_long_enough` are part of
        # the preflight's own answer and were previously ignored: a suspended
        # account or a probe that never authenticated would have counted as
        # proved-zero exposure.
        for name in ("paper_endpoint_configured", "api_token_present_and_long_enough",
                     "account_active"):
            if report.get(name) is not True:
                return False
        if report.get("trading_blocked") is not False:
            return False
        if not self._exact_zero(report.get("positions")):
            return False
        if not self._exact_zero(report.get("open_orders")):
            return False
        simulated = self._in_container("qualify", ["python", "-"],
                                       stdin=SIMULATED_EXPOSURE_PROBE, timeout=90, allow=(0, 1))
        if simulated.code != 0:
            return False
        try:
            document = json.loads(simulated.out)
        except ValueError:
            return False
        if not isinstance(document, dict):
            return False
        return self._exact_zero(document.get("openPositions"))

    # -- Host: mutation -----------------------------------------------------
    def checkout(self, source: str, branch: str) -> None:
        """Move the production checkout to an exact commit on an exact branch.

        Fast-forward only, existing branches only, refuses a dirty tree, and
        re-verifies the frozen files afterwards: a branch move is exactly how
        the preregistration or the qualify session script could change under a
        running measurement.
        """
        self._require_lock("moving the production checkout")
        source = source_sha(source)
        if not isinstance(branch, str) or not branch:
            raise Held("no branch to check out")
        if not self._clean():
            raise Rejected("production checkout has local changes")
        if self._git("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}",
                     allow=(0, 1)).code != 0:
            raise Rejected(f"branch {branch} does not exist in the production checkout")
        self._frozen_parity(source)
        self._git("switch", "--quiet", branch)
        if self._head() != source:
            self._git("merge", "--ff-only", source)
        if self._head() != source or self._branch() != branch:
            raise Rejected(f"checkout did not reach {source} on {branch}")
        if not self._clean():
            raise Rejected("production checkout became dirty during the move")
        if self.policy is not None:
            for name, expected in self.policy.frozen.items():
                if self.files.digest(self.root / name) != expected:
                    raise Rejected(f"frozen content changed: {name}")

    def up(self, images: dict) -> None:
        """Recreate every service from the private recipe on exactly these images.

        The recipe is the adopted baseline's full effective configuration with
        only the image IDs replaced, so this is the same code path for a
        promotion and for a rollback, and neither can lose the overlay's
        configuration. The base compose file is never passed to `up`.
        """
        self._require_lock("recreating services")
        images = image_set(images)
        compose = self._target_recipe(images)
        for role, image in sorted(images.items()):
            identity = self._run(["docker", "image", "inspect", "--format", "{{.Id}}",
                                  image], timeout=60).out.strip()
            if identity != image:
                raise Rejected(f"{role}: pinned image {image} is not present")
        # Only a *candidate* recreation is preflighted against the compose
        # contract of the commit being promoted. Restoring the adopted baseline
        # must never be gated on the repository's compose file: the baseline is
        # the observed reality, and a preflight that could refuse it would turn
        # the rollback this controller exists to guarantee into a hold.
        if images == self._recipe_record()["images"]:
            self.evidence["base_compatibility"] = "skipped: restoring the adopted baseline"
        else:
            self.evidence["base_compatibility"] = self._base_compatibility(compose)
        path = self._write_active_recipe(compose)
        # deploy.sh's own pre-recreation token assertion, with the images and
        # the environment that are about to run.
        self._compose_recipe(path, "run", "--rm", "--no-deps", "--no-build", "qualify",
                             "python", "-c", TOKEN_ASSERT, timeout=180)
        self._compose_recipe(path, "up", "-d", "--no-build", "--wait", "--wait-timeout",
                             str(self.compose_timeout), timeout=self.compose_timeout + 180)
        documents = self._containers()
        running = {role: self._image(document) for role, document in documents.items()}
        if running != images:
            raise Rejected("recreated services do not run the pinned images")
        self._verify_recipe_applied(compose, documents)
        self.evidence["recreated"] = {
            "recipe": str(path),
            "images": dict(images),
            "role_config_digests": {role: self._config_digest(document)
                                    for role, document in sorted(documents.items())},
        }

    # -- Host: proof --------------------------------------------------------
    def _served_bundle(self) -> str:
        """What the project's web upstream actually serves for `/`.

        Production is unchanged: the same loopback `curl` deploy.sh has always
        used. A stage project publishes nothing on the host, so there its
        bundle is fetched over the project's own internal network from inside
        the qualify container.
        """
        if self.stage:
            result = self._in_container("qualify", ["python", "-"], stdin=STAGE_BUNDLE_PROBE,
                                        timeout=60, allow=(0, 1))
            if result.code != 0:
                raise Rejected("the stage web service did not serve its bundle")
            return result.out
        return self._run(["curl", "--fail", "--silent", "--show-error",
                          "http://127.0.0.1:3000/"], timeout=60).out

    def health(self) -> dict:
        """Multi-iteration dwell proof of the real runtime.

        Each iteration re-reads every container, enforces the runtime
        invariants and flag parity, runs the existing authenticated probes,
        compares the served web bundle against the bundle inside the image,
        re-reads the compiled ws commit and re-reads the baked artifact
        digests. Every iteration must agree.

        The clock is checked here against the *candidate* being live, never
        unconditionally: crossing 04:00 America/New_York with the candidate
        running fails this proof so the transaction rolls back, while proving a
        restored baseline after 04:00 must still be possible -- otherwise the
        window would forbid the very rollback it exists to force, and a
        rollback would always end Held instead of verified.
        """
        observed = []
        for iteration in range(max(1, self.dwell_iterations)):
            if iteration:
                self.sleeper(self.dwell_seconds)
            documents = self._containers()
            self._reject_unsafe_runtime(documents)
            # Conformance to the declared effective configuration, re-proved on
            # every iteration rather than only straight after recreation: this
            # is where a lost overlay variable, a changed bind source or
            # dropped log rotation becomes a failed proof, and therefore a
            # rollback rather than a hold.
            if self._recipe_record(required=False) is not None:
                self._verify_recipe_applied(
                    self._target_recipe({role: self._image(document)
                                         for role, document in documents.items()}),
                    documents)
            probe = self._in_container("qualify", ["python", "/app/check_health.py"],
                                       timeout=180, allow=(0, 1))
            if probe.code != 0:
                raise Rejected("authenticated HTTP/WS/qualify/web probes failed")
            served = self._served_bundle()
            inside = self._run(["docker", "exec", self.containers["web"], "cat",
                                "/app/dist/index.html"], timeout=60).out
            if _digest_text(served) != _digest_text(inside):
                raise Rejected("served web bundle differs from the image's bundle")
            commit = self._ws_commit()
            artifacts = self._verify_artifacts(documents)
            if commit == self.candidate_source and self.candidate_source is not None:
                if self.force_rollback:
                    raise Rejected(
                        "rehearsal: the candidate's proof is failed on purpose so the "
                        "rollback this records is a real one"
                    )
                if self.deadline is not None and self._local_now() >= self.deadline:
                    raise Rejected("promotion window closed while the candidate was live")
            observed.append({
                "images": {role: self._image(document) for role, document in documents.items()},
                "running": {role: (document.get("State") or {}).get("Running") is True
                            for role, document in documents.items()},
                "ws_commit": commit,
                "artifacts": artifacts,
                "restarts": {role: document.get("RestartCount")
                             for role, document in documents.items()},
                "config": {role: self._config_digest(document)
                           for role, document in documents.items()},
            })
            if observed[-1] != observed[0]:
                raise Rejected("runtime changed between dwell iterations")
        final = observed[-1]
        self.evidence["dwell"] = {
            "iterations": len(observed),
            "seconds": self.dwell_seconds,
            "restarts": final["restarts"],
            "artifacts": final["artifacts"],
            "role_config_digests": final["config"],
        }
        return {"images": final["images"], "running": final["running"],
                "ws_commit": final["ws_commit"]}

    # -- read-only observation ---------------------------------------------
    def observe(self, include_exposure=True) -> dict:
        """Everything `inspect` reports. Read-only: no fetch, no mutation."""
        documents = self._containers()
        known_tip = self._git("rev-parse", "--verify", "--quiet",
                              "refs/remotes/origin/master", allow=(0, 1))
        effective = self._effective(documents, declared=self._declared_literals())
        legacy_marker, legacy_timer = self.legacy_marker(), self.legacy_timer()
        compiled = self._ws_commit()
        report = {
            "observed_at": self.clock().isoformat(),
            "observed_not_deployed": True,
            "project": self.project,
            "root": str(self.root),
            "control_dir": str(self.control_dir),
            "boot_id": self.boot_id(),
            "checkout_source": self._head(),
            "branch": self._branch(),
            "checkout_clean": self._clean(),
            "known_origin_master": known_tip.out.strip() or None,
            "marker": self.marker(),
            "legacy_marker": legacy_marker,
            "legacy_marker_written_by_this_controller": False,
            "legacy_timer": legacy_timer,
            "compiled_ws_commit": compiled,
            "images": {role: self._image(document) for role, document in documents.items()},
            "running": {role: (document.get("State") or {}).get("Running") is True
                        for role, document in documents.items()},
            "restarts": {role: document.get("RestartCount")
                         for role, document in documents.items()},
            "compose_labels": {
                role: {
                    "project": self._labels(document).get(COMPOSE_PROJECT_LABEL),
                    "service": self._labels(document).get(COMPOSE_SERVICE_LABEL),
                    "config_files": self._labels(document).get(COMPOSE_CONFIG_FILES_LABEL),
                    "working_dir": self._labels(document).get(COMPOSE_WORKING_DIR_LABEL),
                }
                for role, document in documents.items()
            },
            "bind_mounts": {
                role: sorted(f"{mount.get('Source')}:{mount.get('Destination')}"
                             for mount in (document.get("Mounts") or [])
                             if (mount or {}).get("Type") == "bind")
                for role, document in documents.items()
            },
            "flags": {role: self._flags(document) for role, document in documents.items()},
            "effective_env": {
                role: {"env_file": values["env_file"],
                       "dotenv_variables": sorted(values["dotenv"]),
                       "literal_variables": sorted(values["literal"]),
                       "image_variables": sorted(values["image"]),
                       "identity_variables": sorted(values["identity"])}
                for role, values in sorted(effective.items())
            },
            "role_config_digests": {role: self._config_digest(document)
                                    for role, document in documents.items()},
            "frozen_digests": {name: self.files.digest(self.root / name)
                               for name in FROZEN_PATHS},
            "env_hash": self._env_digest(documents),
            "baseline_recipe_captured": self._recipe_record(required=False) is not None,
            "free_bytes": self.files.free_bytes(self.root),
            "window_open_now": self.allowed(),
        }
        report["provenance"] = self.provenance(documents)
        if legacy_marker and legacy_marker != report["checkout_source"]:
            report["legacy_timer_risk"] = (
                f"{LEGACY_MARKER_PATH} names {legacy_marker} while the checkout is at "
                f"{report['checkout_source']}: if {LEGACY_TIMER} is armed, deploy.sh will "
                "rebuild from the base compose file and discard this effective "
                "configuration. This controller never writes that file."
            )
        try:
            report["runtime_invariants"] = "ok"
            self._reject_unsafe_runtime(documents)
        except Rejected as error:
            report["runtime_invariants"] = f"REJECTED: {error}"
        if include_exposure:
            try:
                report["exposure_zero"] = self.exposure_zero()
            except (Deferred, Held, Rejected) as error:
                report["exposure_zero"] = f"UNPROVEN: {error}"
        return report


# ---------------------------------------------------------------------------
# Controller: explicit commands
# ---------------------------------------------------------------------------
PREPARED_MAX_AGE_SECONDS = 12 * 3600


class Controller:
    """`inspect` / `adopt` / `prepare` / `rehearse` / `promote` / `reconcile`.

    Each command is explicit; none implies another. The timer unit shipped with
    this change runs `prepare` only.
    """

    def __init__(self, host: LinuxHost, store, out=None):
        self.host, self.store = host, store
        self.out = out if out is not None else sys.stdout

    # -- receipts -----------------------------------------------------------
    def _receipt(self, kind: str, payload: dict) -> str:
        stamp = self.host.clock().strftime("%Y%m%dT%H%M%SZ")
        name = f"receipt-{kind}-{stamp}.json"
        self.store.write(name, {"version": 1, "kind": kind, "at": stamp,
                                "legacy_timer": self.host.legacy_timer(),
                                "legacy_marker_untouched": self.host.legacy_marker(), **payload})
        return name

    def _policy(self):
        value = self.store.read("policy.json")
        if value is None:
            return None
        return Policy.from_json(value)

    def _print(self, value):
        print(json.dumps(value, indent=2, sort_keys=True), file=self.out)

    # -- commands -----------------------------------------------------------
    def inspect(self, exposure=True) -> int:
        report = self.host.observe(include_exposure=exposure)
        report["adoption"] = {
            "committed_receipt": self.store.read("committed.json") is not None,
            "policy": self.store.read("policy.json") is not None,
            "journal": (self.store.read("journal.json") or {}).get("phase"),
            "rollback_rehearsal": self.store.read(REHEARSAL_NAME) is not None,
        }
        self._print(report)
        self._receipt("observation", {"observation": report})
        return 0

    def adopt(self, confirm: bool, experimental: bool, decisions: dict,
              exemptions: dict, expected_artifacts: dict) -> int:
        """Record the observed baseline as the committed schema-2 receipt.

        Provenance is explicitly `observed-not-deployed`: this controller did
        not produce the running images, it only verified and recorded them.
        Adoption also captures the private full-fidelity compose recipe that
        every later recreation and rollback replays, and it does *not* touch
        `ops/vps/.deployed-commit`.
        """
        if not confirm:
            raise Deferred("adopt requires --confirm")
        if set(decisions) != ROLES:
            raise Deferred(
                "adopt requires an explicit --role-decision <role>=rebuild|hold for each of "
                f"{', '.join(sorted(ROLES))}: promoting must never silently unpin a "
                "deliberately old protected image"
            )
        existing = self.store.read("committed.json")
        with self.host.lock():
            documents = self.host._containers()
            provenance = self.host.provenance(documents)
            for role, digest in sorted(expected_artifacts.items()):
                observed = (provenance.get(role) or {}).get("artifact_sha256")
                if observed is None:
                    raise Held(
                        f"{role}: the asserted artifact digest cannot be verified -- "
                        f"sha256sum could not read {ARTIFACT_PATHS[role]} in the running "
                        "container"
                    )
                if observed != digest:
                    raise Rejected(
                        f"{role}: baked artifact is {observed}, not the asserted {digest}"
                    )
            policy = Policy(
                frozen={name: self.host.files.digest(self.host.root / name)
                        for name in FROZEN_PATHS},
                flags=self.host.flags(),
                experimental_strategies=experimental,
                baseline_images={role: self.host._image(document)
                                 for role, document in documents.items()},
                decisions=dict(decisions),
                exemptions={role: sorted(exemptions.get(role, ())) for role in sorted(ROLES)},
                artifacts={role: (provenance.get(role) or {}).get("artifact_sha256")
                           for role in sorted(ROLES)},
                provenance_digest=_digest_value(self.host.decision_binding(provenance)),
            )
            if any(digest is None for digest in policy.frozen.values()):
                raise Held("a frozen file is missing; refusing to adopt")
            self.host.policy = policy
            observed = state(self.host.snapshot())
            transaction = core.Transaction(self.store, self.host)
            if existing is not None and transaction.committed() != observed:
                raise Held("a different baseline is already committed; reconcile explicitly")
            recipe = self.host.capture_recipe(documents)
            transaction.proof(observed)
            legacy_marker = self.host.legacy_marker()
            self.host.write_marker(observed["source"])
            if self.host.marker() != observed["source"]:
                raise Held("adopted marker did not persist")
            self.store.write("policy.json", policy.as_json())
            self.store.write("committed.json", {"version": 2, "state": observed})
            name = self._receipt("adoption", {
                "provenance": "observed-not-deployed",
                "state": observed,
                "legacy_marker_read_only": legacy_marker,
                "controller_marker": self.host.marker(),
                "policy": policy.as_json(),
                "role_provenance": provenance,
                "asserted_artifacts": dict(expected_artifacts),
                "recipe": {"images": recipe["images"], "effective": recipe["effective"],
                           "inventory": recipe["inventory"]},
                "dwell": self.host.evidence.get("dwell"),
            })
        self._print({"adopted": observed, "receipt": name,
                     "legacy_marker_untouched": legacy_marker,
                     "baseline_recipe": str(self.host.control_dir / BASELINE_RECIPE_NAME),
                     "promotion": "blocked until `rehearse --confirm` records a real rollback"})
        return 0

    def prepare(self) -> int:
        """Gate and build a candidate. Never touches the running stack."""
        with self.host.build_lock():
            return self._prepare_locked()

    def watch(self) -> int:
        """Poll master without repeatedly rebuilding an unchanged candidate.

        This remains build-only. A timer never substitutes for a promotion
        decision or for rollback evidence.
        """
        with self.host.build_lock():
            policy = self._policy()
            if policy is None:
                raise Deferred("adopt a baseline before watching master")
            self.host.policy = policy
            self.host._recipe_record()
            transaction = core.Transaction(self.store, self.host)
            if transaction.journal() is not None:
                raise Deferred("reconcile the deployment journal before building")
            baseline = transaction.committed()
            observed = state(self.host.snapshot())
            if baseline is None or observed != baseline or self.host.marker() != baseline["source"]:
                raise Deferred("running baseline changed; reconcile before building")
            tip = self.host.tip()
            if observed["source"] == tip and observed["checkout_source"] == tip:
                self._print({"unchanged": tip, "built": False})
                return 0
            if self.store.read("prepared.json") is not None:
                try:
                    envelope = self._prepared()
                except Deferred:
                    envelope = None
                if envelope is not None:
                    prepared = envelope["prepared"]
                    if (state(prepared["baseline_state"]) == baseline
                            and state(prepared["state"])["source"] == tip
                            and self.host.env_digest() == baseline["env_hash"]
                            and self.host.ci_passed(tip) is True
                            and self.host.revisions(prepared["state"]["images"])
                            == {role: tip for role in ROLES}):
                        self._print({"prepared": tip, "built": False})
                        return 0
            return self._prepare_locked()

    def _prepare_locked(self) -> int:
        policy = self._policy()
        if policy is None:
            raise Deferred("adopt a baseline before preparing a candidate")
        self.host.policy = policy
        self.host._recipe_record()
        transaction = core.Transaction(self.store, self.host)
        if transaction.journal() is not None:
            raise Deferred("reconcile the deployment journal before building")
        boot_id = self.host.boot_id()
        prepared = transaction.prepare()
        envelope = {
            "version": 1,
            "boot_id": boot_id,
            "prepared_at": self.host.clock().isoformat(),
            "prepared": prepared,
            "build": self.host.evidence.get("build"),
            "server_ci_gate": self.host.evidence.get("server_ci_gate"),
            "android_gate": self.host.evidence.get("android_gate"),
            "candidate_archive_sha256": self.host.evidence.get("candidate_archive_sha256"),
        }
        self.store.write("prepared.json", envelope)
        name = self._receipt("prepare", envelope)
        self._print({"candidate": prepared["state"]["source"],
                     "images": prepared["state"]["images"],
                     "receipt": name, "promotion": "manual only"})
        return 0

    def _prepared(self) -> dict:
        envelope = self.store.read("prepared.json")
        if envelope is None:
            raise Deferred("no prepared candidate; run prepare first")
        if (not isinstance(envelope, dict) or envelope.get("version") != 1
                or not isinstance(envelope.get("prepared"), dict)
                or not isinstance(envelope.get("boot_id"), str)
                or not isinstance(envelope.get("prepared_at"), str)):
            raise Held("Malformed prepared candidate record")
        if envelope["boot_id"] != self.host.boot_id():
            raise Deferred("the host rebooted after preparation; prepare again")
        try:
            prepared_at = datetime.fromisoformat(envelope["prepared_at"])
        except ValueError as error:
            raise Held("Malformed preparation timestamp") from error
        age = (self.host.clock() - prepared_at).total_seconds()
        if age < 0 or age > PREPARED_MAX_AGE_SECONDS:
            raise Deferred("the prepared candidate is stale; prepare again")
        return envelope

    def _transact(self, policy, envelope, rehearsal: bool):
        """Run one promotion attempt, rehearsal or real, with the hard stop."""
        transaction = core.Transaction(self.store, self.host)
        self.host.candidate_source = envelope["prepared"]["state"]["source"]
        self.host.deadline = self.host._window_close(self.host._local_now())
        self.host.force_rollback = rehearsal
        try:
            return transaction.promote(envelope["prepared"])
        finally:
            self.host.deadline = self.host.candidate_source = None
            self.host.force_rollback = False

    def rehearse(self, confirm: bool) -> int:
        """Promote a real candidate, fail its proof on purpose, prove rollback.

        This is the gate `promote` requires. It uses the real host path -- real
        recreation on the candidate images, then a real restoration from the
        private baseline recipe -- and fails the candidate's dwell proof
        deliberately, so what it records is an actual rollback rather than an
        assertion that one would work.
        """
        if not confirm:
            raise Deferred("rehearse requires --confirm")
        policy = self._policy()
        if policy is None:
            raise Deferred("adopt a baseline before rehearsing a rollback")
        self.host.policy = policy
        self.host._require_legacy_timer_disarmed("rehearsal")
        baseline = core.Transaction(self.store, self.host).committed()
        if baseline is None:
            raise Held("no committed baseline to roll back to")
        envelope = self._prepared()
        try:
            self._transact(policy, envelope, rehearsal=True)
        except Rejected as error:
            restored = state(self.host.snapshot())
            if restored != baseline:
                raise Held("rehearsal did not restore the adopted baseline") from error
            evidence = {
                "version": 1,
                "rehearsed_at": self.host.clock().isoformat(),
                "candidate": envelope["prepared"]["state"],
                "baseline_state": baseline,
                "restored_state": restored,
                "policy_digest": policy.digest(),
                "restored_artifacts": self.host.evidence.get("dwell", {}).get("artifacts"),
                "restored_config_digests": self.host.evidence.get("dwell", {})
                .get("role_config_digests"),
                "recreated": self.host.evidence.get("recreated"),
                "boot_id": self.host.boot_id(),
                "rejection": str(error),
            }
            self.store.write(REHEARSAL_NAME, evidence)
            name = self._receipt("rehearsal", evidence)
            self._print({"rehearsed": "rollback verified", "restored": restored,
                         "receipt": name})
            return 0
        raise Held(
            "rehearsal was expected to roll back and did not; the candidate is now live "
            "without a recorded rollback proof -- reconcile explicitly"
        )

    def _require_rehearsal(self, policy, baseline) -> dict:
        evidence = self.store.read(REHEARSAL_NAME)
        if evidence is None:
            raise Deferred(
                "no recorded rollback rehearsal; run `rehearse --confirm` and keep the "
                "receipt before the first real promotion"
            )
        if (not isinstance(evidence, dict) or evidence.get("version") != 1
                or not isinstance(evidence.get("baseline_state"), dict)
                or not isinstance(evidence.get("restored_state"), dict)):
            raise Held("Malformed rollback rehearsal record")
        if evidence.get("policy_digest") != policy.digest():
            raise Deferred("the recorded rollback rehearsal predates this adoption policy; "
                           "rehearse again")
        if state(evidence["restored_state"]) != baseline or state(evidence["baseline_state"]) != baseline:
            raise Deferred("the recorded rollback rehearsal restored a different baseline; "
                           "rehearse again")
        return evidence

    def promote(self, confirm: bool) -> int:
        if not confirm:
            raise Deferred("promote requires --confirm")
        policy = self._policy()
        if policy is None:
            raise Deferred("adopt a baseline before promoting")
        self.host.policy = policy
        self.host._recipe_record()
        legacy_timer = self.host._require_legacy_timer_disarmed("promotion")
        baseline = core.Transaction(self.store, self.host).committed()
        if baseline is None:
            raise Held("no committed baseline to promote from")
        rehearsal = self._require_rehearsal(policy, baseline)
        envelope = self._prepared()
        # The hard stop for this promotion: the close of the window it starts
        # in. `health()` applies it only while the candidate is live.
        promoted = self._transact(policy, envelope, rehearsal=False)
        # A promoted candidate must never be promotable twice: the next
        # promotion starts from a fresh `prepare` against the new baseline, and
        # the rollback rehearsal must be redone against the new baseline too.
        for stale in ("prepared.json", REHEARSAL_NAME):
            (self.store.directory / stale).unlink(missing_ok=True)
        self.store.sync_directory()
        name = self._receipt("promotion", {
            "source": promoted,
            "state": envelope["prepared"]["state"],
            "baseline_state": envelope["prepared"]["baseline_state"],
            "build": envelope.get("build"),
            "server_ci_gate": envelope.get("server_ci_gate"),
            "android_gate": envelope.get("android_gate"),
            "legacy_timer": legacy_timer,
            "rollback_rehearsal_at": rehearsal.get("rehearsed_at"),
            "base_compatibility": self.host.evidence.get("base_compatibility"),
            "recreated": self.host.evidence.get("recreated"),
            "dwell": self.host.evidence.get("dwell"),
        })
        self._print({"promoted": promoted, "receipt": name,
                     "note": "adopt again to capture a recipe for the new baseline"})
        return 0

    def reconcile(self) -> int:
        policy = self._policy()
        if policy is not None:
            self.host.policy = policy
        transaction = core.Transaction(self.store, self.host)
        outcome = transaction.reconcile()
        name = self._receipt("reconciliation", {
            "outcome": outcome, "dwell": self.host.evidence.get("dwell"),
            "recreated": self.host.evidence.get("recreated")})
        self._print({"reconciled": outcome, "receipt": name})
        return 0


DEFAULT_STORE = Path.home() / ".stockspotter-deploy"
EXIT_CODES = {Deferred: 2, Rejected: 3, Held: 4}


def _pairs(values, allowed, description):
    """`role=value` arguments, validated against the five real roles."""
    parsed = {}
    for entry in values or ():
        role, separator, value = str(entry).partition("=")
        if not separator or role not in ROLES or (allowed and value not in allowed):
            raise SystemExit(
                f"deployment_host.py: {description} must be <role>=<"
                f"{'|'.join(allowed) if allowed else 'value'}>, got {entry!r}"
            )
        parsed.setdefault(role, []).append(value)
    return parsed


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="deployment_host.py",
        description="Schema-2 deployment controller for the stockspotter VPS.",
    )
    parser.add_argument("--root", type=Path, default=Path("/opt/apps/stockspotter"))
    parser.add_argument("--controller-root", type=Path, default=Path("/opt/stockspotter-controller"))
    parser.add_argument("--source-repo", type=Path, default=None)
    parser.add_argument("--store", type=Path, default=DEFAULT_STORE)
    parser.add_argument("--staging", type=Path, default=Path("/var/tmp/stockspotter-candidate"))
    parser.add_argument("--dwell-iterations", type=int, default=3)
    parser.add_argument("--dwell-seconds", type=int, default=20)
    commands = parser.add_subparsers(dest="command", required=True)
    inspect = commands.add_parser("inspect", help="observe the host; mutate nothing")
    inspect.add_argument("--no-exposure", action="store_true",
                         help="skip the read-only broker query")
    adopt = commands.add_parser("adopt", help="record the observed baseline")
    adopt.add_argument("--confirm", action="store_true")
    adopt.add_argument("--experimental-strategies", choices=("true", "false"), required=True,
                       help="asserted frontend build flag of the running web bundle")
    adopt.add_argument("--role-decision", action="append", metavar="ROLE=rebuild|hold",
                       required=True,
                       help="explicit per-role source decision; all five roles are required")
    adopt.add_argument("--baseline-exempt", action="append", default=[],
                       metavar="ROLE=unbounded-logs|restart-policy",
                       help="waive a runtime invariant for the adopted baseline image only")
    adopt.add_argument("--expect-artifact", action="append", default=[],
                       metavar="ROLE=SHA256",
                       help="assert the baked artifact digest of a role; verified at adoption")
    commands.add_parser("prepare", help="gate and build a candidate (no promotion)")
    commands.add_parser("watch", help="prepare changed master tips only (no promotion)")
    rehearse = commands.add_parser(
        "rehearse", help="promote with a deliberately failed proof to prove rollback")
    rehearse.add_argument("--confirm", action="store_true")
    promote = commands.add_parser("promote", help="promote the prepared candidate")
    promote.add_argument("--confirm", action="store_true")
    commands.add_parser("reconcile", help="finish or roll back an interrupted transaction")
    return parser


def main(argv=None, host_factory=None, store_factory=None, out=None) -> int:
    arguments = build_parser().parse_args(argv)
    store = (store_factory or core.Store)(arguments.store)
    host = (host_factory or LinuxHost)(
        root=arguments.root,
        controller_root=arguments.controller_root,
        control_dir=arguments.store,
        source_repo=arguments.source_repo,
        staging=arguments.staging,
        dwell_iterations=arguments.dwell_iterations,
        dwell_seconds=arguments.dwell_seconds,
    )
    controller = Controller(host, store, out=out)
    try:
        if arguments.command == "inspect":
            return controller.inspect(exposure=not arguments.no_exposure)
        if arguments.command == "adopt":
            decisions = {role: values[-1] for role, values in
                         _pairs(arguments.role_decision, ROLE_DECISIONS, "--role-decision").items()}
            exemptions = _pairs(arguments.baseline_exempt, EXEMPTABLE, "--baseline-exempt")
            artifacts = {role: values[-1] for role, values in
                         _pairs(arguments.expect_artifact, (), "--expect-artifact").items()}
            for role, digest in artifacts.items():
                if not DIGEST.fullmatch(digest):
                    raise SystemExit(f"deployment_host.py: --expect-artifact {role} needs a "
                                     "full lowercase sha256")
            return controller.adopt(arguments.confirm,
                                    arguments.experimental_strategies == "true",
                                    decisions, exemptions, artifacts)
        if arguments.command == "prepare":
            return controller.prepare()
        if arguments.command == "watch":
            return controller.watch()
        if arguments.command == "rehearse":
            return controller.rehearse(arguments.confirm)
        if arguments.command == "promote":
            return controller.promote(arguments.confirm)
        return controller.reconcile()
    except (Deferred, Rejected, Held) as error:
        print(f"{type(error).__name__.upper()}: {error}", file=sys.stderr)
        return EXIT_CODES.get(type(error), 1)


if __name__ == "__main__":
    raise SystemExit(main())
