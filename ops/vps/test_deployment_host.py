"""Tests for the real Linux host adapter.

There is no host here and no pretence of one. A fake command runner answers the
*actual* argument vectors the adapter issues (`docker inspect --format
{{json .}}`, `docker exec -i stockspotter-vps-qualify-1 python -`,
`docker compose -p stockspotter-vps -f <private recipe> up -d --no-build
--wait`, `systemctl is-enabled stockspotter-deploy.timer`, `git merge-base
--is-ancestor`) with realistic output, and mutates its own world the way the
host would.

The important fidelity property: the fake world's containers are *derived from
whatever compose document the adapter actually passes to `-f`*. The baseline is
launched the way the real host launched it -- from untracked overlay compose
files that set `IAC_ENABLED`/`EXTREME_MOMENTUM_ENABLED` and bind data from the
IAC worktree, neither of which the repository's own
`ops/vps/docker-compose.yml` mentions. So a controller that recreated from the
base compose file would really lose those, and the tests that assert they
survive a promotion and a rollback are therefore meaningful.

Every assertion is about what the adapter commanded and what it refused --
never about a deployment having happened.
"""
import contextlib
from contextlib import contextmanager
from datetime import datetime, timedelta
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
from types import SimpleNamespace
import unittest
from zoneinfo import ZoneInfo


def _load(name):
    path = Path(__file__).with_name(name + ".py")
    # One module object per process, for the reason `deployment_host._load_core`
    # gives: a second execution creates a second set of Rejected/Deferred/Held
    # classes, and then `assertRaises` silently stops matching. This used to
    # return a fresh copy whenever something had already imported the module by
    # name -- which `test_deployment_git_checkout` does -- so the two suites
    # failed wholesale depending on which ran first, including under plain
    # `unittest discover`.
    loaded = sys.modules.get(name)
    if loaded is not None and getattr(loaded, "__file__", None) == str(path):
        return loaded
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    # Registered before execution so `dataclasses` can resolve annotations in a
    # module that was loaded by path rather than imported by name.
    sys.modules.setdefault(name, module)
    spec.loader.exec_module(module)
    return module


core = _load("deployment_transaction")
adapter = _load("deployment_host")

ROOT = Path("/opt/apps/stockspotter")
CONTROLLER = Path("/opt/stockspotter-controller")
STAGING = Path("/var/tmp/stockspotter-candidate")
BASELINE_CHECKOUT = "2472e0f94186d59409e8dcbf0b0bdf168362bcd0"
BASELINE_RUNTIME = "c28fb194849a4fdb532f2dbe6b3938b0c61c7b54"
BASELINE_BRANCH = "release/mover-provenance-20261011"
MASTER_TIP = "7d8ebe5caf2f0450925924d63fed0e12aaa5c0c6"
BOOT_ID = "6f1b2d4e-9a7c-4f21-9d0e-1a2b3c4d5e6f"
TOKEN = "x" * 40            # stands in for STOCKSPOTTER_API_TOKEN
SECRET = "alpaca-secret-value"
INDEX_HTML = "<!doctype html><title>stockspotter</title>"
# What the checkout tracks. `.env` is ignored rather than tracked, which is
# precisely why `reset --hard`/`switch` could write over the live one.
TRACKED_PATHS = (
    "OI-V2-PREREGISTRATION-2026-09-19.md",
    "ops/qualify/session.sh",
    "ops/vps/docker-compose.yml",
)

# The live stack's own data mount: the IAC coverage worktree, not the
# production checkout. Losing this on a rollback would lose the journals.
DATA_SOURCE = "/home/wavystack/stockspotter-iac-coverage-20261013/data"
EVIDENCE = "/home/wavystack/stockspotter-iac-coverage-20261013/release-evidence"
OVERLAY_FILES = {
    "web": f"{EVIDENCE}/active-web-compose.json",
    "ws": f"{EVIDENCE}/active-compose.json",
    "qualify": f"{EVIDENCE}/active-compose.json",
    "auto-trader": f"{EVIDENCE}/active-compose.json",
    "discovery-review": f"{EVIDENCE}/active-compose.json",
}

# What the host's own .env carries. Values never reach a controller file.
DOTENV = {
    "ALPACA_API_KEY": "key-value",
    "ALPACA_API_SECRET": SECRET,
    "STOCKSPOTTER_API_TOKEN": TOKEN,
    "AUTO_TRADER_EXECUTION_MODE": "journal",
}
# What every image provides. All on the validated allowlist.
IMAGE_ENV = {"PATH": "/usr/local/bin:/usr/bin", "LANG": "C.UTF-8"}

# The untracked overlay's `environment:` blocks. IAC_ENABLED and
# EXTREME_MOMENTUM_ENABLED appear nowhere in ops/vps/docker-compose.yml.
OVERLAY_ENV = {
    "web": {"VITE_PUBLIC_BASE": "https://stockspotter.app"},
    "ws": {
        "WS_SERVER_ADDR": "0.0.0.0:8787",
        "HTTP_SERVER_ADDR": "0.0.0.0:8788",
        "QUALIFY_SERVICE_URL": "http://qualify:8000",
        "OPPORTUNITY_OBSERVATION_ROOT": "/app/data/observation",
        "IAC_ENABLED": "true",
        "EXTREME_MOMENTUM_ENABLED": "true",
    },
    "qualify": {},
    "auto-trader": {"AUTO_TRADER_WS_URL": "ws://ws:8787"},
    "discovery-review": {},
}
USES_ENV_FILE = {"web": False, "ws": True, "qualify": True,
                 "auto-trader": True, "discovery-review": True}
PORTS = {
    "web": {"3000/tcp": [{"HostIp": "127.0.0.1", "HostPort": "3000"}]},
    "ws": {"8787/tcp": [{"HostIp": "127.0.0.1", "HostPort": "8787"}],
           "8788/tcp": [{"HostIp": "127.0.0.1", "HostPort": "8788"}]},
    "qualify": {},
    "auto-trader": {},
    "discovery-review": {},
}
BINDS = {
    "web": {},
    "qualify": {},
    "ws": {"/app/data": DATA_SOURCE},
    "auto-trader": {"/app/data": DATA_SOURCE},
    "discovery-review": {"/app/data": DATA_SOURCE},
}
CMD = {
    "web": ["bun", "run", "start"],
    "ws": ["./ws-server"],
    "qualify": ["uvicorn", "app:app"],
    "auto-trader": ["./auto-trader"],
    "discovery-review": ["python", "/app/discovery_review.py"],
}
LOGGING = {
    "ws": {"Type": "json-file", "Config": {"max-size": "256m", "max-file": "8"}},
}
DEFAULT_LOGGING = {"Type": "json-file", "Config": {"max-size": "50m", "max-file": "4"}}

# `docker compose -f ops/vps/docker-compose.yml config --format json`, as the
# repository's own compose file really resolves. Note what it does NOT contain:
# IAC_ENABLED, EXTREME_MOMENTUM_ENABLED or the IAC worktree's data path.
BASE_COMPOSE = {
    "name": adapter.PROJECT,
    "services": {
        "web": {"ports": [{"mode": "ingress", "target": 3000, "published": "3000",
                           "protocol": "tcp", "host_ip": "127.0.0.1"}]},
        "ws": {
            "env_file": [{"path": "../../.env", "required": True}],
            "environment": ["WS_SERVER_ADDR=0.0.0.0:8787", "HTTP_SERVER_ADDR=0.0.0.0:8788",
                            "QUALIFY_SERVICE_URL=http://qualify:8000",
                            "OPPORTUNITY_OBSERVATION_ROOT=/app/data/observation"],
            "ports": [{"mode": "ingress", "target": 8787, "published": "8787",
                       "protocol": "tcp", "host_ip": "127.0.0.1"},
                      {"mode": "ingress", "target": 8788, "published": "8788",
                       "protocol": "tcp", "host_ip": "127.0.0.1"}],
            "volumes": [{"type": "bind", "source": "../../data", "target": "/app/data"}],
        },
        "qualify": {"env_file": [{"path": "../../.env", "required": True}]},
        "auto-trader": {
            "env_file": [{"path": "../../.env", "required": True}],
            "environment": ["AUTO_TRADER_WS_URL=ws://ws:8787"],
            "volumes": [{"type": "bind", "source": "../../data", "target": "/app/data"}],
        },
        "discovery-review": {
            "env_file": [{"path": "../../.env", "required": True}],
            "command": ["python", "/app/discovery_review.py"],
            "volumes": [{"type": "bind", "source": "../../data", "target": "/app/data"}],
        },
    },
}

DISARMED_TIMER = {"is-enabled": "disabled", "is-active": "inactive"}


def image_id(label):
    return "sha256:" + hashlib.sha256(label.encode()).hexdigest()


def digest(value):
    return hashlib.sha256((value or "").encode()).hexdigest()


def dotenv_text(pairs=None):
    return "".join(f"{name}={value}\n" for name, value in sorted((pairs or DOTENV).items()))


class Killed(BaseException):
    """An interruption that is not an ordinary failure, like SIGKILL."""


