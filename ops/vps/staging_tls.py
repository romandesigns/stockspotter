"""Ephemeral TLS fixtures for an isolated paper endpoint; never production.

The preserved preflight keeps its exact HTTPS hostname. Only the isolated
Docker network gives that name to a synthetic broker. A one-day fixture
certificate is trusted through the staged container's own SSL_CERT_FILE;
production DNS, CA stores, signing keys and preflight code are untouched.
"""
from __future__ import annotations
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import deployment_host as adapter
import staging_rehearsal as rehearsal

PAPER_URL = "https://paper-api.alpaca.markets"
CERT_TARGET = "/tmp/stockspotter-staging-root.pem"
BROKER_CERT_TARGET = "/staging-not-a-real-credential-cert.pem"
BROKER_KEY_TARGET = "/staging-not-a-real-credential-key.pem"


def provision(directory: Path, openssl: str = "openssl") -> dict:
    directory = Path(directory).resolve()
    if directory.exists() and any(directory.iterdir()):
        raise ValueError("refuse an existing nonempty fixture certificate directory")
    directory.mkdir(parents=True, mode=0o700, exist_ok=True)
    os.chmod(directory, 0o700)
    certificate, key = directory / "certificate.pem", directory / "key.pem"
    command = [openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
               "-keyout", str(key), "-out", str(certificate), "-subj", "/CN=paper-api.alpaca.markets",
               "-addext", "subjectAltName=DNS:paper-api.alpaca.markets,DNS:localhost,IP:127.0.0.1",
               "-addext", "basicConstraints=critical,CA:TRUE"]
    subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
    os.chmod(key, 0o600)
    os.chmod(certificate, 0o600)
    return {"certificate": str(certificate), "key": str(key),
            "certificate_sha256": hashlib.sha256(certificate.read_bytes()).hexdigest(),
            "scope": "ephemeral synthetic staging fixture only"}


def derive(recipe: dict, *, stage_root: Path, project: str, broker_script: Path,
           certificate: Path, key: Path, faults=()) -> dict:
    stage_root = Path(stage_root).resolve()
    certificate, key = Path(certificate).resolve(), Path(key).resolve()
    if not adapter.STAGE_PROJECT.fullmatch(project):
        raise ValueError("invalid isolated stage project")
    if adapter._inside(stage_root, adapter.PRODUCTION_ROOT):
        raise ValueError("stage root must be outside production")
    if not certificate.is_relative_to(stage_root) or not key.is_relative_to(stage_root):
        raise ValueError("fixture certificates must be inside the owned stage root")
    pem = certificate.read_text(encoding="ascii")
    if not pem.startswith("-----BEGIN CERTIFICATE-----"):
        raise ValueError("invalid fixture certificate")
    document = rehearsal.derive(recipe, data_dir=stage_root / "data", broker_script=broker_script, faults=faults)
    document["name"] = project
    document["networks"] = {"default": {"name": project + "_default", "internal": True}}
    for role in sorted(adapter.ROLES):
        service = document["services"][role]
        environment = adapter._env_map(service["environment"])
        environment["ALPACA_TRADING_BASE"] = PAPER_URL
        environment["SSL_CERT_FILE"] = CERT_TARGET
        # Trust is scoped to this synthetic container, restored on every
        # recreation without a new host bind or changes to the real image.
        original = list(service.get("entrypoint") or ()) + list(service.get("command") or ())
        if not original or not all(isinstance(argument, str) for argument in original):
            raise ValueError(f"{role}: explicit argv is required for fixture bootstrap")
        bootstrap = "umask 077; printf '%s\n' " + shlex.quote(pem) + " > " + shlex.quote(CERT_TARGET) + "; exec " + shlex.join(original)
        if "$" in bootstrap:
            raise ValueError("fixture argv contains compose interpolation; refuse rather than reinterpret")
        service["entrypoint"] = ["/bin/sh", "-c", bootstrap]
        service["command"] = []
        service["environment"] = [f"{name}={value}" for name, value in sorted(environment.items())]
    broker = document["services"][rehearsal.BROKER_SERVICE]
    environment = adapter._env_map(broker["environment"])
    environment.update({"STAGING_BROKER_PORT": "443", "STAGING_BROKER_TLS_CERT": BROKER_CERT_TARGET,
                        "STAGING_BROKER_TLS_KEY": BROKER_KEY_TARGET})
    broker["environment"] = [f"{name}={value}" for name, value in sorted(environment.items())]
    broker["volumes"] += [f"{certificate}:{BROKER_CERT_TARGET}:ro", f"{key}:{BROKER_KEY_TARGET}:ro"]
    broker["networks"] = {"default": {"aliases": ["paper-api.alpaca.markets"]}}
    # Never rely on an operator remembering to check these before `up`.
    failures = adapter.stage_recipe_failures(document, project)
    for role, service in document["services"].items():
        for entry in service.get("volumes") or ():
            source, _destination, mode = adapter._bind_parts(entry)
            source_path = Path(source).resolve()
            if role == rehearsal.BROKER_SERVICE and source_path == Path(broker_script).resolve() and mode == "ro":
                continue
            if not source_path.is_relative_to(stage_root):
                failures.append(f"{role}: bind source escapes the owned stage root")
    if failures:
        raise ValueError("unsafe stage recipe: " + "; ".join(failures))
    document["x-stockspotter-staging"]["exposure_gate"] = "exact preserved HTTPS preflight through isolated synthetic DNS/TLS; never a live account proof"
    document["x-stockspotter-staging"]["certificate_sha256"] = hashlib.sha256(certificate.read_bytes()).hexdigest()
    return document
