"""Durable deployment decisions; host effects are supplied by an adapter.

Schema 2 keeps the mapped compiled WS git commit (source) separate from
the actual checkout git commit (checkout_source). Mixed baselines require
explicit adoption and verified immutable images; no legacy-marker fallback.

This module alone does not activate deployments. The Linux adapter must use
the existing deployment flock and implement real health/exposure checks.
"""
from __future__ import annotations

from contextlib import AbstractContextManager
import json
import os
from pathlib import Path
import re
from typing import Protocol
from uuid import uuid4

ROLES = frozenset(("web", "ws", "qualify", "auto-trader", "discovery-review"))


class Rejected(RuntimeError):
    """A candidate failed a release check, rather than a transient host check."""


class Deferred(RuntimeError):
    """No promotion was attempted; preparation can be retried."""


class Held(RuntimeError):
    """An unsafe or incomplete transaction needs explicit reconciliation."""


def source_sha(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise Held("Invalid source identity")
    return value


def image_set(value):
    if not isinstance(value, dict) or set(value) != ROLES:
        raise Held("Image set must cover exactly the five services")
    if any(not isinstance(image, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", image) for image in value.values()):
        raise Held("Invalid immutable image identity")
    return dict(value)


def state(value):
    if not isinstance(value, dict) or set(value) != {"source", "checkout_source", "branch", "images", "env_hash"}:
        raise Held("Malformed deployment state")
    source_sha(value["source"])
    source_sha(value["checkout_source"])
    image_set(value["images"])
    if not isinstance(value["branch"], str) or not value["branch"]:
        raise Held("Missing source branch")
    if not isinstance(value["env_hash"], str) or not re.fullmatch(r"[0-9a-f]{64}", value["env_hash"]):
        raise Held("Invalid environment fingerprint")
    return json.loads(json.dumps(value))


class Host(Protocol):
    def lock(self) -> AbstractContextManager: ...
    def tip(self) -> str: ...
    def ancestor(self, previous: str, candidate: str) -> bool: ...
    def ci_passed(self, source: str) -> bool: ...
    def snapshot(self) -> dict: ...
    def env_digest(self) -> str: ...
    def marker(self) -> str: ...
    def build(self, source: str) -> dict: ...
    def revisions(self, images: dict) -> dict: ...
    def allowed(self) -> bool: ...
    def exposure_zero(self) -> bool: ...
    def checkout(self, source: str, branch: str) -> None: ...
    def up(self, images: dict) -> None: ...
    def health(self) -> dict: ...
    def write_marker(self, source: str) -> None: ...


class Store:
    def __init__(self, directory: Path):
        self.directory = Path(directory)
        if self.directory.is_symlink():
            raise Held("Control directory cannot be a symlink")
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        if os.name == "posix":
            info = self.directory.stat()
            if info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise Held("Control directory must be private and owned by the deploy user")

    def read(self, name):
        path = self.directory / name
        try:
            if path.is_symlink():
                raise Held("Unsafe control record")
            descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
            with os.fdopen(descriptor, "r", encoding="utf-8") as handle:
                if os.fstat(handle.fileno()).st_size > 128 * 1024:
                    raise Held("Unsafe control record")
                raw = handle.read(128 * 1024 + 1)
                if len(raw) > 128 * 1024:
                    raise Held("Unsafe control record")
                return json.loads(raw)
        except FileNotFoundError:
            return None
        except (OSError, ValueError) as error:
            raise Held("Unreadable control record") from error

    def sync_directory(self):
        if os.name == "nt":
            return  # Windows fixtures prove semantics, not POSIX durability.
        descriptor = os.open(self.directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    def write(self, name, value):
        target = self.directory / name
        temporary = self.directory / (name + "." + uuid4().hex + ".pending")
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                json.dump(value, handle, sort_keys=True)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, target)
            self.sync_directory()
        finally:
            temporary.unlink(missing_ok=True)

    def clear_journal(self):
        (self.directory / "journal.json").unlink(missing_ok=True)
        self.sync_directory()


class Transaction:
    def __init__(self, store: Store, host: Host):
        self.store, self.host = store, host

    def proof(self, expected):
        expected = state(expected)
        if state(self.host.snapshot()) != expected:
            raise Rejected("Source, image or environment differs")
        health = self.host.health()
        if not isinstance(health, dict) or set(health) != {"images", "running", "ws_commit"}:
            raise Rejected("Malformed runtime proof")
        if image_set(health["images"]) != expected["images"] or source_sha(health["ws_commit"]) != expected["source"]:
            raise Rejected("Runtime identity differs")
        running = health["running"]
        if not isinstance(running, dict) or set(running) != ROLES or any(value is not True for value in running.values()):
            raise Rejected("A service is not running")

    def committed(self):
        value = self.store.read("committed.json")
        if value is None:
            return None
        if not isinstance(value, dict) or set(value) != {"version", "state"} or type(value["version"]) is not int or value["version"] != 2:
            raise Held("Malformed committed deployment receipt")
        return state(value["state"])

    def prepare(self):
        before = state(self.host.snapshot())
        committed = self.committed()
        if committed is not None and before != committed:
            raise Deferred("Running baseline differs from committed receipt")
        if source_sha(self.host.marker()) != before["source"]:
            raise Deferred("Checkout and deployed marker differ; reconcile first")
        tip = source_sha(self.host.tip())
        if self.host.ancestor(before["source"], tip) is not True:
            raise Rejected("Master is not a fast-forward")
        if self.host.ci_passed(tip) is not True:
            raise Deferred("Exact master CI is not ready")
        images = image_set(self.host.build(tip))
        if self.host.revisions(images) != {role: tip for role in ROLES}:
            raise Rejected("Built images do not identify the candidate")
        return {"baseline_state": before, "state": {"source": tip, "checkout_source": tip, "branch": "master", "images": images, "env_hash": before["env_hash"]}}

    def journal(self):
        record = self.store.read("journal.json")
        if record is None:
            return None
        if not isinstance(record, dict) or set(record) != {"version", "tx", "phase", "before", "candidate"}:
            raise Held("Malformed deployment journal")
        if type(record["version"]) is not int or record["version"] != 2 or record["phase"] not in {"promoting", "verified", "rolling_back", "held"}:
            raise Held("Invalid journal version or phase")
        if not isinstance(record["tx"], str) or not re.fullmatch(r"[0-9a-f]{32}", record["tx"]):
            raise Held("Invalid transaction identity")
        state(record["before"])
        state(record["candidate"])
        return record

    def hold(self, record, message):
        record["phase"] = "held"
        self.store.write("journal.json", record)
        raise Held(message)

    def restore(self, record):
        if self.host.exposure_zero() is not True:
            self.hold(record, "Exposure prevents automatic restoration")
        if self.host.env_digest() != record["before"]["env_hash"]:
            self.hold(record, "Environment changed; no automatic restore")
        record["phase"] = "rolling_back"
        self.store.write("journal.json", record)
        before = record["before"]
        try:
            self.host.checkout(before["checkout_source"], before["branch"])
            # Always reconcile up: identical image IDs can still be stopped.
            self.host.up(before["images"])
            self.proof(before)
            if self.host.exposure_zero() is not True:
                self.hold(record, "Exposure changed during restoration")
            self.host.write_marker(before["source"])
            if source_sha(self.host.marker()) != before["source"]:
                self.hold(record, "Restored marker differs")
            self.store.write("committed.json", {"version": 2, "state": before})
            self.store.clear_journal()
        except Held:
            raise
        except Exception:
            self.hold(record, "Restoration did not verify")

    def finish(self, record):
        candidate = record["candidate"]
        self.proof(candidate)
        if self.host.exposure_zero() is not True:
            self.hold(record, "Exposure changed before commit")
        self.host.write_marker(candidate["source"])
        if source_sha(self.host.marker()) != candidate["source"]:
            self.hold(record, "Committed marker differs")
        self.store.write("committed.json", {"version": 2, "state": candidate})
        self.store.clear_journal()

    def reconcile_locked(self):
        record = self.journal()
        if record is None:
            committed = self.committed()
            if committed is not None:
                try:
                    self.proof(committed)
                except Exception as error:
                    raise Held("Committed runtime differs; explicit recovery required") from error
                if source_sha(self.host.marker()) != committed["source"]:
                    raise Held("Committed marker changed")
            return "clean"
        if record["phase"] == "held":
            raise Held("Transaction is held for explicit recovery")
        if record["phase"] == "verified":
            try:
                self.proof(record["candidate"])
            except Held:
                self.hold(record, "Malformed runtime proof during reconciliation")
            except Exception:
                self.restore(record)
                return "restored"
            try:
                self.finish(record)
            except Rejected:
                self.hold(record, "Verified candidate changed before commit")
            return "committed"
        self.restore(record)
        return "restored"

    def reconcile(self):
        with self.host.lock():
            return self.reconcile_locked()

    def promote(self, prepared):
        if not isinstance(prepared, dict) or set(prepared) != {"baseline_state", "state"}:
            raise Held("Malformed prepared candidate")
        candidate = state(prepared["state"])
        baseline = state(prepared["baseline_state"])
        with self.host.lock():
            if self.reconcile_locked() != "clean":
                raise Deferred("Previous transaction reconciled; prepare again")
            before = state(self.host.snapshot())
            if source_sha(self.host.marker()) != before["source"]:
                raise Held("Deployed marker differs under deployment lock")
            if before != baseline or before["env_hash"] != candidate["env_hash"]:
                raise Deferred("Preparation baseline changed")
            if self.host.env_digest() != before["env_hash"]:
                raise Held("Live environment differs from snapshot")
            if source_sha(self.host.tip()) != candidate["source"]:
                raise Deferred("Master moved; build the latest tip")
            if self.host.ancestor(before["source"], candidate["source"]) is not True or self.host.ci_passed(candidate["source"]) is not True:
                raise Deferred("Candidate CI/ancestry must be rechecked")
            if self.host.revisions(candidate["images"]) != {role: candidate["source"] for role in ROLES}:
                raise Rejected("Candidate image labels changed")
            if self.host.allowed() is not True or self.host.exposure_zero() is not True:
                raise Deferred("Promotion window or exposure is not safe")
            record = {"version": 2, "tx": uuid4().hex, "phase": "promoting", "before": before, "candidate": candidate}
            self.store.write("journal.json", record)
            try:
                self.host.checkout(candidate["checkout_source"], "master")
                if self.host.exposure_zero() is not True:
                    self.hold(record, "Exposure changed before recreation")
                self.host.up(candidate["images"])
                self.proof(candidate)
                if self.host.exposure_zero() is not True:
                    self.hold(record, "Exposure changed after recreation")
            except Held:
                raise
            except Exception:
                self.restore(record)
                raise
            record["phase"] = "verified"
            self.store.write("journal.json", record)
            try:
                self.finish(record)
            except Rejected:
                self.hold(record, "Verified candidate changed before commit")
            return candidate["source"]