class World:
    """The host as the adapter can observe and change it.

    `self.spec` is how each service is *currently launched*; `self.containers`
    is what `docker inspect` therefore reports. A recreation replaces a spec
    from the compose document the adapter passed, which is what makes "did the
    overlay configuration survive?" a real question here.
    """

    def __init__(self):
        self.branches = {"master": MASTER_TIP, BASELINE_BRANCH: BASELINE_CHECKOUT}
        self.branch = BASELINE_BRANCH
        # Commit -> first parent. A real commit graph, however small, because
        # `merge --ff-only` is only honest with one: see `_descends_from`.
        self.parents = {MASTER_TIP: BASELINE_CHECKOUT}
        self.fetch_tip = MASTER_TIP
        self.dirty = False
        self.ancestor_ok = True
        # Paths git tracks right now, and per commit when a commit differs.
        # `.env` is deliberately absent from both: it is ignored, which is
        # exactly why a checkout move can destroy it.
        self.tracked = set(TRACKED_PATHS)
        self.tracked_by_commit = {}
        self.retained = []                   # refs written by `git update-ref`
        self.update_ref_code = 0
        self.ci_code = 0
        self.ws_commit = BASELINE_RUNTIME
        self.dotenv = dict(DOTENV)
        self.base_compose = json.loads(json.dumps(BASE_COMPOSE))
        # `docker compose config` reads the file in the *checkout*, so the
        # answer depends on which commit is checked out -- which is what makes
        # a rollback's preflight differ from a promotion's.
        self.base_compose_by_commit = {}
        self.compose_config_code = 0
        self.timer = dict(DISARMED_TIMER)
        self.systemctl_present = True
        self.images = {}
        self.tags = {}
        for role in core.ROLES:
            identity = image_id(f"baseline-{role}")
            environment = dict(IMAGE_ENV)
            if role == "ws":
                environment["STOCKSPOTTER_COMMIT"] = BASELINE_RUNTIME
            self.images[identity] = {"labels": {}, "env": environment,
                                     "created": "2026-10-13T22:14:05Z", "digests": []}
        self.spec = {
            role: {
                "image": image_id(f"baseline-{role}"),
                "literal": dict(OVERLAY_ENV[role]),
                "env_file": USES_ENV_FILE[role],
                "ports": json.loads(json.dumps(PORTS[role])),
                "binds": dict(BINDS[role]),
                "restart": "unless-stopped",
                "logging": json.loads(json.dumps(LOGGING.get(role, DEFAULT_LOGGING))),
                "cmd": list(CMD[role]),
                "entrypoint": None,
                "user": "",
                "working_dir": "/app",
                "config_files": OVERLAY_FILES[role],
                "project": adapter.PROJECT,
                "network": f"{adapter.PROJECT}_default",
                "restarts": 0,
                "running": True,
                "drop_env": (),
            }
            for role in core.ROLES
        }
        self.artifact_override = {}
        self.sha256sum_missing = set()
        self.builder_driver = "docker-container"
        # Exactly the adapter's required bound; `_verify_builder` asserts
        # equality, so a fixture that drifted from it would hide the assertion.
        self.builder_memory = 2 * 1024 * 1024 * 1024
        self.builder_nano_cpus = 0
        self.builder_cpu_quota = 100000
        self.builder_cpu_period = 100000
        self.builder_present = True
        self.broker = {"paper_endpoint_configured": True, "api_token_present_and_long_enough": True, "trading_blocked": False,
                       "account_active": True, "market_open": False,
                       "positions": 0, "open_orders": 0}
        self.simulated_positions = 0
        self.health_code = 0
        self.bundle = INDEX_HTML
        self.stale_bundle_on = None          # the commit whose served bundle is stale
        self.break_health_on = None          # the commit whose probe fails
        self.branch_frozen = {}              # branch -> ops/qualify/session.sh content
        self.files = None                    # set by the harness, for branch-local content
        self.frozen_blobs = {}               # (commit, path) -> blob id, when it differs
        self.archive_extra = ()
        self.locked = False
        self.calls = []
        self.kill_on_up = None
        self.on_up = None
        self.mutations = []
        self.recipes = []                    # every compose document handed to `-f`

    # -- containers --------------------------------------------------------
    @property
    def containers(self):
        return {role: self._document(role) for role in self.spec}

    def _document(self, role):
        spec = self.spec[role]
        environment = dict(self.images[spec["image"]]["env"])
        if spec["env_file"]:
            environment.update(self.dotenv)
        environment.update(spec["literal"])
        for name in spec.get("drop_env") or ():
            environment.pop(name, None)
        destinations = sorted(spec["binds"])
        return {
            "Image": spec["image"],
            "State": {"Running": spec["running"]},
            "RestartCount": spec["restarts"],
            "Mounts": [{"Type": "bind", "Destination": destination,
                        "Source": spec["binds"][destination]}
                       for destination in destinations],
            "Config": {
                "Env": [f"{name}={value}" for name, value in sorted(environment.items())],
                "Cmd": spec["cmd"],
                "Entrypoint": spec["entrypoint"],
                "User": spec["user"],
                "WorkingDir": spec["working_dir"],
                "ExposedPorts": {},
                "Healthcheck": None,
                "Labels": {
                    adapter.COMPOSE_PROJECT_LABEL: spec["project"],
                    adapter.COMPOSE_SERVICE_LABEL: role,
                    adapter.COMPOSE_CONFIG_FILES_LABEL: spec["config_files"],
                    # As a Linux daemon really labels it, whatever platform
                    # happens to be running these tests.
                    adapter.COMPOSE_WORKING_DIR_LABEL: ROOT.as_posix(),
                },
            },
            "HostConfig": {
                "Binds": [f"{spec['binds'][destination]}:{destination}:rw"
                          for destination in destinations],
                "PortBindings": spec["ports"],
                "RestartPolicy": {"Name": spec["restart"]},
                "LogConfig": spec["logging"],
                "NetworkMode": spec["network"],
                "Memory": 0,
            },
        }

    def artifact(self, role):
        key = (role, self.spec[role]["image"])
        if key in self.artifact_override:
            return self.artifact_override[key]
        return digest(f"artifact|{role}|{self.spec[role]['image']}")

    # -- git ---------------------------------------------------------------
    @property
    def head(self):
        return self.branches[self.branch]

    @property
    def served(self):
        """What Caddy's upstream actually returns for `/`."""
        if self.stale_bundle_on is not None and self.ws_commit == self.stale_bundle_on:
            return "<!doctype html><title>stale bundle</title>"
        return self.bundle

    def _apply_branch_content(self, files):
        """Tracked content that differs between branches, as a checkout would."""
        content = self.branch_frozen.get(self.branch)
        if content is not None and files is not None:
            files.write_private(ROOT / "ops/qualify/session.sh", content)

    def _descends_from(self, descendant, ancestor):
        """Is `ancestor` reachable from `descendant` by first parents?

        Deliberately *not* wired to `self.ancestor_ok`. That flag answers
        `merge-base --is-ancestor`, which the adapter uses as a gate and which
        tests flip to stand for "master is not a fast-forward"; this answers
        what the ref-moving commands themselves would do, which is a different
        question and the one the fixture used to get wrong.
        """
        seen = set()
        while descendant and descendant not in seen:
            if descendant == ancestor:
                return True
            seen.add(descendant)
            descendant = self.parents.get(descendant)
        return False

    def _git(self, arguments, files):
        if arguments[:1] == ["fetch"]:
            return 0, "", ""
        if arguments[:2] == ["rev-parse", "FETCH_HEAD"]:
            return 0, self.fetch_tip + "\n", ""
        if arguments == ["rev-parse", "HEAD"]:
            return 0, self.head + "\n", ""
        if arguments == ["rev-parse", "--abbrev-ref", "HEAD"]:
            return 0, self.branch + "\n", ""
        if arguments[:2] == ["status", "--porcelain"]:
            return 0, (" M crates/ws-server/src/main.rs\n" if self.dirty else ""), ""
        if arguments[:2] == ["merge-base", "--is-ancestor"]:
            return (0 if self.ancestor_ok else 1), "", ""
        if arguments[:3] == ["rev-parse", "--verify", "--quiet"]:
            reference = arguments[3]
            if reference.startswith("refs/heads/"):
                name = reference[len("refs/heads/"):]
                return (0, self.branches[name] + "\n", "") if name in self.branches else (1, "", "")
            if reference == "refs/remotes/origin/master":
                return 0, MASTER_TIP + "\n", ""
            return 1, "", ""
        if arguments[:1] == ["switch"]:
            name = arguments[-1]
            assert self.locked, "a checkout move must hold the deployment lock"
            if name not in self.branches:
                return 1, "", f"no branch {name}"
            self.branch = name
            self.mutations.append("switch:" + name)
            self._apply_branch_content(files)
            return 0, "", ""
        if arguments[:2] == ["merge", "--ff-only"]:
            assert self.locked, "a checkout move must hold the deployment lock"
            target = arguments[2]
            if target == self.head or self._descends_from(self.head, target):
                # Real Git: "Already up to date.", exit *zero*, HEAD exactly
                # where it was. This fixture used to move the ref to any target
                # unconditionally -- forwards, backwards or sideways -- which is
                # the single reason no test here could see that a second
                # master-to-master rollback cannot move the checkout at all.
                # The regression lives in test_deployment_git_checkout.py now,
                # against actual Git; this keeps the fixture from lying again.
                return 0, "Already up to date.\n", ""
            if not self._descends_from(target, self.head):
                return 128, "", "fatal: Not possible to fast-forward, aborting."
            self.branches[self.branch] = target
            self.mutations.append("merge:" + target)
            return 0, "", ""
        if arguments[:2] == ["reset", "--hard"]:
            assert self.locked, "a checkout move must hold the deployment lock"
            self.branches[self.branch] = arguments[2]
            self.mutations.append("reset:" + arguments[2])
            self._apply_branch_content(files)
            return 0, "", ""
        if arguments[:1] == ["update-ref"]:
            self.retained.append(arguments[1])
            if self.update_ref_code:
                return self.update_ref_code, "", "fatal: fixture refused update-ref"
            return 0, "", ""
        if arguments[:1] == ["ls-tree"]:
            paths = self.tracked_by_commit.get(arguments[-1], self.tracked)
            return 0, "\0".join(sorted(paths)) + "\0", ""
        if arguments[:1] == ["ls-files"]:
            return 0, "\0".join(sorted(self.tracked)) + "\0", ""
        if arguments[:1] == ["hash-object"]:
            # The working tree's blob id for a frozen path.
            return 0, self.frozen_blobs.get((self.head, arguments[-1]),
                                            "f" * 40) + "\n", ""
        if arguments[0] == "rev-parse" and ":" in arguments[-1]:
            commit, _, path = arguments[-1].partition(":")
            return 0, self.frozen_blobs.get((commit, path), "f" * 40) + "\n", ""
        if arguments[:1] == ["archive"]:
            assert not self.locked, "candidate source must be staged outside the lock"
            destination = arguments[arguments.index("-o") + 1]
            files.contents[destination] = "tar:" + arguments[-1]
            return 0, "", ""
        raise AssertionError(f"unexpected git command {arguments}")

    # -- docker ------------------------------------------------------------
    def _inspect_containers(self, names):
        lines = []
        documents = self.containers
        for name in names:
            role = next((role for role, value in adapter.CONTAINERS.items() if value == name), None)
            if role is None or role not in documents:
                return 1, "", f"No such object: {name}"
            lines.append(json.dumps(documents[role]))
        return 0, "\n".join(lines) + "\n", ""

    def _resolve(self, reference):
        if reference in self.images:
            return reference
        return self.tags.get(reference)

    def _exec(self, container, arguments, stdin):
        role = next((role for role, name in adapter.CONTAINERS.items() if name == container), None)
        assert role is not None, f"exec into unknown container {container}"
        if arguments[:1] == ["sha256sum"]:
            if role in self.sha256sum_missing:
                return 127, "", "sha256sum: not found"
            path = arguments[1]
            if path != adapter.ARTIFACT_PATHS[role]:
                return 1, "", "No such file"
            value = self.artifact(role)
            return (0, f"{value}  {path}\n", "") if value else (1, "", "No such file")
        if arguments[:1] == ["cat"]:
            assert role == "web" and arguments[1] == "/app/dist/index.html"
            return 0, self.bundle, ""
        if arguments == ["python", "/app/check_health.py"]:
            assert role == "qualify"
            return self.health_code, "probes passed\n", ""
        if arguments == ["python", "-"]:
            assert role == "qualify"
            return self._probe(stdin)
        raise AssertionError(f"unexpected exec {arguments}")

    def _docker(self, arguments, files, stdin):
        if arguments[:2] == ["inspect", "--format"]:
            if arguments[2] == "{{json .}}":
                return self._inspect_containers(arguments[3:])
            if arguments[2] == adapter.BUILDER_LIMITS_FORMAT:
                if not self.builder_present:
                    return 1, "", "no such container"
                return 0, f"{self.builder_memory} {self.builder_nano_cpus} {self.builder_cpu_quota} {self.builder_cpu_period}\n", ""
            raise AssertionError(f"unexpected docker inspect {arguments}")
        if arguments[:3] == ["image", "inspect", "--format"]:
            identity = self._resolve(arguments[4])
            if identity is None:
                return 1, "", "No such image"
            described = self.images[identity]
            if arguments[3] == "{{.Id}}":
                return 0, identity + "\n", ""
            if arguments[3] == "{{json .Config.Env}}":
                return 0, json.dumps([f"{name}={value}"
                                      for name, value in sorted(described["env"].items())]), ""
            if arguments[3] == "{{json .}}":
                return 0, json.dumps({
                    "Id": identity, "Created": described["created"],
                    "RepoDigests": described["digests"],
                    "Config": {"Labels": described["labels"]},
                }), ""
            for label in (adapter.REVISION_LABEL, adapter.EXPERIMENTAL_LABEL):
                if label in arguments[3]:
                    return 0, described["labels"].get(label, "") + "\n", ""
            raise AssertionError(f"unexpected image inspect {arguments}")
        if arguments[:1] == ["tag"]:
            identity = self._resolve(arguments[1])
            assert identity is not None, f"cannot tag unknown image {arguments[1]}"
            assert arguments[2].startswith("stockspotter-rollback-") \
                or arguments[2].startswith("stockspotter-candidate-"), \
                f"this controller must not write the shared tag {arguments[2]}"
            self.tags[arguments[2]] = identity
            return 0, "", ""
        if arguments[:2] == ["buildx", "inspect"]:
            if not self.builder_present:
                return 1, "", "no builder"
            return 0, f"Name: {arguments[2]}\nDriver: {self.builder_driver}\n", ""
        if arguments[:2] == ["buildx", "build"]:
            assert not self.locked, "builds must happen outside the deployment lock"
            labels, built, commit = {}, [], None
            for index, value in enumerate(arguments):
                if value == "--label":
                    name, _, content = arguments[index + 1].partition("=")
                    labels[name] = content
                if value == "--tag":
                    built.append(arguments[index + 1])
                if value == "--build-arg" and arguments[index + 1].startswith("STOCKSPOTTER_COMMIT="):
                    commit = arguments[index + 1].split("=", 1)[1]
            # One build produces one image, however many tags it carries --
            # which is why qualify and discovery-review end up on the same
            # image id, exactly as the shared python/Dockerfile implies.
            identity = image_id("candidate-" + "|".join(sorted(built)))
            environment = dict(IMAGE_ENV)
            if commit is not None:
                environment["STOCKSPOTTER_COMMIT"] = commit
            self.images[identity] = {
                "labels": dict(labels), "env": environment,
                "created": "2026-10-15T01:00:00Z", "digests": [],
                "commit": commit or labels.get(adapter.REVISION_LABEL),
            }
            for tag in built:
                self.tags[tag] = identity
            return 0, "", ""
        if arguments[:1] == ["exec"]:
            offset = 2 if arguments[1] == "-i" else 1
            return self._exec(arguments[offset], arguments[offset + 1:], stdin)
        if arguments[:1] == ["compose"]:
            index = arguments.index("-f")
            return self._compose(arguments[index + 1], arguments[index + 2:], files, stdin)
        raise AssertionError(f"unexpected docker command {arguments}")

    def _compose(self, file, arguments, files, stdin):
        if arguments[:1] == ["config"]:
            assert file == adapter.BASE_COMPOSE_FILE, \
                "only the repository's own compose file is ever parsed"
            assert "--no-interpolate" in arguments, "config must not expand .env"
            if self.compose_config_code != 0:
                return self.compose_config_code, "", "invalid compose file"
            document = self.base_compose_by_commit.get(self.head, self.base_compose)
            return 0, json.dumps(document), ""
        assert file != adapter.BASE_COMPOSE_FILE, (
            "recreation must use the private resolved recipe, never the base compose file: "
            "the base file does not describe the live effective configuration"
        )
        document = json.loads(files.read_text(file))
        self.recipes.append(document)
        if arguments[:1] == ["run"]:
            assert self.locked, "the pre-recreation token check runs under the lock"
            assert adapter.TOKEN_ASSERT in arguments, arguments
            assert "--no-build" in arguments
            environment = dict(self.images[document["services"]["qualify"]["image"]]["env"])
            if document["services"]["qualify"].get("env_file"):
                environment.update(self.dotenv)
            if len(environment.get("STOCKSPOTTER_API_TOKEN", "")) < 32:
                return 1, "", "AssertionError"
            return 0, "", ""
        if arguments[:1] == ["up"]:
            assert self.locked, "recreation must hold the deployment lock"
            assert "--no-build" in arguments, "recreation must never build"
            self.mutations.append("up")
            if self.kill_on_up is not None:
                error, self.kill_on_up = self.kill_on_up, None
                raise error
            self._apply(document, file)
            return 0, "", ""
        raise AssertionError(f"unexpected compose command {arguments}")

    def _apply(self, document, recipe_path):
        """Recreate every service exactly as the compose document describes."""
        for role, service in document["services"].items():
            literal = {}
            for entry in service.get("environment") or []:
                name, _, value = str(entry).partition("=")
                literal[name] = value
            binds = {}
            for entry in service.get("volumes") or []:
                pieces = str(entry).split(":")
                binds[pieces[1]] = pieces[0]
            ports = {}
            for entry in service.get("ports") or []:
                host_ip, host_port, target = str(entry).split(":")
                container_port, _, protocol = target.partition("/")
                ports.setdefault(f"{container_port}/{protocol or 'tcp'}", []).append(
                    {"HostIp": host_ip, "HostPort": host_port})
            self.spec[role] = {
                **self.spec[role],
                "image": service["image"],
                "literal": literal,
                "env_file": bool(service.get("env_file")),
                "ports": ports,
                "binds": binds,
                "restart": service["restart"],
                "logging": {"Type": service["logging"]["driver"],
                            "Config": dict(service["logging"]["options"])},
                "cmd": list(service.get("command") or []) or None,
                "entrypoint": list(service.get("entrypoint") or []) or None,
                "user": service.get("user") or "",
                "working_dir": service.get("working_dir") or "",
                "config_files": recipe_path,
                "restarts": 0,
            }
        self.ws_commit = (self.images[self.spec["ws"]["image"]].get("commit")
                          or self.images[self.spec["ws"]["image"]]["env"].get(
                              "STOCKSPOTTER_COMMIT") or BASELINE_RUNTIME)
        if self.on_up is not None:
            self.on_up()

    def _probe(self, stdin):
        if "research/completeness" in (stdin or ""):
            if self.break_health_on is not None and self.ws_commit == self.break_health_on:
                return 1, "", "probe failed"
            return 0, json.dumps({"commit": self.ws_commit}), ""
        if "auto-trader/status" in (stdin or ""):
            return 0, json.dumps({"openPositions": self.simulated_positions,
                                  "executionMode": "journal"}), ""
        if "paper_endpoint_configured" in (stdin or ""):
            return 0, json.dumps(self.broker), ""
        raise AssertionError("unexpected in-container probe")

    # -- entry point -------------------------------------------------------
    def runner(self, files):
        def run(argv, *, cwd=None, stdin=None, timeout=120):
            self.calls.append(tuple(argv))
            arguments = list(argv)
            if arguments[0] == "git":
                code, out, err = self._git(arguments[3:], files)
            elif arguments[0] == "docker":
                code, out, err = self._docker(arguments[1:], files, stdin)
            elif arguments[0] == "curl":
                code, out, err = 0, self.served, ""
            elif arguments[0] == "systemctl":
                if not self.systemctl_present:
                    raise OSError("systemctl: not found")
                value = self.timer[arguments[1]]
                code = 0 if value in ("enabled", "active") else 1
                code, out, err = code, value + "\n", ""
            elif arguments[0] == "tar":
                destination = Path(arguments[arguments.index("-C") + 1])
                files.dirs.add(str(destination))
                for name in self.archive_extra:
                    files.contents[str(destination / name)] = "x"
                code, out, err = 0, "", ""
            elif arguments[0] == "python3":
                assert arguments[1].endswith("deploy_guard.py")
                code = self.ci_code
                out = (f"RELEASE CHECKS PASS: trusted server jobs succeeded for master at "
                       f"{arguments[3]}\n" if code == 0 else "")
                err = "" if code == 0 else "RELEASE CHECK BLOCKED: not completed\n"
            else:
                raise AssertionError(f"unexpected command {argv}")
            return adapter.Result(tuple(argv), code, out, err)
        return run


class FakeFiles:
    """The controller's own filesystem, as a dictionary."""

    def __init__(self, contents, free=40 * 1024 ** 3):
        self.contents = {str(Path(key)): value for key, value in contents.items()}
        self.dirs = set()
        self.links = set()               # symlinks, including dangling ones
        self.free = free
        self.removed = []
        self.modes = {}

    def owner_uid(self):
        return adapter.Files.owner_uid(self)

    def secure_directory_failures(self, path, *, allow_missing=False):
        key = str(Path(path))
        if key in self.links:
            return ["is a symlink, not a real directory"]
        if key in self.dirs:
            return [] if self.is_private(path) else ["is group/world accessible"]
        if key in self.contents:
            return ["is not a directory"]
        return [] if allow_missing else ["does not exist"]

    def exists(self, path):
        key = str(Path(path))
        return key in self.contents or key in self.dirs

    def present(self, path):
        key = str(Path(path))
        return self.exists(path) or key in self.links

    def real_directory(self, path):
        key = str(Path(path))
        return key in self.dirs and key not in self.links

    def read_text(self, path):
        return self.contents.get(str(Path(path)))

    def digest(self, path):
        value = self.contents.get(str(Path(path)))
        return None if value is None else hashlib.sha256(value.encode()).hexdigest()

    def env_values(self, path):
        return adapter.Files.env_values(self, path)

    def env_pairs(self, path):
        return adapter.Files.env_pairs(self, path)

    def write_private(self, path, text):
        self.contents[str(Path(path))] = text
        self.modes[str(Path(path))] = 0o600

    def make_private_dir(self, path):
        self.dirs.add(str(Path(path)))
        self.modes[str(Path(path))] = 0o700

    def is_private(self, path):
        return self.modes.get(str(Path(path)), 0o700) in (0o600, 0o700)

    def remove_tree(self, path, inside):
        self.removed.append(str(Path(path)))
        self.dirs.discard(str(Path(path)))

    def free_bytes(self, path):
        return self.free


class Harness:
    """A world, a store and a controller wired together."""

    def __init__(self, test, *, now=None, dwell=1):
        directory = tempfile.TemporaryDirectory()
        test.addCleanup(directory.cleanup)
        self.world = World()
        self.control = Path(directory.name)
        self.files = FakeFiles({
            adapter.BOOT_ID_PATH: BOOT_ID,
            ROOT / ".env": dotenv_text(),
            ROOT / "OI-V2-PREREGISTRATION-2026-09-19.md": "preregistration",
            ROOT / "ops/qualify/session.sh": "#!/usr/bin/env bash\n",
            # The legacy state file, as deploy.sh left it. READ-ONLY.
            ROOT / adapter.LEGACY_MARKER_PATH: BASELINE_CHECKOUT + "\n",
            CONTROLLER / "deploy_guard.py": "# installed gate\n",
            CONTROLLER / "release_preflight.py": '"""paper_endpoint_configured"""\n',
        })
        self.world.files = self.files
        self.moment = now or datetime(2026, 10, 15, 1, 30, tzinfo=ZoneInfo(adapter.ZONE))
        self.slept = []
        self.store = core.Store(self.control)
        self.out = io.StringIO()
        self.host = adapter.LinuxHost(
            root=ROOT, controller_root=CONTROLLER, control_dir=self.control, staging=STAGING,
            runner=self.world.runner(self.files), files=self.files,
            locker=self._locker, clock=lambda: self.moment,
            build_locker=lambda: __import__("contextlib").nullcontext(),
            sleeper=self.slept.append, dwell_iterations=dwell, dwell_seconds=5,
        )
        self.controller = adapter.Controller(self.host, self.store, out=self.out)

    @contextmanager
    def _locker(self):
        assert not self.world.locked, "the deployment lock is not reentrant"
        self.world.locked = True
        try:
            yield
        finally:
            self.world.locked = False

    # -- helpers -----------------------------------------------------------
    def adopt(self, experimental=False, decisions=None, exemptions=None, artifacts=None):
        self.controller.adopt(
            confirm=True, experimental=experimental,
            decisions=decisions or {role: "rebuild" for role in core.ROLES},
            exemptions=exemptions or {}, expected_artifacts=artifacts or {})
        return self.store.read("committed.json")["state"]

    def prepare(self):
        self.controller.prepare()
        return self.store.read("prepared.json")

    def rehearse(self):
        self.controller.rehearse(confirm=True)
        return self.store.read(adapter.REHEARSAL_NAME)

    def ready_to_promote(self):
        """Adopt, prepare, rehearse the rollback, prepare again.

        The rehearsal really recreates and really restores, so the recorded
        mutations are cleared afterwards: tests that assert "the promotion
        attempt recreated nothing" are about the promotion, not the setup.
        """
        self.adopt()
        self.prepare()
        self.rehearse()
        prepared = self.prepare()
        self.world.mutations.clear()
        self.world.calls.clear()
        self.world.recipes.clear()
        return prepared

    def recipe(self):
        return json.loads(self.files.read_text(self.control / adapter.BASELINE_RECIPE_NAME))

    def receipts(self):
        return [json.loads(path.read_text(encoding="utf-8"))
                for path in sorted(Path(self.store.directory).glob("receipt-*.json"))]

    def flat_calls(self):
        return [" ".join(call) for call in self.world.calls]

    def legacy_marker_file(self):
        return self.files.read_text(ROOT / adapter.LEGACY_MARKER_PATH)


class ObservationTests(unittest.TestCase):
    def setUp(self):
        self.harness = Harness(self)

    def test_inspect_reports_the_mixed_baseline_without_fetching_or_mutating(self):
        self.assertEqual(self.harness.controller.inspect(), 0)
        report = json.loads(self.harness.out.getvalue())
        self.assertEqual(report["checkout_source"], BASELINE_CHECKOUT)
        self.assertEqual(report["compiled_ws_commit"], BASELINE_RUNTIME)
        self.assertEqual(report["branch"], BASELINE_BRANCH)
        self.assertTrue(report["observed_not_deployed"])
        self.assertEqual(report["known_origin_master"], MASTER_TIP)
        self.assertEqual(report["runtime_invariants"], "ok")
        self.assertTrue(report["exposure_zero"])
        self.assertEqual(self.harness.world.mutations, [])
        self.assertNotIn("git -C /opt/apps/stockspotter fetch --quiet origin master",
                         self.harness.flat_calls())

    def test_inspect_records_a_private_observation_receipt(self):
        self.harness.controller.inspect()
        receipt = self.harness.receipts()[0]
        self.assertEqual(receipt["kind"], "observation")
        self.assertEqual(receipt["observation"]["compiled_ws_commit"], BASELINE_RUNTIME)
        self.assertEqual(receipt["observation"]["adoption"]["committed_receipt"], False)
        self.assertEqual(receipt["observation"]["adoption"]["rollback_rehearsal"], False)

    def test_no_command_or_receipt_ever_carries_a_credential(self):
        self.harness.controller.inspect()
        rendered = json.dumps(self.harness.receipts()) + self.harness.out.getvalue()
        rendered += " ".join(self.harness.flat_calls())
        self.assertNotIn(TOKEN, rendered)
        self.assertNotIn(SECRET, rendered)

    def test_the_two_markers_are_reported_separately_and_the_legacy_one_is_read_only(self):
        report = self.harness.host.observe(include_exposure=False)
        self.assertEqual(report["legacy_marker"], BASELINE_CHECKOUT)
        self.assertEqual(report["marker"], "")          # this controller has none yet
        self.assertFalse(report["legacy_marker_written_by_this_controller"])

    def test_inspect_names_the_legacy_timer_risk_when_the_marker_and_checkout_agree(self):
        """The checkout and the legacy marker agree today, so there is no risk."""
        report = self.harness.host.observe(include_exposure=False)
        self.assertNotIn("legacy_timer_risk", report)
        self.harness.files.write_private(ROOT / adapter.LEGACY_MARKER_PATH, MASTER_TIP)
        report = self.harness.host.observe(include_exposure=False)
        self.assertIn("deploy.sh will rebuild", report["legacy_timer_risk"])

    def test_observation_records_the_overlay_launch_path_of_every_role(self):
        report = self.harness.host.observe(include_exposure=False)
        self.assertEqual(report["compose_labels"]["web"]["config_files"],
                         OVERLAY_FILES["web"])
        self.assertEqual(report["compose_labels"]["ws"]["config_files"],
                         OVERLAY_FILES["ws"])
        self.assertNotEqual(report["compose_labels"]["web"]["config_files"],
                            report["compose_labels"]["ws"]["config_files"])

    def test_observation_records_the_real_data_bind_source_not_an_assumed_one(self):
        report = self.harness.host.observe(include_exposure=False)
        self.assertEqual(report["bind_mounts"]["ws"], [f"{DATA_SOURCE}:/app/data"])
        self.assertEqual(report["bind_mounts"]["web"], [])

    def test_per_role_config_and_env_are_digested_not_disclosed(self):
        report = self.harness.host.observe(include_exposure=False)
        for digest_value in report["role_config_digests"].values():
            self.assertRegex(digest_value, r"^[0-9a-f]{64}$")
        self.assertEqual(set(report["flags"]["ws"]), set(adapter.PARITY_FLAGS))
        self.assertEqual(report["flags"]["ws"]["IAC_ENABLED"], "true")

    def test_effective_environment_is_attributed_per_role_by_name_only(self):
        report = self.harness.host.observe(include_exposure=False)
        ws = report["effective_env"]["ws"]
        self.assertTrue(ws["env_file"])
        self.assertIn("ALPACA_API_SECRET", ws["dotenv_variables"])
        self.assertIn("IAC_ENABLED", ws["literal_variables"])
        self.assertIn("EXTREME_MOMENTUM_ENABLED", ws["literal_variables"])
        self.assertEqual(ws["identity_variables"], ["STOCKSPOTTER_COMMIT"])
        self.assertEqual(sorted(ws["image_variables"]), ["LANG", "PATH"])
        # web has no env_file in the live launch path, so it carries no secret.
        self.assertFalse(report["effective_env"]["web"]["env_file"])
        self.assertEqual(report["effective_env"]["web"]["dotenv_variables"], [])

    def test_provenance_reads_images_artifacts_and_launch_files_not_just_labels(self):
        report = self.harness.host.observe(include_exposure=False)
        provenance = report["provenance"]
        self.assertEqual(provenance["ws"]["image"], image_id("baseline-ws"))
        self.assertEqual(provenance["ws"]["artifact_path"], "/app/ws-server")
        self.assertEqual(provenance["ws"]["artifact_sha256"], self.harness.world.artifact("ws"))
        self.assertIsNone(provenance["ws"]["image_revision_label"])
        self.assertEqual(provenance["ws"]["compose_config_files"], OVERLAY_FILES["ws"])
        self.assertEqual(provenance["compiled_ws_commit"], BASELINE_RUNTIME)
        self.assertEqual(provenance["served_bundle_sha256"], digest(INDEX_HTML))
        self.assertIn("unknown-by-construction", provenance["frontend_build_identity"])

    def test_an_unreadable_artifact_is_reported_as_null_not_guessed(self):
        self.harness.world.sha256sum_missing = {"ws"}
        provenance = self.harness.host.provenance()
        self.assertIsNone(provenance["ws"]["artifact_sha256"])
        self.assertIsNotNone(provenance["web"]["artifact_sha256"])

    def test_unstamped_runtime_cannot_be_a_baseline(self):
        self.harness.world._probe = lambda stdin: (0, json.dumps({"commit": None}), "")
        with self.assertRaises(core.Rejected):
            self.harness.host.snapshot()

    def test_legacy_timer_state_is_observed_and_unknown_when_systemctl_is_unusable(self):
        self.assertEqual(self.harness.host.legacy_timer(), DISARMED_TIMER)
        self.harness.world.systemctl_present = False
        self.assertEqual(self.harness.host.legacy_timer(),
                         {"is-enabled": "unknown", "is-active": "unknown"})


class EnvironmentBindingTests(unittest.TestCase):
    """`env_hash` must bind the whole operator-controlled environment."""

    def setUp(self):
        self.harness = Harness(self)

    def test_overlay_flag_drift_changes_the_environment_hash(self):
        before = self.harness.host.env_digest()
        self.harness.world.spec["ws"]["literal"]["IAC_ENABLED"] = "false"
        self.assertNotEqual(self.harness.host.env_digest(), before)

    def test_a_changed_secret_value_changes_the_environment_hash(self):
        before = self.harness.host.env_digest()
        self.harness.world.dotenv["ALPACA_API_SECRET"] = "rotated"
        self.harness.files.write_private(ROOT / ".env", dotenv_text(self.harness.world.dotenv))
        self.assertNotEqual(self.harness.host.env_digest(), before)

    def test_a_new_overlay_variable_changes_the_environment_hash(self):
        before = self.harness.host.env_digest()
        self.harness.world.spec["ws"]["literal"]["OPPORTUNITY_PROVIDER_URL"] = "http://elsewhere"
        self.assertNotEqual(self.harness.host.env_digest(), before)

    def test_build_identity_and_allowlisted_image_variables_do_not_change_it(self):
        before = self.harness.host.env_digest()
        image = self.harness.world.spec["ws"]["image"]
        self.harness.world.images[image]["env"]["STOCKSPOTTER_COMMIT"] = MASTER_TIP
        self.harness.world.images[image]["env"]["PATH"] = "/usr/bin"
        self.assertEqual(self.harness.host.env_digest(), before)

    def test_an_image_variable_off_the_allowlist_is_bound_not_forgiven(self):
        before = self.harness.host.env_digest()
        image = self.harness.world.spec["ws"]["image"]
        self.harness.world.images[image]["env"]["SOMETHING_NEW"] = "1"
        self.assertNotEqual(self.harness.host.env_digest(), before)

    def test_the_hash_never_consumes_a_cleartext_value(self):
        """Every input is a digest, so the hash cannot leak by reconstruction."""
        effective = self.harness.host._effective(self.harness.world.containers)
        binding = self.harness.host._env_binding(effective)
        rendered = json.dumps(binding)
        self.assertNotIn(SECRET, rendered)
        self.assertNotIn(TOKEN, rendered)
        # Not even non-secret values: every value is reduced to its digest.
        self.assertNotIn("0.0.0.0:8787", rendered)
        self.assertIn(digest("0.0.0.0:8787"), rendered)
        self.assertIn(digest("true"), rendered)

    def test_a_secret_the_overlay_sets_literally_is_refused_not_copied(self):
        self.harness.world.spec["web"]["literal"]["STOCKSPOTTER_API_TOKEN"] = TOKEN
        with self.assertRaises(core.Held) as caught:
            self.harness.host.env_digest()
        self.assertIn("STOCKSPOTTER_API_TOKEN", str(caught.exception))
        self.assertIn("env_file", str(caught.exception))

    def test_a_renamed_copy_of_a_credential_is_refused_too(self):
        self.harness.world.spec["web"]["literal"]["HARMLESS_LOOKING"] = SECRET
        with self.assertRaises(core.Held):
            self.harness.host.env_digest()

    def test_a_container_missing_a_dotenv_variable_cannot_be_reproduced(self):
        """`env_file` would *add* the variable back, so the recipe would not be
        a faithful reproduction. Refused rather than silently changed."""
        self.harness.world.dotenv["BRAND_NEW"] = "1"
        self.harness.files.write_private(ROOT / ".env", dotenv_text(self.harness.world.dotenv))
        self.harness.world.spec["ws"]["drop_env"] = ("BRAND_NEW",)
        with self.assertRaises(core.Held) as caught:
            self.harness.host.env_digest()
        self.assertIn("BRAND_NEW", str(caught.exception))

    def test_the_environment_hash_is_stable_once_the_recipe_declares_the_attribution(self):
        self.harness.adopt()
        before = self.harness.host.env_digest()
        # A rebuilt image that bakes a value the overlay also sets must not
        # change the attribution, or promotion would be impossible.
        image = self.harness.world.spec["ws"]["image"]
        self.harness.world.images[image]["env"]["IAC_ENABLED"] = "true"
        self.assertEqual(self.harness.host.env_digest(), before)


class RuntimeInvariantTests(unittest.TestCase):
    def setUp(self):
        self.harness = Harness(self)

    def change(self, role, **changes):
        self.harness.world.spec[role].update(changes)

    def refuse(self, role, **changes):
        self.change(role, **changes)
        with self.assertRaises(core.Rejected):
            self.harness.host.snapshot()

    def test_bind_mount_outside_the_allowlist_is_refused(self):
        self.refuse("ws", binds={"/app/data": DATA_SOURCE, "/opt/host/etc": "/etc"})

    def test_bind_mount_shadowing_the_compiled_binary_is_refused(self):
        self.refuse("ws", binds={"/app/ws-server": "/tmp/ws-server"})

    def test_web_bind_mount_is_refused_because_the_image_is_the_bundle(self):
        self.refuse("web", binds={"/app/dist": "/tmp/dist"})

    def test_port_published_off_loopback_is_refused(self):
        self.refuse("web", ports={"3000/tcp": [{"HostIp": "0.0.0.0", "HostPort": "3000"}]})

    def test_unbounded_logging_is_refused(self):
        self.refuse("ws", logging={"Type": "json-file", "Config": {}})

    def test_restart_policy_regression_is_refused(self):
        self.refuse("ws", restart="always")

    def test_order_flow_ingestion_must_stay_off(self):
        self.harness.world.spec["ws"]["literal"]["ORDER_FLOW_ENABLED"] = "true"
        with self.assertRaises(core.Rejected):
            self.harness.host.snapshot()

    def test_order_flow_off_values_are_accepted(self):
        self.harness.world.spec["ws"]["literal"]["ORDER_FLOW_ENABLED"] = "0"
        self.assertEqual(self.harness.host.snapshot()["source"], BASELINE_RUNTIME)

    def test_flag_drift_from_the_adopted_policy_is_refused(self):
        self.harness.adopt()
        self.harness.world.spec["ws"]["literal"]["IAC_ENABLED"] = "false"
        with self.assertRaises(core.Rejected):
            self.harness.host.snapshot()

    def test_missing_service_defers_instead_of_guessing(self):
        del self.harness.world.spec["qualify"]
        with self.assertRaises(core.Deferred):
            self.harness.host.snapshot()

    def test_a_container_outside_this_compose_project_cannot_be_adopted(self):
        self.harness.world.spec["ws"]["project"] = "stockspotter-iac"
        with self.assertRaises(core.Held) as caught:
            self.harness.adopt()
        self.assertIn("second stack", str(caught.exception))

    def test_a_foreign_network_cannot_be_reproduced_and_is_refused(self):
        self.harness.world.spec["ws"]["network"] = "iac_bridge"
        with self.assertRaises(core.Held):
            self.harness.adopt()


class BaselineExemptionTests(unittest.TestCase):
    """A stricter candidate rule must not make adopting reality impossible."""

    def setUp(self):
        self.harness = Harness(self)
        self.harness.world.spec["ws"]["logging"] = {"Type": "json-file", "Config": {}}

    def test_an_unbounded_baseline_cannot_be_adopted_without_an_explicit_exemption(self):
        with self.assertRaises(core.Rejected) as caught:
            self.harness.adopt()
        self.assertIn("logging is unbounded", str(caught.exception))

    def test_a_recorded_exemption_lets_the_real_baseline_be_adopted(self):
        adopted = self.harness.adopt(exemptions={"ws": ["unbounded-logs"]})
        self.assertEqual(adopted["source"], BASELINE_RUNTIME)
        policy = adapter.Policy.from_json(self.harness.store.read("policy.json"))
        self.assertEqual(policy.exemptions["ws"], ["unbounded-logs"])
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "adoption")
        self.assertEqual(receipt["policy"]["exemptions"]["ws"], ["unbounded-logs"])

    def test_the_exemption_is_bound_to_the_adopted_image_and_never_a_candidate(self):
        self.harness.adopt(exemptions={"ws": ["unbounded-logs"]})
        policy = adapter.Policy.from_json(self.harness.store.read("policy.json"))
        self.assertTrue(policy.exempt("ws", "unbounded-logs", image_id("baseline-ws")))
        self.assertFalse(policy.exempt("ws", "unbounded-logs", image_id("candidate-ws")))
        self.assertFalse(policy.exempt("web", "unbounded-logs", image_id("baseline-web")))

    def test_an_exempted_baseline_still_refuses_an_unsafe_bind_or_public_port(self):
        self.harness.adopt(exemptions={"ws": ["unbounded-logs"]})
        self.harness.world.spec["ws"]["ports"] = {
            "8787/tcp": [{"HostIp": "0.0.0.0", "HostPort": "8787"}]}
        with self.assertRaises(core.Rejected):
            self.harness.host.snapshot()

    def test_exemptions_outside_the_waivable_set_are_refused_by_the_parser(self):
        with self.assertRaises(SystemExit):
            adapter._pairs(["ws=public-ports"], adapter.EXEMPTABLE, "--baseline-exempt")


class RecipeCaptureTests(unittest.TestCase):
    """The private recipe is the whole rollback guarantee."""

    def setUp(self):
        self.harness = Harness(self)
        self.harness.adopt()
        self.record = self.harness.recipe()

    def test_the_recipe_pins_five_immutable_image_ids(self):
        self.assertEqual(set(self.record["images"]), set(core.ROLES))
        for role, image in self.record["images"].items():
            self.assertEqual(image, image_id(f"baseline-{role}"))
            self.assertRegex(image, r"^sha256:[0-9a-f]{64}$")

    def test_the_recipe_preserves_the_overlay_flags_the_base_compose_file_lacks(self):
        ws = self.record["compose"]["services"]["ws"]
        self.assertIn("IAC_ENABLED=true", ws["environment"])
        self.assertIn("EXTREME_MOMENTUM_ENABLED=true", ws["environment"])
        base = json.dumps(BASE_COMPOSE)
        self.assertNotIn("IAC_ENABLED", base)
        self.assertNotIn("EXTREME_MOMENTUM_ENABLED", base)

    def test_the_recipe_preserves_the_real_bind_source_ports_logging_and_command(self):
        services = self.record["compose"]["services"]
        self.assertEqual(services["ws"]["volumes"], [f"{DATA_SOURCE}:/app/data:rw"])
        self.assertEqual(services["ws"]["ports"],
                         ["127.0.0.1:8787:8787/tcp", "127.0.0.1:8788:8788/tcp"])
        self.assertEqual(services["ws"]["logging"],
                         {"driver": "json-file", "options": {"max-size": "256m", "max-file": "8"}})
        self.assertEqual(services["discovery-review"]["command"],
                         ["python", "/app/discovery_review.py"])
        self.assertEqual(services["web"].get("ports"), ["127.0.0.1:3000:3000/tcp"])
        self.assertNotIn("volumes", services["web"])

    def test_the_recipe_keeps_secrets_in_env_file_and_writes_none_of_them(self):
        services = self.record["compose"]["services"]
        # A POSIX path whatever platform rendered the document: it is consumed
        # by a Linux docker daemon.
        self.assertEqual(services["ws"]["env_file"], ["/opt/apps/stockspotter/.env"])
        self.assertNotIn("env_file", services["web"])
        rendered = json.dumps(self.record)
        self.assertNotIn(SECRET, rendered)
        self.assertNotIn(TOKEN, rendered)

    def test_the_recipe_has_no_build_section_so_a_recreation_cannot_build(self):
        for service in self.record["compose"]["services"].values():
            self.assertNotIn("build", service)

    def test_the_recipe_keeps_the_project_name_so_the_live_containers_are_replaced(self):
        self.assertEqual(self.record["compose"]["name"], adapter.PROJECT)
        self.assertEqual(self.record["project"], adapter.PROJECT)

    def test_the_recipe_is_written_private_and_bound_to_the_adoption_policy(self):
        path = str(self.harness.control / adapter.BASELINE_RECIPE_NAME)
        self.assertEqual(self.harness.files.modes[path], 0o600)
        policy = adapter.Policy.from_json(self.harness.store.read("policy.json"))
        self.assertEqual(self.record["policy_digest"], policy.digest())

    def test_a_recipe_from_a_different_policy_is_refused(self):
        self.harness.files.write_private(
            self.harness.control / adapter.BASELINE_RECIPE_NAME,
            json.dumps({**self.record, "policy_digest": "0" * 64}))
        with self.assertRaises(core.Held) as caught:
            self.harness.host._recipe_record()
        self.assertIn("different adoption policy", str(caught.exception))

    def test_recreating_without_a_captured_recipe_is_refused(self):
        harness = Harness(self)
        with harness.host.lock():
            with self.assertRaises(core.Held) as caught:
                harness.host.up({role: image_id(f"baseline-{role}") for role in core.ROLES})
        self.assertIn("adopt first", str(caught.exception))

    def test_every_path_in_the_recipe_is_posix(self):
        """The document is read by a Linux daemon, not by this machine."""
        for service in self.record["compose"]["services"].values():
            for entry in (service.get("env_file") or []) + (service.get("volumes") or []):
                self.assertNotIn("\\", entry)
                self.assertTrue(entry.startswith("/"), entry)

    def test_the_recipe_records_the_overlay_launch_files_it_replaces(self):
        self.assertEqual(self.record["inventory"]["ws"]["compose_config_files"],
                         OVERLAY_FILES["ws"])
        self.assertEqual(self.record["inventory"]["web"]["compose_config_files"],
                         OVERLAY_FILES["web"])


class MarkerTests(unittest.TestCase):
    """The legacy marker is the one file that must never be written."""

    def setUp(self):
        self.harness = Harness(self)

    def test_adoption_writes_a_private_marker_and_leaves_the_legacy_one_alone(self):
        adopted = self.harness.adopt()
        self.assertEqual(adopted["source"], BASELINE_RUNTIME)
        self.assertEqual(self.harness.host.marker(), BASELINE_RUNTIME)
        self.assertEqual(self.harness.legacy_marker_file(), BASELINE_CHECKOUT + "\n")
        self.assertEqual(self.harness.host.legacy_marker(), BASELINE_CHECKOUT)

    def test_the_private_marker_lives_in_the_private_control_directory(self):
        self.harness.adopt()
        path = str(self.harness.control / adapter.CONTROLLER_MARKER_NAME)
        self.assertEqual(self.harness.files.contents[path], BASELINE_RUNTIME + "\n")
        self.assertEqual(self.harness.files.modes[path], 0o600)

    def test_no_adopt_marker_option_exists_any_more(self):
        parser = adapter.build_parser()
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            parser.parse_args(["adopt", "--adopt-marker", "--experimental-strategies", "false",
                               "--role-decision", "web=rebuild"])

    def test_a_full_promotion_never_writes_the_legacy_marker(self):
        self.harness.ready_to_promote()
        self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.legacy_marker_file(), BASELINE_CHECKOUT + "\n")
        self.assertEqual(self.harness.host.marker(), MASTER_TIP)
        self.assertFalse(any(adapter.LEGACY_MARKER_PATH in call
                             for call in self.harness.flat_calls()))

    def test_a_rollback_never_writes_the_legacy_marker_either(self):
        self.harness.adopt()
        self.harness.prepare()
        self.harness.rehearse()
        self.assertEqual(self.harness.legacy_marker_file(), BASELINE_CHECKOUT + "\n")
        self.assertEqual(self.harness.host.marker(), BASELINE_RUNTIME)

    def test_adoption_receipt_records_the_legacy_marker_read_only(self):
        self.harness.adopt()
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "adoption")
        self.assertEqual(receipt["legacy_marker_read_only"], BASELINE_CHECKOUT)
        self.assertEqual(receipt["controller_marker"], BASELINE_RUNTIME)
        self.assertEqual(receipt["legacy_marker_untouched"], BASELINE_CHECKOUT)


class LegacyTimerGateTests(unittest.TestCase):
    """deploy.sh runs every two minutes; it must be off before a cutover."""

    def setUp(self):
        self.harness = Harness(self)

    def test_promotion_is_refused_while_the_legacy_timer_is_armed(self):
        self.harness.ready_to_promote()
        for timer in ({"is-enabled": "enabled", "is-active": "inactive"},
                      {"is-enabled": "disabled", "is-active": "active"},
                      {"is-enabled": "unknown", "is-active": "unknown"}):
            with self.subTest(timer=timer):
                self.harness.world.timer = dict(timer)
                with self.assertRaises(core.Deferred) as caught:
                    self.harness.controller.promote(confirm=True)
                self.assertIn(adapter.LEGACY_TIMER, str(caught.exception))
                self.assertEqual(self.harness.world.mutations, [])

    def test_the_refusal_explains_the_base_compose_rebuild_risk(self):
        self.harness.ready_to_promote()
        self.harness.world.timer = {"is-enabled": "enabled", "is-active": "active"}
        with self.assertRaises(core.Deferred) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("base compose", str(caught.exception))
        self.assertIn("every two minutes", str(caught.exception))

    def test_a_masked_or_stopped_timer_is_accepted(self):
        self.harness.world.timer = {"is-enabled": "masked", "is-active": "failed"}
        self.harness.ready_to_promote()
        self.assertEqual(self.harness.controller.promote(confirm=True), 0)

    def test_an_unusable_systemctl_is_treated_as_armed(self):
        self.harness.ready_to_promote()
        self.harness.world.systemctl_present = False
        with self.assertRaises(core.Deferred):
            self.harness.controller.promote(confirm=True)

    def test_inspection_and_adoption_are_not_gated_on_the_timer(self):
        self.harness.world.timer = {"is-enabled": "enabled", "is-active": "active"}
        self.assertEqual(self.harness.controller.inspect(exposure=False), 0)
        self.assertEqual(self.harness.adopt()["source"], BASELINE_RUNTIME)

    def test_every_receipt_records_the_timer_state(self):
        self.harness.adopt()
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "adoption")
        self.assertEqual(receipt["legacy_timer"], DISARMED_TIMER)


class RoleDecisionTests(unittest.TestCase):
    """Nothing gets unpinned without a recorded per-role decision."""

    def setUp(self):
        self.harness = Harness(self)

    def test_adoption_requires_a_decision_for_every_role(self):
        with self.assertRaises(core.Deferred) as caught:
            self.harness.controller.adopt(confirm=True, experimental=False,
                                          decisions={"web": "rebuild"}, exemptions={},
                                          expected_artifacts={})
        self.assertIn("--role-decision", str(caught.exception))
        self.assertIsNone(self.harness.store.read("committed.json"))

    def test_a_held_role_blocks_the_build_instead_of_being_silently_unpinned(self):
        decisions = {role: "rebuild" for role in core.ROLES}
        decisions["qualify"] = "hold"
        decisions["discovery-review"] = "hold"
        self.harness.adopt(decisions=decisions)
        with self.assertRaises(core.Held) as caught:
            self.harness.controller.prepare()
        self.assertIn("qualify", str(caught.exception))
        self.assertIn("discovery-review", str(caught.exception))
        self.assertFalse(any("buildx build" in call for call in self.harness.flat_calls()))

    def test_the_decisions_are_recorded_in_the_policy_and_the_receipts(self):
        decisions = {**{role: "rebuild" for role in core.ROLES}, "auto-trader": "hold"}
        self.harness.adopt(decisions=decisions)
        policy = adapter.Policy.from_json(self.harness.store.read("policy.json"))
        self.assertEqual(policy.decisions["auto-trader"], "hold")
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "adoption")
        self.assertEqual(receipt["policy"]["decisions"], decisions)
        self.assertEqual(set(receipt["role_provenance"]) - {"compiled_ws_commit",
                                                            "served_bundle_sha256",
                                                            "frontend_build_identity",
                                                            "decision_binding"},
                         set(core.ROLES))

    def test_an_unknown_decision_is_refused_by_the_parser(self):
        with self.assertRaises(SystemExit):
            adapter._pairs(["web=maybe"], adapter.ROLE_DECISIONS, "--role-decision")
        with self.assertRaises(SystemExit):
            adapter._pairs(["frontend=rebuild"], adapter.ROLE_DECISIONS, "--role-decision")

    def test_provenance_change_after_adoption_blocks_the_build(self):
        self.harness.adopt()
        self.harness.world.artifact_override[("ws", image_id("baseline-ws"))] = "a" * 64
        with self.assertRaises(core.Held) as caught:
            self.harness.controller.prepare()
        self.assertIn("adopt again", str(caught.exception))

    def test_an_asserted_artifact_digest_is_verified_at_adoption(self):
        observed = self.harness.world.artifact("ws")
        self.assertEqual(self.harness.adopt(artifacts={"ws": observed})["source"],
                         BASELINE_RUNTIME)
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "adoption")
        self.assertEqual(receipt["asserted_artifacts"], {"ws": observed})

    def test_a_wrong_asserted_artifact_digest_is_rejected(self):
        with self.assertRaises(core.Rejected) as caught:
            self.harness.adopt(artifacts={"ws": "b" * 64})
        self.assertIn("baked artifact", str(caught.exception))
        self.assertIsNone(self.harness.store.read("committed.json"))

    def test_an_unverifiable_asserted_digest_is_held_not_accepted(self):
        self.harness.world.sha256sum_missing = {"ws"}
        with self.assertRaises(core.Held):
            self.harness.adopt(artifacts={"ws": "b" * 64})


class AdoptionTests(unittest.TestCase):
    def setUp(self):
        self.harness = Harness(self)

    def test_adoption_requires_confirmation(self):
        with self.assertRaises(core.Deferred):
            self.harness.controller.adopt(confirm=False, experimental=False,
                                          decisions={role: "rebuild" for role in core.ROLES},
                                          exemptions={}, expected_artifacts={})
        self.assertIsNone(self.harness.store.read("committed.json"))

    def test_adoption_records_the_mixed_baseline(self):
        adopted = self.harness.adopt()
        self.assertEqual(adopted["source"], BASELINE_RUNTIME)
        self.assertEqual(adopted["checkout_source"], BASELINE_CHECKOUT)
        self.assertEqual(adopted["branch"], BASELINE_BRANCH)
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "adoption")
        self.assertEqual(receipt["provenance"], "observed-not-deployed")

    def test_adoption_pins_the_freeze_and_the_asserted_frontend_flag(self):
        self.harness.adopt(experimental=True)
        policy = adapter.Policy.from_json(self.harness.store.read("policy.json"))
        self.assertTrue(policy.experimental_strategies)
        self.assertEqual(sorted(policy.frozen), sorted(adapter.FROZEN_PATHS))
        self.assertEqual(policy.flags["auto-trader"]["AUTO_TRADER_EXECUTION_MODE"], "journal")
        self.assertEqual(policy.baseline_images["ws"], image_id("baseline-ws"))

    def test_adoption_refuses_a_runtime_that_does_not_prove_itself(self):
        self.harness.world.health_code = 1
        with self.assertRaises(core.Rejected):
            self.harness.adopt()
        self.assertIsNone(self.harness.store.read("committed.json"))

    def test_adoption_refuses_a_different_committed_baseline(self):
        self.harness.adopt()
        identity = image_id("other-web")
        self.harness.world.images[identity] = {"labels": {}, "env": dict(IMAGE_ENV),
                                               "created": "x", "digests": []}
        self.harness.world.spec["web"]["image"] = identity
        with self.assertRaises(core.Held):
            self.harness.adopt()

    def test_adoption_tells_the_operator_promotion_is_still_blocked(self):
        self.harness.adopt()
        self.assertIn("rehearse", self.harness.out.getvalue())

    def test_prepare_refuses_before_any_adoption(self):
        with self.assertRaises(core.Deferred):
            self.harness.controller.prepare()
        self.assertEqual(self.harness.world.calls, [])


class StageBindIsolationTests(unittest.TestCase):
    def test_production_parent_traversal_and_named_volume_are_refused(self):
        owner = Path('/var/tmp/stockspotter-stage-owned')
        for source in ('/opt/apps/stockspotter/data', '/var/tmp/stockspotter-stage-owned/../foreign', 'shared-data'):
            document = {'services': {'ws': {'volumes': [source + ':/app/data:rw']}}}
            self.assertTrue(adapter.stage_bind_failures(document, owner), source)

    def test_an_owned_bind_is_allowed(self):
        document = {'services': {'ws': {'volumes': ['/var/tmp/stockspotter-stage-owned/data:/app/data:rw']}}}
        self.assertEqual(adapter.stage_bind_failures(document, Path('/var/tmp/stockspotter-stage-owned')), [])

    def test_stage_host_requires_an_explicit_owner_root(self):
        with self.assertRaises(core.Held):
            adapter.LinuxHost(project='stockspotter-stage-proof', root=Path('/var/tmp/proof/repo'),
                              control_dir=Path('/var/tmp/proof/store'), staging=Path('/var/tmp/proof/candidate'))

    def test_stage_paths_and_owner_must_agree(self):
        project = 'stockspotter-stage-proof'
        owner = Path('/var/tmp') / project
        kwargs = dict(project=project, stage_owner_root=owner, root=owner / 'repo',
                      control_dir=owner / 'store', staging=owner / 'candidate')
        self.assertTrue(adapter.LinuxHost(**kwargs).stage)
        for change in ({'root': Path('/opt/apps/stockspotter')},
                       {'staging': Path('/var/tmp/foreign')},
                       {'stage_owner_root': Path('/var/tmp')}):
            with self.subTest(change=change), self.assertRaises(core.Held):
                adapter.LinuxHost(**{**kwargs, **change})

    def test_owner_cannot_overlap_the_real_controller_store(self):
        project = 'stockspotter-stage-proof'
        owner = adapter.DEFAULT_STORE / project
        with self.assertRaises(core.Held):
            adapter.LinuxHost(project=project, stage_owner_root=owner, root=owner / 'repo',
                              control_dir=owner / 'store', staging=owner / 'candidate')


class StatInjectedFiles(adapter.Files):
    """A real `Files` with only `lstat`/`owner_uid` answered by the test.

    POSIX ownership and modes cannot be created on every platform this suite
    runs on, and a test that skipped there would leave the foreign-owner and
    insecure-mode branches unproven everywhere. Injecting the two primitives
    exercises the real `secure_directory_failures` logic instead.
    """

    def __init__(self, *, mode=stat.S_IFDIR | 0o700, uid=4242, current=4242, missing=False):
        self.info = None if missing else SimpleNamespace(st_mode=mode, st_uid=uid)
        self.current = current

    def lstat(self, path):
        return self.info

    def owner_uid(self):
        return self.current


class LegacyTimerNamespaceTests(unittest.TestCase):
    """An armed production timer must refuse production and only production.

    The timer runs `deploy.sh` against `/opt/apps/stockspotter` into the
    `stockspotter-vps` project. A stage namespace is outside both, but the
    adapter has to *show* that rather than assume it from `stage` being true --
    and a stage host that cannot be shown to be contained is refused too, so
    the production requirement is answered rather than weakened.
    """

    ARMED = {"is-enabled": "enabled", "is-active": "active"}

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name).resolve()
        self.project = "stockspotter-stage-timerproof"
        self.owner = self.base / self.project
        self.owner.mkdir(mode=0o700)
        self.queried = []

    def runner(self, argv, *, cwd=None, stdin=None, timeout=120):
        argv = tuple(argv)
        assert argv[0] == "systemctl", f"unexpected command {argv}"
        # The one safeguard that matters most here: this adapter may only ever
        # *read* unit state. A stop/disable/mask/start would be a production
        # change made by a stage rehearsal.
        assert argv[1] in ("is-enabled", "is-active"), f"mutating systemctl call {argv}"
        self.queried.append(argv[1])
        return adapter.Result(argv, 0, self.ARMED[argv[1]] + "\n", "")

    def stage_host(self, **changes):
        kwargs = dict(project=self.project, stage_owner_root=self.owner,
                      root=self.owner / "repo", control_dir=self.owner / "control",
                      staging=self.owner / "candidate", runner=self.runner,
                      locker=contextlib.nullcontext, build_locker=contextlib.nullcontext)
        kwargs.update(changes)
        return adapter.LinuxHost(**kwargs)

    def test_an_armed_timer_still_refuses_a_production_promotion(self):
        harness = Harness(self)
        harness.world.timer = {"is-enabled": "enabled", "is-active": "active"}
        with self.assertRaises(core.Deferred) as caught:
            harness.host._require_legacy_timer_disarmed("promotion")
        message = str(caught.exception)
        self.assertIn("promotion refused while", message)
        self.assertIn("systemctl disable --now", message)
        self.assertNotIn("applies", message)

    def test_a_contained_stage_host_proves_non_applicability(self):
        host = self.stage_host()
        proof = host._require_legacy_timer_disarmed("rehearsal")
        self.assertEqual(self.queried, ["is-enabled", "is-active"])
        self.assertIs(proof["applies"], False)
        # The observed production status is recorded, not discarded.
        self.assertEqual((proof["is-enabled"], proof["is-active"]), ("enabled", "active"))
        self.assertIn(str(adapter.PRODUCTION_ROOT), proof["reason"])
        self.assertEqual(host.evidence["legacy_timer_not_applicable"], proof)

    def test_a_stage_host_that_cannot_be_shown_contained_is_refused(self):
        host = self.stage_host()
        host.containers = dict(adapter.CONTAINERS)
        with self.assertRaises(core.Deferred) as caught:
            host._require_legacy_timer_disarmed("rehearsal")
        self.assertIn("cannot be shown to be outside its reach", str(caught.exception))
        self.assertIn("is a production container name", str(caught.exception))

    def test_a_container_outside_the_namespace_is_refused(self):
        host = self.stage_host()
        host.containers = {**host.containers, "ws": "some-other-project-ws-1"}
        problems = host._stage_containment_failures()
        self.assertTrue(any("outside the" in problem for problem in problems), problems)

    def test_a_swapped_owner_root_is_refused(self):
        host = self.stage_host()
        host.files = StatInjectedFiles(mode=stat.S_IFLNK | 0o777)
        with self.assertRaises(core.Deferred):
            host._require_legacy_timer_disarmed("rehearsal")

    def test_a_contained_namespace_reports_no_problems(self):
        self.assertEqual(self.stage_host()._stage_containment_failures(), [])

    def test_recreation_refuses_a_production_container_name(self):
        host = self.stage_host()
        host.containers = dict(adapter.CONTAINERS)
        with self.assertRaises(core.Rejected) as caught:
            host._require_stage_isolation({
                "name": self.project,
                "networks": {"default": {"name": self.project + "_default", "internal": True}},
                "services": {role: {"image": "sha256:" + "0" * 64} for role in core.ROLES},
            })
        self.assertIn("production container name", str(caught.exception))

    def test_this_module_never_mutates_a_unit(self):
        """Grep-level safeguard: no stop/disable/mask/start anywhere in it."""
        source = Path(adapter.__file__).read_text(encoding="utf-8")
        for line in source.splitlines():
            if "systemctl" not in line or line.lstrip().startswith(("#", '"', "*")):
                continue
            for verb in ("stop", "disable", "mask", "start", "enable "):
                self.assertNotIn(f'"{verb}', line, line.strip())


class StageOwnerRootTests(unittest.TestCase):
    """The disposable root's own security, by lstat rather than by name.

    A stage project is only isolated if the directory everything is validated
    against is itself a real, owned, private directory. Matching the *name*
    proves nothing: a symlink named `stockspotter-stage-x` pointing at
    `/opt/apps/stockspotter` would satisfy every containment check, because
    those checks run after `resolve()`.
    """

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name).resolve()
        self.project = "stockspotter-stage-ownercheck"
        self.owner = self.base / self.project
        self.owner.mkdir(mode=0o700)

    def build(self, *, owner=None, files=None):
        owner = self.owner if owner is None else owner
        return adapter.LinuxHost(
            project=self.project, stage_owner_root=owner, root=owner / "repo",
            control_dir=owner / "store", staging=owner / "candidate",
            files=files or adapter.Files(), locker=contextlib.nullcontext,
            build_locker=contextlib.nullcontext,
        )

    def test_a_real_owned_private_directory_is_accepted(self):
        host = self.build()
        host._require_stage_owner_root()
        evidence = host.evidence["stage_owner_root"]
        self.assertEqual(evidence["path"], str(self.owner))
        self.assertIn("real directory, not a symlink", evidence["checks"])
        self.assertEqual(evidence["ownership_verified"], os.name == "posix")

    def test_evidence_does_not_claim_ownership_it_cannot_verify(self):
        host = self.build(files=StatInjectedFiles(current=None))
        host._require_stage_owner_root()
        evidence = host.evidence["stage_owner_root"]
        self.assertFalse(evidence["ownership_verified"])
        self.assertIn("NOT verifiable", evidence["checks"])

    def test_a_foreign_owner_is_refused(self):
        files = StatInjectedFiles(uid=0, current=1000)
        with self.assertRaises(core.Held) as caught:
            self.build(files=files)
        self.assertIn("owned by uid 0, not 1000", str(caught.exception))

    def test_a_group_or_world_accessible_root_is_refused(self):
        files = StatInjectedFiles(mode=stat.S_IFDIR | 0o750)
        with self.assertRaises(core.Held) as caught:
            self.build(files=files)
        self.assertIn("group/world accessible", str(caught.exception))
        self.assertIn("0750", str(caught.exception))

    @unittest.skipUnless(os.name == "posix", "POSIX modes only")
    def test_an_actually_insecure_directory_on_disk_is_refused(self):
        """The same refusal without injection, where the platform allows it."""
        self.owner.chmod(0o755)
        with self.assertRaises(core.Held) as caught:
            self.build()
        self.assertIn("group/world accessible", str(caught.exception))

    def test_a_non_directory_root_is_refused(self):
        with self.assertRaises(core.Held) as caught:
            self.build(files=StatInjectedFiles(mode=stat.S_IFREG | 0o600))
        self.assertIn("is not a directory", str(caught.exception))

    def test_a_symlinked_root_is_refused_before_anything_resolves_it(self):
        target = self.base / "pointed-elsewhere"
        target.mkdir(mode=0o700)
        holder = self.base / "holder"
        holder.mkdir(mode=0o700)
        link = holder / self.project
        try:
            link.symlink_to(target, target_is_directory=True)
        except OSError as error:                        # unprivileged Windows
            self.skipTest(f"cannot create a directory symlink here: {error}")
        with self.assertRaises(core.Held) as caught:
            self.build(owner=link)
        self.assertIn("is a symlink, not a real directory", str(caught.exception))

    def test_a_symlink_is_refused_by_lstat_even_when_it_resolves_correctly(self):
        """`stat` would follow it and report a perfectly good directory."""
        files = StatInjectedFiles(mode=stat.S_IFLNK | 0o777)
        with self.assertRaises(core.Held) as caught:
            self.build(files=files)
        self.assertIn("symlink", str(caught.exception))

    def test_a_root_that_does_not_exist_yet_constructs_but_cannot_be_read(self):
        """Construction has nothing to resolve through; a read does."""
        host = self.build(files=StatInjectedFiles(missing=True))
        with self.assertRaises(core.Held) as caught:
            host._require_stage_owner_root()
        self.assertIn("does not exist", str(caught.exception))

    def test_the_root_is_rechecked_before_a_recipe_is_read(self):
        host = self.build()
        host._require_stage_owner_root()
        host.files = StatInjectedFiles(mode=stat.S_IFLNK | 0o777)
        with self.assertRaises(core.Held):
            host._recipe_record()

    def test_the_root_is_rechecked_before_recreation(self):
        host = self.build()
        host.files = StatInjectedFiles(mode=stat.S_IFLNK | 0o777)
        with self.assertRaises(core.Held):
            host._require_stage_isolation({"name": self.project})

    def test_the_root_is_rechecked_before_the_stage_dotenv_is_read(self):
        host = self.build()
        host.files = StatInjectedFiles(uid=0, current=1000)
        with self.assertRaises(core.Held) as caught:
            host._dotenv()
        self.assertIn("owned by uid 0", str(caught.exception))

    def test_a_stage_capture_inlines_synthetic_values_instead_of_an_env_file(self):
        """The deterministic adoption dead end, closed.

        `_role_env` reports `env_file` in use as soon as the stage `.env`
        digest-matches the containers -- which is exactly what a correct
        synthetic `.env` guarantees -- and then `stage_recipe_failures` refuses
        every `env_file`. Capture therefore inlines the covered values for a
        stage project, and the same guard validates what it wrote.
        """
        host = self.build()
        host.files = FakeFiles({host.root / ".env": "ALPACA_API_SECRET="
                                + adapter.STAGE_MARKER + "-secret\n"})
        covered = {"ALPACA_API_SECRET":
                   adapter._digest_text(adapter.STAGE_MARKER + "-secret")}
        inlined = host._stage_dotenv_values("ws", covered)
        self.assertEqual(inlined,
                         {"ALPACA_API_SECRET": adapter.STAGE_MARKER + "-secret"})
        document = {
            "name": self.project,
            "networks": {"default": {"name": self.project + "_default", "internal": True}},
            "services": {role: {"image": "sha256:" + "0" * 64,
                                "environment": [f"{name}={value}"
                                                for name, value in inlined.items()]}
                         for role in core.ROLES},
        }
        self.assertEqual(adapter.stage_recipe_failures(document, self.project), [])

    def test_a_stage_capture_refuses_to_inline_a_real_looking_credential(self):
        host = self.build()
        host.files = FakeFiles({host.root / ".env": "ALPACA_API_SECRET=a-real-secret\n"})
        covered = {"ALPACA_API_SECRET": adapter._digest_text("a-real-secret")}
        inlined = host._stage_dotenv_values("ws", covered)
        document = {
            "name": self.project,
            "networks": {"default": {"name": self.project + "_default", "internal": True}},
            "services": {"ws": {"environment": [f"ALPACA_API_SECRET={inlined['ALPACA_API_SECRET']}"]}},
        }
        problems = adapter.stage_recipe_failures(document, self.project)
        self.assertTrue(any("may be a real credential" in problem for problem in problems),
                        problems)

    def test_inlining_dotenv_values_is_refused_for_production(self):
        harness = Harness(self)
        self.assertFalse(harness.host.stage)
        with self.assertRaises(core.Held) as caught:
            harness.host._stage_dotenv_values("ws", {})
        self.assertIn("refusing to inline", str(caught.exception))

    def test_a_dotenv_value_that_changed_during_capture_is_refused(self):
        host = self.build()
        host.files = FakeFiles({host.root / ".env": "ALPACA_API_SECRET=changed\n"})
        with self.assertRaises(core.Held) as caught:
            host._stage_dotenv_values("ws", {"ALPACA_API_SECRET": "0" * 64})
        self.assertIn("changed during capture", str(caught.exception))
        with self.assertRaises(core.Held) as caught:
            host._stage_dotenv_values("ws", {"ABSENT_NAME": "0" * 64})
        self.assertIn("left the stage .env", str(caught.exception))

    def test_the_exact_paper_endpoint_is_not_treated_as_a_credential(self):
        document = {
            "name": self.project,
            "networks": {"default": {"name": self.project + "_default", "internal": True}},
            "services": {"qualify": {"environment": [
                "ALPACA_TRADING_BASE=https://paper-api.alpaca.markets",
                f"ALPACA_API_KEY={adapter.STAGE_MARKER}-key"]}},
        }
        self.assertEqual(adapter.stage_recipe_failures(document, self.project), [])
        for impostor in ("https://user:password@paper-api.alpaca.markets",
                         "https://paper-api.alpaca.markets.evil.invalid",
                         "https://paper-api.alpaca.markets/",
                         "http://paper-api.alpaca.markets"):
            with self.subTest(impostor=impostor):
                document["services"]["qualify"]["environment"][0] = (
                    "ALPACA_TRADING_BASE=" + impostor)
                self.assertTrue(adapter.stage_recipe_failures(document, self.project),
                                f"{impostor} must not receive the exact-endpoint exception")

    def test_the_endpoint_exception_covers_no_other_variable(self):
        self.assertEqual(set(adapter.STAGE_ENDPOINT_EXEMPT), {"ALPACA_TRADING_BASE"})
        document = {
            "name": self.project,
            "networks": {"default": {"name": self.project + "_default", "internal": True}},
            "services": {"qualify": {"environment": [
                "ALPACA_API_KEY=https://paper-api.alpaca.markets"]}},
        }
        self.assertTrue(adapter.stage_recipe_failures(document, self.project))

    def test_the_production_command_line_is_unchanged_by_the_stage_flags(self):
        """Omitting them must reproduce the previous construction exactly."""
        arguments = adapter.build_parser().parse_args(["inspect"])
        self.assertEqual(arguments.project, adapter.PROJECT)
        self.assertIsNone(arguments.stage_owner_root)
        self.assertEqual(arguments.root, adapter.PRODUCTION_ROOT)
        self.assertEqual(arguments.store, adapter.DEFAULT_STORE)

    def test_a_stage_namespace_is_drivable_from_the_real_command_line(self):
        arguments = adapter.build_parser().parse_args([
            "--project", self.project, "--stage-owner-root", str(self.owner),
            "--root", str(self.owner / "repo"), "--store", str(self.owner / "control"),
            "--staging", str(self.owner / "candidate"), "rehearse", "--confirm"])
        self.assertEqual(arguments.project, self.project)
        self.assertEqual(Path(arguments.stage_owner_root), self.owner)
        host = adapter.LinuxHost(
            root=arguments.root, control_dir=arguments.store, staging=arguments.staging,
            project=arguments.project, stage_owner_root=arguments.stage_owner_root,
            locker=contextlib.nullcontext, build_locker=contextlib.nullcontext)
        self.assertTrue(host.stage)
        self.assertEqual(host._stage_containment_failures(), [])

    def test_production_keeps_its_behaviour_exactly(self):
        """No owner root, no check, no evidence: the production CLI is immutable."""
        harness = Harness(self)
        self.assertFalse(harness.host.stage)
        harness.host._require_stage_owner_root()
        self.assertNotIn("stage_owner_root", harness.host.evidence)
        harness.adopt()
        self.assertNotIn("stage_owner_root", harness.host.evidence)


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.harness = Harness(self)
        self.harness.adopt()

    def test_watch_reuses_an_exact_prepared_candidate_without_building_again(self):
        self.harness.controller.watch()
        initial = sum("buildx build" in call for call in self.harness.flat_calls())
        self.assertEqual(initial, 4)  # The two Python roles share a build.
        self.harness.controller.watch()
        self.assertEqual(sum("buildx build" in call for call in self.harness.flat_calls()), initial)
        self.assertEqual(self.harness.world.mutations, [])

    def test_watch_rebuilds_after_host_reboot(self):
        self.harness.controller.watch()
        self.harness.files.write_private(adapter.BOOT_ID_PATH, "another-boot")
        self.harness.controller.watch()
        self.assertEqual(sum("buildx build" in call for call in self.harness.flat_calls()), 8)

    def test_watch_rebuilds_an_expired_candidate(self):
        self.harness.controller.watch()
        self.harness.moment += timedelta(hours=13)
        self.harness.controller.watch()
        self.assertEqual(sum("buildx build" in call for call in self.harness.flat_calls()), 8)

    def test_watch_refuses_a_changed_baseline_without_building(self):
        self.harness.controller.watch()
        self.harness.world.spec["web"]["image"] = self.harness.store.read("prepared.json")["prepared"]["state"]["images"]["web"]
        with self.assertRaises((core.Deferred, core.Held)):
            self.harness.controller.watch()
        self.assertEqual(sum("buildx build" in call for call in self.harness.flat_calls()), 4)

    def test_busy_build_lock_does_not_start_a_second_build(self):
        @contextmanager
        def busy():
            raise core.Deferred("another preparation owns the build lock")
            yield
        self.harness.host.build_locker = busy
        with self.assertRaises(core.Deferred):
            self.harness.controller.watch()
        self.assertFalse(any("buildx build" in call for call in self.harness.flat_calls()))

    def test_candidate_is_built_from_a_tracked_only_archive_with_bounded_builder(self):
        self.assertEqual(self.harness.controller.prepare(), 0)
        calls = self.harness.flat_calls()
        self.assertTrue(any(f"archive --format=tar -o {STAGING / (MASTER_TIP + '.tar')} {MASTER_TIP}"
                            in call for call in calls))
        builds = [call for call in calls if "buildx build" in call]
        # Four Dockerfiles for five roles: qualify and discovery-review share
        # python/Dockerfile, so that image is built once and tagged twice.
        self.assertEqual(len(builds), 4)
        for call in builds:
            self.assertIn(f"--builder {self.harness.host.builder}", call)
            self.assertIn(f"--label {adapter.REVISION_LABEL}={MASTER_TIP}", call)
        prepared = self.harness.store.read("prepared.json")["prepared"]["state"]["images"]
        self.assertEqual(prepared["qualify"], prepared["discovery-review"])
        self.assertTrue(any(f"--build-arg STOCKSPOTTER_COMMIT={MASTER_TIP}" in call
                            for call in builds))
        self.assertTrue(any("--build-arg VITE_EXPERIMENTAL_STRATEGIES=false" in call
                            for call in builds))

    def test_the_adopted_frontend_flag_is_what_gets_compiled(self):
        harness = Harness(self)
        harness.adopt(experimental=True)
        harness.controller.prepare()
        builds = [call for call in harness.flat_calls() if "buildx build" in call]
        self.assertTrue(any("--build-arg VITE_EXPERIMENTAL_STRATEGIES=true" in call
                            for call in builds))
        self.assertTrue(any(f"--label {adapter.EXPERIMENTAL_LABEL}=true" in call
                            for call in builds))

    def test_preparation_never_touches_the_running_stack(self):
        self.harness.controller.prepare()
        self.assertEqual(self.harness.world.mutations, [])
        self.assertEqual(self.harness.world.branch, BASELINE_BRANCH)
        self.assertEqual(self.harness.host.marker(), BASELINE_RUNTIME)

    def test_rollback_images_are_retained_under_a_private_tag_only(self):
        self.harness.controller.prepare()
        for role in core.ROLES:
            self.assertEqual(
                self.harness.world.tags[f"stockspotter-rollback-{role}:{BASELINE_RUNTIME}"],
                image_id(f"baseline-{role}"))
        self.assertFalse(any("prune" in call for call in self.harness.flat_calls()))
        # Nothing the legacy path reads was written.
        self.assertFalse(any(f"{adapter.PROJECT}-{role}:latest" in call
                             for role in core.ROLES
                             for call in self.harness.flat_calls()))

    def test_prepared_record_binds_the_boot_id_and_the_gate_evidence(self):
        self.harness.controller.prepare()
        prepared = self.harness.store.read("prepared.json")
        self.assertEqual(prepared["boot_id"], BOOT_ID)
        self.assertIn("RELEASE CHECKS PASS", prepared["server_ci_gate"])
        self.assertIn("not-evaluated-by-this-controller", prepared["android_gate"])
        self.assertEqual(prepared["prepared"]["state"]["source"], MASTER_TIP)
        self.assertEqual(prepared["prepared"]["baseline_state"]["source"], BASELINE_RUNTIME)
        self.assertEqual(prepared["build"]["decisions"],
                         {role: "rebuild" for role in core.ROLES})

    def test_unprovisioned_or_unbounded_builder_defers_without_provisioning_it(self):
        for change in ({"builder_present": False}, {"builder_memory": 4 * 1024 ** 3},
                       {"builder_cpu_quota": 0}, {"builder_cpu_quota": 200000},
                       {"builder_cpu_period": 0}, {"builder_nano_cpus": 1000000000},
                       {"builder_driver": "docker"}):
            with self.subTest(change=change):
                harness = Harness(self)
                harness.adopt()
                for name, value in change.items():
                    setattr(harness.world, name, value)
                with self.assertRaises(core.Deferred):
                    harness.controller.prepare()
                self.assertFalse(any("buildx create" in call for call in harness.flat_calls()))

    def test_insufficient_headroom_defers_and_never_prunes(self):
        self.harness.files.free = 2 * 1024 ** 3
        with self.assertRaises(core.Deferred):
            self.harness.controller.prepare()
        self.assertFalse(any("buildx build" in call for call in self.harness.flat_calls()))
        self.assertFalse(any("prune" in call for call in self.harness.flat_calls()))

    def test_candidate_tree_carrying_env_or_git_is_rejected(self):
        for name in (".env", ".git"):
            with self.subTest(name=name):
                harness = Harness(self)
                harness.adopt()
                harness.world.archive_extra = (name,)
                with self.assertRaises(core.Rejected):
                    harness.controller.prepare()

    def test_missing_controller_gate_is_held_with_no_checkout_fallback(self):
        del self.harness.files.contents[str(CONTROLLER / "deploy_guard.py")]
        with self.assertRaises(core.Held):
            self.harness.controller.prepare()
        self.assertFalse(any("buildx build" in call for call in self.harness.flat_calls()))

    def test_unready_server_ci_defers_and_non_ancestor_master_is_rejected(self):
        self.harness.world.ci_code = 1
        with self.assertRaises(core.Deferred):
            self.harness.controller.prepare()
        self.harness.world.ci_code, self.harness.world.ancestor_ok = 0, False
        with self.assertRaises(core.Rejected):
            self.harness.controller.prepare()
        self.assertFalse(any("buildx build" in call for call in self.harness.flat_calls()))

    def test_a_private_marker_that_names_the_checkout_instead_of_the_runtime_defers(self):
        self.harness.files.write_private(
            self.harness.control / adapter.CONTROLLER_MARKER_NAME, BASELINE_CHECKOUT + "\n")
        with self.assertRaises(core.Deferred):
            self.harness.controller.prepare()

    def test_preparation_requires_a_captured_recipe(self):
        del self.harness.files.contents[str(self.harness.control / adapter.BASELINE_RECIPE_NAME)]
        with self.assertRaises(core.Held):
            self.harness.controller.prepare()


class BaseCompatibilityTests(unittest.TestCase):
    """The recipe is replayed, so a candidate that redefines the contract
    must be refused rather than silently ignored."""

    def setUp(self):
        self.harness = Harness(self, dwell=1)
        self.harness.ready_to_promote()
        # The candidate commit's own compose file, as that checkout resolves it.
        self.candidate = json.loads(json.dumps(BASE_COMPOSE))
        self.harness.world.base_compose_by_commit[MASTER_TIP] = self.candidate

    def test_a_compatible_candidate_promotes_and_records_the_preflight(self):
        self.assertEqual(self.harness.controller.promote(confirm=True), 0)
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "promotion")
        self.assertEqual(receipt["base_compatibility"]["ws"]["published"],
                         ["8787:8787", "8788:8788"])
        self.assertIn("/app/data", receipt["base_compatibility"]["ws"]["binds"])

    def test_a_candidate_that_publishes_a_new_port_is_rejected_and_rolled_back(self):
        self.candidate["services"]["qualify"]["ports"] = [
            {"target": 8000, "published": "8000", "protocol": "tcp", "host_ip": "127.0.0.1"}]
        with self.assertRaises(core.Rejected) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("publishes", str(caught.exception))
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(self.harness.world.branch, BASELINE_BRANCH)

    def test_a_candidate_that_adds_a_bind_mount_is_rejected(self):
        self.candidate["services"]["web"]["volumes"] = [
            {"type": "bind", "source": "../../data", "target": "/app/data"}]
        with self.assertRaises(core.Rejected) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("mounts", str(caught.exception))

    def test_a_candidate_that_changes_a_service_command_is_rejected(self):
        self.candidate["services"]["discovery-review"]["command"] = [
            "python", "/app/discovery_review_v2.py"]
        with self.assertRaises(core.Rejected) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("command", str(caught.exception))

    def test_a_candidate_declaring_a_variable_the_live_config_lacks_is_rejected(self):
        self.candidate["services"]["ws"]["environment"].append(
            "NEW_PROVIDER_URL=http://example")
        with self.assertRaises(core.Rejected) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("NEW_PROVIDER_URL", str(caught.exception))
        self.assertIn("adopt a new baseline", str(caught.exception))

    def test_a_variable_the_candidate_declares_and_dotenv_supplies_is_fine(self):
        self.candidate["services"]["ws"]["environment"].append(
            "ALPACA_API_KEY=")
        self.assertEqual(self.harness.controller.promote(confirm=True), 0)

    def contract(self, relax=()):
        """`_base_compatibility` against the live recipe, directly."""
        host = self.harness.host
        self.harness.world.base_compose = self.candidate
        compose = host._target_recipe(host._recipe_record()["images"])
        return host._base_compatibility(compose, relax=relax)

    def test_production_relaxes_nothing_by_default(self):
        """The default argument is the production contract, unchanged."""
        self.candidate["services"]["web"]["ports"] = [
            {"target": 9999, "published": "9999", "protocol": "tcp", "host_ip": "127.0.0.1"}]
        with self.assertRaises(core.Rejected) as caught:
            self.contract()
        self.assertIn("publishes", str(caught.exception))

    def test_relaxing_ports_and_command_leaves_the_other_comparisons_running(self):
        """Exactly the two the stage mandates away, and not one more."""
        self.candidate["services"]["web"]["ports"] = [
            {"target": 9999, "published": "9999", "protocol": "tcp", "host_ip": "127.0.0.1"}]
        self.candidate["services"]["discovery-review"]["command"] = ["python", "/other.py"]
        self.contract(relax=("ports", "command"))        # must not raise

        self.candidate["services"]["web"]["volumes"] = [
            {"type": "bind", "source": "../../data", "target": "/app/elsewhere"}]
        with self.assertRaises(core.Rejected) as caught:
            self.contract(relax=("ports", "command"))
        self.assertIn("mounts", str(caught.exception))

    def test_relaxation_does_not_excuse_a_new_declared_variable(self):
        self.candidate["services"]["ws"]["environment"].append("NEW_PROVIDER_URL=http://x")
        with self.assertRaises(core.Rejected) as caught:
            self.contract(relax=("ports", "command"))
        self.assertIn("NEW_PROVIDER_URL", str(caught.exception))

    def test_relaxation_does_not_excuse_a_changed_service_set(self):
        del self.candidate["services"]["qualify"]
        with self.assertRaises(core.Rejected) as caught:
            self.contract(relax=("ports", "command"))
        self.assertIn("not the five live services", str(caught.exception))

    def test_the_adopted_recipe_records_the_launch_contract_blob(self):
        record = self.harness.host._recipe_record()
        self.assertRegex(record["base_compose_blob"], r"^[0-9a-f]{40}$")

    def test_a_candidate_whose_compose_file_does_not_resolve_is_rolled_back(self):
        self.harness.world.compose_config_code = 1
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        # The restoration itself is never preflighted, so it still completes.
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(self.harness.world.branch, BASELINE_BRANCH)
        self.assertIsNone(self.harness.store.read("journal.json"))

    def test_restoring_the_baseline_is_never_gated_on_the_compose_contract(self):
        """A preflight that could refuse a rollback would defeat its purpose."""
        self.harness.world.break_health_on = MASTER_TIP
        self.candidate["services"]["qualify"]["ports"] = [
            {"target": 8000, "published": "8000", "protocol": "tcp", "host_ip": "127.0.0.1"}]
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(self.harness.world.spec["ws"]["literal"]["IAC_ENABLED"], "true")
        self.assertIsNone(self.harness.store.read("journal.json"))

    def test_a_base_compose_file_describing_other_services_is_rejected(self):
        del self.candidate["services"]["qualify"]
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)


class PromotionTests(unittest.TestCase):
    def setUp(self):
        self.harness = Harness(self, dwell=3)
        self.harness.ready_to_promote()

    def test_promotion_commits_only_after_a_full_dwell_proof(self):
        self.assertEqual(self.harness.controller.promote(confirm=True), 0)
        world = self.harness.world
        self.assertEqual(world.branch, "master")
        self.assertEqual(world.head, MASTER_TIP)
        self.assertEqual(world.ws_commit, MASTER_TIP)
        self.assertEqual(self.harness.host.marker(), MASTER_TIP)
        committed = self.harness.store.read("committed.json")["state"]
        self.assertEqual(committed["source"], MASTER_TIP)
        self.assertEqual(committed["checkout_source"], MASTER_TIP)
        self.assertIsNone(self.harness.store.read("journal.json"))
        self.assertIsNone(self.harness.store.read("prepared.json"))
        self.assertGreaterEqual(len(self.harness.slept), 2)   # the dwell really waited

    def test_promotion_preserves_the_whole_effective_configuration(self):
        self.harness.controller.promote(confirm=True)
        spec = self.harness.world.spec
        self.assertEqual(spec["ws"]["literal"]["IAC_ENABLED"], "true")
        self.assertEqual(spec["ws"]["literal"]["EXTREME_MOMENTUM_ENABLED"], "true")
        self.assertEqual(spec["ws"]["binds"], {"/app/data": DATA_SOURCE})
        self.assertEqual(spec["ws"]["logging"]["Config"]["max-size"], "256m")
        self.assertTrue(spec["ws"]["env_file"])
        self.assertFalse(spec["web"]["env_file"])
        self.assertEqual(spec["discovery-review"]["cmd"],
                         ["python", "/app/discovery_review.py"])
        self.assertEqual(spec["web"]["ports"],
                         {"3000/tcp": [{"HostIp": "127.0.0.1", "HostPort": "3000"}]})

    def test_recreation_uses_the_private_recipe_with_the_candidate_images(self):
        prepared = self.harness.store.read("prepared.json")["prepared"]["state"]["images"]
        self.harness.controller.promote(confirm=True)
        ups = [call for call in self.harness.flat_calls() if " up -d" in call]
        self.assertTrue(ups)
        for call in ups:
            self.assertIn("--no-build", call)
            self.assertIn("--wait", call)
            self.assertIn(str(self.harness.control / adapter.ACTIVE_RECIPE_NAME), call)
            self.assertNotIn(adapter.BASE_COMPOSE_FILE, call)
        promoted = [document for document in self.harness.world.recipes
                    if document["services"]["ws"]["image"] == prepared["ws"]]
        self.assertTrue(promoted)
        self.assertIn("IAC_ENABLED=true", promoted[-1]["services"]["ws"]["environment"])

    def test_no_shared_service_tag_is_ever_written(self):
        self.harness.controller.promote(confirm=True)
        self.assertFalse(any(call.startswith("docker tag") and ":latest" in call
                             for call in self.harness.flat_calls()))

    def test_promotion_requires_confirmation(self):
        with self.assertRaises(core.Deferred):
            self.harness.controller.promote(confirm=False)
        self.assertEqual(self.harness.world.mutations, [])

    def test_reboot_after_preparation_defers_the_candidate(self):
        self.harness.files.write_private(adapter.BOOT_ID_PATH, "another-boot-id")
        with self.assertRaises(core.Deferred):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.mutations, [])

    def test_stale_preparation_defers_the_candidate(self):
        self.harness.moment = self.harness.moment + timedelta(hours=13)
        with self.assertRaises(core.Deferred):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.mutations, [])

    def test_outside_the_overnight_window_nothing_is_recreated(self):
        for hour in (10, 19, 3):                 # trading hours, pre-window, too close to 04:00
            with self.subTest(hour=hour):
                self.harness.moment = datetime(2026, 10, 15, hour, 30,
                                               tzinfo=ZoneInfo(adapter.ZONE))
                with self.assertRaises(core.Deferred):
                    self.harness.controller.promote(confirm=True)
                self.assertEqual(self.harness.world.mutations, [])

    def test_window_is_open_overnight_and_closed_in_the_session(self):
        host = self.harness.host
        for hour, expected in ((20, True), (23, True), (1, True), (3, False), (9, False),
                               (16, False)):
            self.harness.moment = datetime(2026, 10, 15, hour, 30, tzinfo=ZoneInfo(adapter.ZONE))
            self.assertEqual(host.allowed(), expected, hour)

    def test_broker_exposure_blocks_promotion_before_anything_is_recreated(self):
        for change in ({"positions": 1}, {"open_orders": 2}, {"paper_endpoint_configured": False},
                       {"positions": False}, {"open_orders": "0"},
                       {"account_active": False}, {"api_token_present_and_long_enough": False},
                       {"trading_blocked": True}):
            with self.subTest(change=change):
                original = dict(self.harness.world.broker)
                self.harness.world.broker.update(change)
                with self.assertRaises(core.Deferred):
                    self.harness.controller.promote(confirm=True)
                self.assertEqual(self.harness.world.mutations, [])
                self.harness.world.broker = original

    def test_simulated_open_position_blocks_promotion(self):
        self.harness.world.simulated_positions = 1
        with self.assertRaises(core.Deferred):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.mutations, [])

    def test_failed_candidate_health_restores_images_checkout_branch_and_marker(self):
        self.harness.world.break_health_on = MASTER_TIP
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        world = self.harness.world
        self.assertEqual(world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual({role: world.spec[role]["image"] for role in core.ROLES},
                         {role: image_id(f"baseline-{role}") for role in core.ROLES})
        self.assertEqual(world.branch, BASELINE_BRANCH)
        self.assertEqual(world.head, BASELINE_CHECKOUT)
        self.assertEqual(self.harness.host.marker(), BASELINE_RUNTIME)
        self.assertIsNone(self.harness.store.read("journal.json"))
        self.assertEqual(self.harness.store.read("committed.json")["state"]["source"],
                         BASELINE_RUNTIME)

    def test_a_rollback_restores_the_overlay_configuration_exactly(self):
        self.harness.world.break_health_on = MASTER_TIP
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        spec = self.harness.world.spec
        self.assertEqual(spec["ws"]["literal"]["IAC_ENABLED"], "true")
        self.assertEqual(spec["ws"]["literal"]["EXTREME_MOMENTUM_ENABLED"], "true")
        self.assertEqual(spec["ws"]["binds"], {"/app/data": DATA_SOURCE})
        self.assertEqual(spec["ws"]["logging"]["Config"]["max-size"], "256m")
        self.assertEqual(spec["web"]["literal"], OVERLAY_ENV["web"])
        self.assertEqual(self.harness.host.env_digest(),
                         self.harness.store.read("committed.json")["state"]["env_hash"])

    def test_a_rollback_verifies_the_restored_baked_artifact(self):
        """The baseline image id is back, but its bytes are not the adopted
        ones: a tag that points somewhere else is not a restoration."""
        world = self.harness.world
        world.break_health_on = MASTER_TIP

        def corrupt():
            if world.spec["ws"]["image"] == image_id("baseline-ws"):
                world.artifact_override[("ws", image_id("baseline-ws"))] = "c" * 64
        world.on_up = corrupt
        with self.assertRaises(core.Held) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("Restoration did not verify", str(caught.exception))
        self.assertEqual(self.harness.store.read("journal.json")["phase"], "held")

    def test_served_bundle_that_differs_from_the_image_is_rejected_and_rolled_back(self):
        self.harness.world.stale_bundle_on = MASTER_TIP
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(self.harness.world.branch, BASELINE_BRANCH)
        self.assertIsNone(self.harness.store.read("journal.json"))

    def test_a_candidate_image_that_changes_the_environment_rolls_back(self):
        """A rebuilt image that bakes a variable off the allowlist fails the
        proof -- and must roll back, not hold with the candidate live."""
        harness = Harness(self, dwell=1)
        harness.ready_to_promote()
        candidate = harness.store.read("prepared.json")["prepared"]["state"]["images"]["ws"]
        harness.world.images[candidate]["env"]["UNEXPECTED_PROVIDER"] = "http://elsewhere"
        with self.assertRaises(core.Rejected):
            harness.controller.promote(confirm=True)
        self.assertEqual(harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(harness.world.spec["ws"]["image"], image_id("baseline-ws"))
        self.assertIsNone(harness.store.read("journal.json"))

    def test_crossing_the_hard_stop_during_the_dwell_rolls_back(self):
        """04:00 ET is a hard stop, and it forces the rollback it implies."""
        self.harness = Harness(self, dwell=3,
                               now=datetime(2026, 10, 14, 23, 0, tzinfo=ZoneInfo(adapter.ZONE)))
        self.harness.ready_to_promote()
        original = self.harness.host.sleeper

        def advance(seconds):
            """The dwell crosses 04:00 while the candidate is the live build."""
            original(seconds)
            if self.harness.world.ws_commit == MASTER_TIP:
                self.harness.moment = datetime(2026, 10, 15, 4, 5,
                                               tzinfo=ZoneInfo(adapter.ZONE))
        self.harness.host.sleeper = advance
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        # The candidate was live and is gone again; the restored baseline was
        # still allowed to prove itself after the window closed.
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(self.harness.world.branch, BASELINE_BRANCH)
        self.assertEqual(self.harness.host.marker(), BASELINE_RUNTIME)
        self.assertIsNone(self.harness.store.read("journal.json"))

    def test_the_hard_stop_never_blocks_a_baseline_proof(self):
        """Adoption, reconciliation and rollback proof are not clock-gated.

        Codex's review read `health()` as refusing outside 20:00-04:00, which
        would have made adoption impossible during the day and every rollback
        after 04:00 end `Held` instead of verified. The clock is consulted only
        while the *candidate* is the live build, so this passes at 11:00.
        """
        harness = Harness(self, now=datetime(2026, 10, 15, 11, 0, tzinfo=ZoneInfo(adapter.ZONE)))
        self.assertFalse(harness.host.allowed())
        self.assertEqual(harness.adopt()["source"], BASELINE_RUNTIME)
        self.assertEqual(harness.controller.reconcile(), 0)
        self.assertEqual(harness.host.health()["ws_commit"], BASELINE_RUNTIME)
        harness.host.deadline = datetime(2026, 10, 15, 4, 0, tzinfo=ZoneInfo(adapter.ZONE))
        harness.host.candidate_source = MASTER_TIP
        # Still fine: the live build is the baseline, not the candidate.
        self.assertEqual(harness.host.health()["ws_commit"], BASELINE_RUNTIME)

    def test_candidate_that_would_change_frozen_content_never_moves_the_checkout(self):
        """`master`'s ops/qualify/session.sh differs: refuse before moving."""
        harness = Harness(self)
        harness.adopt()
        harness.world.frozen_blobs[(MASTER_TIP, "ops/qualify/session.sh")] = "a" * 40
        with self.assertRaises(core.Rejected):
            harness.controller.prepare()
        self.assertFalse(any("buildx build" in call for call in harness.flat_calls()))
        self.assertEqual(harness.world.mutations, [])
        self.assertEqual(harness.world.branch, BASELINE_BRANCH)

    def test_frozen_divergence_discovered_after_preparation_restores_the_baseline(self):
        self.harness.world.frozen_blobs[
            (MASTER_TIP, "OI-V2-PREREGISTRATION-2026-09-19.md")] = "b" * 40
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.branch, BASELINE_BRANCH)
        self.assertEqual(self.harness.world.head, BASELINE_CHECKOUT)
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(self.harness.host.marker(), BASELINE_RUNTIME)
        self.assertIsNone(self.harness.store.read("journal.json"))

    def test_dirty_production_checkout_is_held_with_no_mutation_and_no_journal(self):
        """A dirty baseline cannot be proved, so it is never mutated either."""
        self.harness.world.dirty = True
        with self.assertRaises(core.Held):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.mutations, [])
        self.assertIsNone(self.harness.store.read("journal.json"))

    def test_master_moving_after_preparation_defers(self):
        self.harness.world.fetch_tip = "0" * 39 + "1"
        with self.assertRaises(core.Deferred):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.world.mutations, [])

    def test_interrupted_recreation_leaves_intent_and_reconcile_restores(self):
        self.harness.world.kill_on_up = Killed()
        with self.assertRaises(Killed):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(self.harness.store.read("journal.json")["phase"], "promoting")
        self.assertEqual(self.harness.controller.reconcile(), 0)
        world = self.harness.world
        self.assertEqual(world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(world.branch, BASELINE_BRANCH)
        self.assertEqual(world.spec["ws"]["literal"]["IAC_ENABLED"], "true")
        self.assertIsNone(self.harness.store.read("journal.json"))
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "reconciliation")
        self.assertEqual(receipt["outcome"], "restored")

    def test_promotion_receipt_keeps_both_gates_distinct_and_no_secrets(self):
        self.harness.controller.promote(confirm=True)
        receipt = next(r for r in self.harness.receipts() if r["kind"] == "promotion")
        self.assertEqual(receipt["source"], MASTER_TIP)
        self.assertIn("RELEASE CHECKS PASS", receipt["server_ci_gate"])
        self.assertIn("not-evaluated-by-this-controller", receipt["android_gate"])
        self.assertEqual(receipt["dwell"]["iterations"], 3)
        self.assertIsNotNone(receipt["rollback_rehearsal_at"])
        self.assertNotIn(TOKEN, json.dumps(receipt))
        self.assertNotIn(SECRET, json.dumps(receipt))

    def test_candidate_image_without_the_expected_frontend_flag_is_rejected(self):
        world = self.harness.world
        web = world.tags[f"stockspotter-candidate-web:{MASTER_TIP}"]
        world.images[web]["labels"][adapter.EXPERIMENTAL_LABEL] = "true"
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(world.mutations, [])

    def test_candidate_image_without_a_revision_label_is_rejected(self):
        world = self.harness.world
        ws = world.tags[f"stockspotter-candidate-ws:{MASTER_TIP}"]
        world.images[ws]["labels"].pop(adapter.REVISION_LABEL)
        with self.assertRaises(core.Rejected):
            self.harness.controller.promote(confirm=True)
        self.assertEqual(world.mutations, [])

    def test_a_promotion_requires_a_fresh_adoption_before_the_next_one(self):
        self.harness.controller.promote(confirm=True)
        self.assertIsNone(self.harness.store.read(adapter.REHEARSAL_NAME))
        with self.assertRaises(core.Held) as caught:
            self.harness.controller.prepare()
        self.assertIn("adopt again", str(caught.exception))


class StageLaunchContractTests(unittest.TestCase):
    """The stage substitute for two comparisons the stage itself mandates.

    A stage stack may not publish a port (`stage_recipe_failures`) and its
    command is wrapped by the TLS bootstrap, so `_base_compatibility` would
    reject every stage *candidate* recreation deterministically -- before a
    container exists, and in the one way that used to mint a false rehearsal
    receipt. Byte equality of the launch-contract file answers a stronger
    question about the same input instead.
    """

    BASELINE_BLOB = "7ed173e241c22b9637a69836784c4bee92bb659d"
    CANDIDATE = "b" * 40

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name).resolve()
        self.project = "stockspotter-stage-launchcontract"
        self.owner = self.base / self.project
        self.owner.mkdir(mode=0o700)
        self.blobs = {self.CANDIDATE: self.BASELINE_BLOB}
        self.record = {"base_compose_blob": self.BASELINE_BLOB}
        self.relaxed = []
        self.host = adapter.LinuxHost(
            project=self.project, stage_owner_root=self.owner,
            root=self.owner / "repo", control_dir=self.owner / "control",
            staging=self.owner / "candidate", runner=self.runner,
            locker=contextlib.nullcontext, build_locker=contextlib.nullcontext)
        self.host.candidate_source = self.CANDIDATE
        self.host._recipe_record = lambda *a, **k: self.record
        self.host._base_compatibility = self.base_compatibility

    def runner(self, argv, *, cwd=None, stdin=None, timeout=120):
        argv = tuple(argv)
        assert argv[0] == "git" and "rev-parse" in argv, f"unexpected command {argv}"
        commit, _, path = argv[-1].partition(":")
        assert path == adapter.BASE_COMPOSE_FILE, f"unexpected path {path}"
        blob = self.blobs.get(commit)
        if blob is None:
            return adapter.Result(argv, 128, "", "fatal: not a valid object name")
        return adapter.Result(argv, 0, blob + "\n", "")

    def base_compatibility(self, compose, relax=()):
        self.relaxed.append(sorted(relax))
        return {"roles": "checked"}

    def recipe(self, **changes):
        bootstrap = ["/bin/sh", "-c", "umask 077; printf '%s\n' <pem> > /tmp/ca; exec ./run"]
        services = {role: {"entrypoint": list(bootstrap), "command": []}
                    for role in core.ROLES}
        for role, change in changes.items():
            services[role].update(change)
        return {"services": services}

    def test_an_identical_launch_contract_relaxes_exactly_two_comparisons(self):
        evidence = self.host._stage_launch_contract(self.recipe())
        contract = evidence["stage_launch_contract"]
        self.assertEqual(contract["baseline_blob"], self.BASELINE_BLOB)
        self.assertEqual(contract["candidate_blob"], self.BASELINE_BLOB)
        self.assertIs(contract["equal"], True)
        self.assertEqual(contract["relaxed"], ["command", "ports"])
        self.assertEqual(contract["retained"],
                         ["bind_destinations", "declared_env", "service_set"])
        self.assertEqual(contract["candidate_source"], self.CANDIDATE)
        self.assertEqual(self.relaxed, [["command", "ports"]])

    def test_the_evidence_cannot_read_as_a_production_base_comparison(self):
        evidence = self.host._stage_launch_contract(self.recipe())
        self.assertEqual(set(evidence), {"stage_launch_contract"})
        self.assertNotIn("skipped", json.dumps(evidence))
        for role in core.ROLES:
            self.assertNotIn(role, evidence)

    def test_a_changed_launch_contract_is_rejected_naming_both_blobs(self):
        self.blobs[self.CANDIDATE] = "a" * 40
        with self.assertRaises(core.Rejected) as caught:
            self.host._stage_launch_contract(self.recipe())
        message = str(caught.exception)
        self.assertIn("a" * 40, message)
        self.assertIn(self.BASELINE_BLOB, message)
        self.assertIn("re-derive the baseline", message)
        self.assertEqual(self.relaxed, [])

    def test_a_recipe_without_a_recorded_blob_is_rejected_never_defaulted(self):
        for record in ({}, {"base_compose_blob": None}, {"base_compose_blob": "short"},
                       {"base_compose_blob": "Z" * 40}):
            self.record = record
            with self.subTest(record=record), self.assertRaises(core.Rejected) as caught:
                self.host._stage_launch_contract(self.recipe())
            self.assertIn("records no", str(caught.exception))

    def test_a_missing_candidate_identity_is_rejected(self):
        self.host.candidate_source = None
        with self.assertRaises(core.Rejected) as caught:
            self.host._stage_launch_contract(self.recipe())
        self.assertIn("commit identity", str(caught.exception))

    def test_a_candidate_without_the_launch_contract_file_is_rejected(self):
        self.blobs.clear()
        with self.assertRaises(core.Rejected) as caught:
            self.host._stage_launch_contract(self.recipe())
        self.assertIn("does not carry", str(caught.exception))

    def test_only_the_known_bootstrap_shape_justifies_relaxing_the_command(self):
        cases = {
            "command alongside": {"command": ["./ws-server"]},
            "unwrapped argv": {"entrypoint": ["./ws-server"], "command": []},
            "no entrypoint": {"entrypoint": [], "command": []},
            "wrong shell flag": {"entrypoint": ["/bin/sh", "-lc", "exec ./x"], "command": []},
            "extra argv": {"entrypoint": ["/bin/sh", "-c", "exec ./x", "extra"], "command": []},
        }
        for name, change in cases.items():
            with self.subTest(case=name), self.assertRaises(core.Rejected):
                self.host._stage_launch_contract(self.recipe(ws=change))
        self.assertEqual(self.relaxed, [])

    def test_production_can_never_reach_the_relaxed_path(self):
        harness = Harness(self)
        self.assertFalse(harness.host.stage)
        with self.assertRaises(core.Held):
            harness.host._stage_launch_contract(self.recipe())


class RehearsalEvidenceBindingTests(unittest.TestCase):
    """A receipt means the candidate was live and verified, not that something
    somewhere raised `Rejected`."""

    def setUp(self):
        self.harness = Harness(self)
        self.harness.adopt()
        self.harness.prepare()

    def stamp(self, **changes):
        candidate = self.harness.store.read("prepared.json")["prepared"]["state"]
        record = {"candidate_source": candidate["source"], "ws_commit": candidate["source"],
                  "images": dict(candidate["images"]), "attempt": "attempt-token",
                  "boot_id": self.harness.host.boot_id()}
        record.update(changes)
        self.harness.host.attempt_id = "attempt-token"
        return record, candidate

    def test_the_forced_failure_site_stamps_the_verified_candidate(self):
        evidence = self.harness.rehearse()
        reached = evidence["candidate_reached"]
        self.assertEqual(evidence["version"], 2)
        self.assertEqual(reached["candidate_source"], MASTER_TIP)
        self.assertEqual(reached["ws_commit"], MASTER_TIP)
        self.assertEqual(reached["images"], evidence["candidate"]["images"])
        self.assertEqual(reached["project"], adapter.PROJECT)
        self.assertTrue(reached["artifacts"])
        self.assertEqual(set(reached["role_config_digests"]), set(core.ROLES))
        self.assertIsNotNone(reached["attempt"])

    def test_the_stamp_is_not_the_restored_baselines_evidence(self):
        """`restore()` re-runs up() and health(), so a shared key would be lost."""
        evidence = self.harness.rehearse()
        self.assertEqual(evidence["recreated"]["images"], evidence["baseline_state"]["images"])
        self.assertNotEqual(evidence["candidate_reached"]["images"],
                            evidence["baseline_state"]["images"])

    def test_a_rejection_before_the_candidate_is_live_mints_nothing(self):
        """The §1 shape: `up()` refuses before any container is created."""
        original = self.harness.host.up

        def refuse(images):
            if images != self.harness.host._recipe_record()["images"]:
                raise core.Rejected("fixture: the candidate recreation was preflighted away")
            return original(images)
        self.harness.host.up = refuse
        with self.assertRaises(core.Rejected) as caught:
            self.harness.controller.rehearse(confirm=True)
        self.assertIn("preflighted away", str(caught.exception))
        self.assertIsNone(self.harness.store.read(adapter.REHEARSAL_NAME))
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)

    def test_a_stale_stamp_from_an_earlier_attempt_authorizes_nothing(self):
        """Each attempt clears the previous stamp before it starts."""
        self.harness.host.evidence["forced_proof_failure"] = self.stamp()[0]
        original = self.harness.host.up

        def refuse(images):
            if images != self.harness.host._recipe_record()["images"]:
                raise core.Rejected("fixture: the candidate recreation was refused")
            return original(images)
        self.harness.host.up = refuse
        with self.assertRaises(core.Rejected):
            self.harness.controller.rehearse(confirm=True)
        self.assertIsNone(self.harness.store.read(adapter.REHEARSAL_NAME))
        self.assertNotIn("forced_proof_failure", self.harness.host.evidence)

    def test_every_field_of_the_stamp_is_required_to_match(self):
        record, candidate = self.stamp()
        controller = self.harness.controller
        self.assertTrue(controller._reached_and_verified(record, candidate))
        for change in ({"attempt": "another-attempt"}, {"boot_id": "another-boot"},
                       {"candidate_source": "f" * 40}, {"ws_commit": "f" * 40},
                       {"images": {role: "sha256:" + "f" * 64 for role in core.ROLES}}):
            with self.subTest(change=change):
                self.assertFalse(controller._reached_and_verified({**record, **change},
                                                                  candidate))
        for absent in (None, {}, "a string", []):
            with self.subTest(absent=absent):
                self.assertFalse(controller._reached_and_verified(absent, candidate))

    def test_a_version_one_receipt_defers_rather_than_being_upgraded(self):
        evidence = self.harness.rehearse()
        downgraded = {key: value for key, value in evidence.items()
                      if key != "candidate_reached"}
        self.harness.store.write(adapter.REHEARSAL_NAME, {**downgraded, "version": 1})
        self.harness.prepare()
        with self.assertRaises(core.Deferred) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("predates the reached-and-verified binding", str(caught.exception))
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)

    def test_a_receipt_without_candidate_evidence_defers(self):
        evidence = self.harness.rehearse()
        self.harness.store.write(adapter.REHEARSAL_NAME,
                                 {**evidence, "candidate_reached": None})
        self.harness.prepare()
        with self.assertRaises(core.Deferred) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("no proof that the candidate was live", str(caught.exception))

    def test_a_receipt_whose_evidence_describes_another_candidate_is_held(self):
        evidence = self.harness.rehearse()
        reached = {**evidence["candidate_reached"], "ws_commit": "f" * 40}
        self.harness.store.write(adapter.REHEARSAL_NAME,
                                 {**evidence, "candidate_reached": reached})
        self.harness.prepare()
        with self.assertRaises(core.Held) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("does not describe its own candidate", str(caught.exception))

    def test_a_rollback_from_the_baseline_to_itself_is_held(self):
        evidence = self.harness.rehearse()
        reached = {**evidence["candidate_reached"],
                   "images": dict(evidence["baseline_state"]["images"])}
        candidate = {**evidence["candidate"],
                     "images": dict(evidence["baseline_state"]["images"])}
        self.harness.store.write(adapter.REHEARSAL_NAME,
                                 {**evidence, "candidate_reached": reached,
                                  "candidate": candidate})
        self.harness.prepare()
        with self.assertRaises(core.Held) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("baseline to itself", str(caught.exception))

    def test_a_good_version_two_receipt_still_permits_promotion(self):
        self.harness.rehearse()
        self.harness.prepare()
        self.assertEqual(self.harness.controller.promote(confirm=True), 0)


class RehearsalGateTests(unittest.TestCase):
    """A real rollback must have happened before a real promotion."""

    def setUp(self):
        self.harness = Harness(self)
        self.harness.adopt()
        self.harness.prepare()

    def test_promotion_is_refused_without_a_recorded_rollback_rehearsal(self):
        with self.assertRaises(core.Deferred) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("rehearse --confirm", str(caught.exception))
        self.assertEqual(self.harness.world.mutations, [])

    def test_the_rehearsal_really_recreates_and_really_restores(self):
        evidence = self.harness.rehearse()
        self.assertEqual(self.harness.world.ws_commit, BASELINE_RUNTIME)
        self.assertEqual(self.harness.world.branch, BASELINE_BRANCH)
        self.assertEqual(evidence["candidate"]["source"], MASTER_TIP)
        self.assertEqual(evidence["restored_state"]["source"], BASELINE_RUNTIME)
        self.assertIn("rehearsal", evidence["rejection"])
        # The candidate was genuinely live in between.
        self.assertIn("up", self.harness.world.mutations)
        self.assertGreaterEqual(self.harness.world.mutations.count("up"), 2)

    def test_the_rehearsal_requires_confirmation_and_an_adoption(self):
        with self.assertRaises(core.Deferred):
            self.harness.controller.rehearse(confirm=False)
        harness = Harness(self)
        with self.assertRaises(core.Deferred):
            harness.controller.rehearse(confirm=True)

    def test_a_rehearsal_receipt_from_another_policy_does_not_count(self):
        self.harness.rehearse()
        evidence = self.harness.store.read(adapter.REHEARSAL_NAME)
        self.harness.store.write(adapter.REHEARSAL_NAME,
                                 {**evidence, "policy_digest": "0" * 64})
        self.harness.prepare()
        with self.assertRaises(core.Deferred) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("predates this adoption policy", str(caught.exception))

    def test_a_rehearsal_receipt_for_another_baseline_does_not_count(self):
        self.harness.rehearse()
        evidence = self.harness.store.read(adapter.REHEARSAL_NAME)
        other = {**evidence["restored_state"], "source": MASTER_TIP}
        self.harness.store.write(adapter.REHEARSAL_NAME,
                                 {**evidence, "restored_state": other})
        self.harness.prepare()
        with self.assertRaises(core.Deferred) as caught:
            self.harness.controller.promote(confirm=True)
        self.assertIn("different baseline", str(caught.exception))

    def test_a_malformed_rehearsal_record_is_held(self):
        self.harness.store.write(adapter.REHEARSAL_NAME, {"version": 9})
        with self.assertRaises(core.Held):
            self.harness.controller.promote(confirm=True)

    def test_the_rehearsal_is_refused_while_the_legacy_timer_is_armed(self):
        self.harness.world.timer = {"is-enabled": "enabled", "is-active": "active"}
        with self.assertRaises(core.Deferred):
            self.harness.controller.rehearse(confirm=True)
        self.assertEqual(self.harness.world.mutations, [])

    def test_a_rehearsal_that_does_not_roll_back_is_held_not_recorded(self):
        self.harness.host.force_rollback = False
        original = self.harness.controller._transact

        def transact(policy, envelope, rehearsal):
            return original(policy, envelope, rehearsal=False)
        self.harness.controller._transact = transact
        with self.assertRaises(core.Held) as caught:
            self.harness.controller.rehearse(confirm=True)
        self.assertIn("expected to roll back", str(caught.exception))
        self.assertIsNone(self.harness.store.read(adapter.REHEARSAL_NAME))


class CommandLineTests(unittest.TestCase):
    def setUp(self):
        self.harness = Harness(self)

    def _main(self, argv):
        with contextlib.redirect_stderr(io.StringIO()):
            return adapter.main(
                argv,
                host_factory=lambda **kwargs: self.harness.host,
                store_factory=lambda path: self.harness.store,
                out=self.harness.out,
            )

    def test_every_command_is_explicit(self):
        parser = adapter.build_parser()
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            parser.parse_args([])
        for command in ("inspect", "adopt", "prepare", "rehearse", "promote", "reconcile"):
            with self.subTest(command=command):
                arguments = (["adopt", "--experimental-strategies", "false",
                              "--role-decision", "web=rebuild"]
                             if command == "adopt" else [command])
                self.assertEqual(parser.parse_args(arguments).command, command)

    def test_adopt_requires_role_decisions_on_the_command_line(self):
        parser = adapter.build_parser()
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            parser.parse_args(["adopt", "--experimental-strategies", "false"])

    def test_the_control_directory_is_also_the_marker_and_recipe_home(self):
        arguments = adapter.build_parser().parse_args(["--store", "/tmp/control", "inspect"])
        self.assertEqual(arguments.store, Path("/tmp/control"))

    def test_inspect_through_the_command_line_succeeds_and_mutates_nothing(self):
        self.assertEqual(self._main(["inspect", "--no-exposure"]), 0)
        self.assertEqual(self.harness.world.mutations, [])

    def test_adopt_through_the_command_line_parses_every_explicit_decision(self):
        argv = ["adopt", "--confirm", "--experimental-strategies", "true",
                "--baseline-exempt", "ws=unbounded-logs",
                "--expect-artifact", f"ws={self.harness.world.artifact('ws')}"]
        for role in sorted(core.ROLES):
            argv += ["--role-decision", f"{role}=rebuild"]
        self.assertEqual(self._main(argv), 0)
        policy = adapter.Policy.from_json(self.harness.store.read("policy.json"))
        self.assertTrue(policy.experimental_strategies)
        self.assertEqual(policy.exemptions["ws"], ["unbounded-logs"])
        self.assertEqual(policy.decisions, {role: "rebuild" for role in core.ROLES})

    def test_a_malformed_expected_artifact_digest_is_refused(self):
        argv = ["adopt", "--confirm", "--experimental-strategies", "false",
                "--expect-artifact", "ws=nothex"]
        for role in sorted(core.ROLES):
            argv += ["--role-decision", f"{role}=rebuild"]
        with self.assertRaises(SystemExit):
            self._main(argv)

    def test_exit_codes_distinguish_deferred_rejected_and_held(self):
        self.assertEqual(self._main(["promote"]), 2)                      # no confirmation
        self.harness.adopt()
        self.harness.world.ancestor_ok = False
        self.assertEqual(self._main(["prepare"]), 3)                      # rejected candidate
        self.harness.world.ancestor_ok = True
        del self.harness.files.contents[str(CONTROLLER / "deploy_guard.py")]
        self.assertEqual(self._main(["prepare"]), 4)                      # held: no gate


class SchemaSurfaceTests(unittest.TestCase):
    """The adapter must not quietly change the schema-2 contract."""

    def test_the_adapter_implements_every_host_method(self):
        for name in ("lock", "tip", "ancestor", "ci_passed", "snapshot", "env_digest",
                     "marker", "build", "revisions", "allowed", "exposure_zero",
                     "checkout", "restore_checkout", "up", "health", "write_marker"):
            self.assertTrue(callable(getattr(adapter.LinuxHost, name)), name)

    def test_state_produced_by_the_adapter_is_valid_schema_two(self):
        harness = Harness(self)
        self.assertEqual(core.state(harness.host.snapshot())["branch"], BASELINE_BRANCH)

    def test_policy_round_trips_and_refuses_foreign_records(self):
        policy = adapter.Policy(
            frozen={name: "0" * 64 for name in adapter.FROZEN_PATHS},
            flags={role: {name: None for name in adapter.PARITY_FLAGS} for role in core.ROLES},
            experimental_strategies=False,
            baseline_images={role: image_id(role) for role in core.ROLES},
            decisions={role: "rebuild" for role in core.ROLES},
            exemptions={role: [] for role in core.ROLES},
            artifacts={role: None for role in core.ROLES},
            provenance_digest="1" * 64,
        )
        self.assertEqual(adapter.Policy.from_json(policy.as_json()), policy)
        for broken in ({}, {"version": 1}, {**policy.as_json(), "flags": {}},
                       {**policy.as_json(), "decisions": {role: "maybe" for role in core.ROLES}},
                       {**policy.as_json(),
                        "exemptions": {role: ["public-ports"] for role in core.ROLES}},
                       {**policy.as_json(), "provenance_digest": "short"},
                       {**policy.as_json(), "artifacts": {role: "nope" for role in core.ROLES}}):
            with self.assertRaises(core.Held):
                adapter.Policy.from_json(broken)

    def test_the_fixture_git_does_not_claim_a_fast_forward_can_rewind(self):
        """Guards the fixture, not the adapter.

        `World._git` used to move the branch ref to whatever `merge --ff-only`
        was handed, in any direction. Every test in this file passed while the
        real adapter could not roll back at all. If this assertion is ever
        relaxed again the whole suite goes back to certifying behaviour Git
        does not have, so it is pinned here rather than left implicit.
        """
        world = World()
        world.locked = True
        world.branch, world.branches["master"] = "master", MASTER_TIP
        code, out, _ = world._git(["merge", "--ff-only", BASELINE_CHECKOUT], None)
        self.assertEqual((code, world.branches["master"]), (0, MASTER_TIP))
        self.assertIn("up to date", out.lower())
        code, _, _ = world._git(["merge", "--ff-only", "9" * 40], None)
        self.assertEqual((code, world.branches["master"]), (128, MASTER_TIP))
        world.branches["master"] = BASELINE_CHECKOUT
        code, _, _ = world._git(["merge", "--ff-only", MASTER_TIP], None)
        self.assertEqual((code, world.branches["master"]), (0, MASTER_TIP))

    def test_the_legacy_marker_path_is_never_a_write_target_in_this_module(self):
        source = Path(adapter.__file__).read_text(encoding="utf-8")
        writes = [line for line in source.splitlines()
                  if "write_private" in line and "LEGACY" in line]
        self.assertEqual(writes, [])
        self.assertIn("CONTROLLER_MARKER_NAME", source)


if __name__ == "__main__":
    unittest.main()
