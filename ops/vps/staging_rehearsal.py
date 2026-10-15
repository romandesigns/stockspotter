#!/usr/bin/env python3
"""Isolated staging rehearsal for the private deployment recipe.

The single scariest unknown about `deployment_host.py` is not whether it can
call `docker compose`: it is whether the private recipe captured from the live
containers is a *faithful* reproduction of them. If it is not, the first
promotion silently changes the running configuration and the first rollback
silently fails to restore it.

This module answers that question without touching production. It derives an
isolated staging stack from the captured recipe, running the same five images
with the same commands, environment variable *names*, binds and logging, and
then compares what the daemon actually created against what was asked for --
using `deployment_host.conformance_failures`, the very function the production
proof uses, so a staging pass cannot mean something a production proof would
refuse.

Isolation is enforced in code, not by convention:

* a distinct compose project (`stockspotter-staging`), so no production
  container name can be recreated or removed by it;
* **no published ports at all** -- the staging stack is unreachable from the
  host, cannot collide with 3000/8787/8788 and cannot be mistaken for the real
  service by Caddy;
* an `internal: true` network, so nothing in it reaches the internet, the real
  broker or the real market-data feed;
* **no `env_file`** -- the host's real `.env` is never mounted or read. Every
  variable the real stack takes from `.env` is replaced by a synthetic value
  carrying an obvious marker, so a staging container cannot hold a real
  credential even by accident;
* bind sources rewritten to a throwaway staging directory, so the live
  `data/` journals cannot be written to or truncated.

`verify_isolation` re-checks every one of those on the generated document, and
the CLI refuses to emit a document that fails any of them.

Nothing here runs Docker. `plan` prints the document and the exact command
sequence; `write` saves the document privately; `compare` reads a saved
`docker inspect` output and reports the fidelity differences. Each step is
separately inspectable, and the module never starts a container itself.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import sys

STAGING_PROJECT = "stockspotter-staging"
BROKER_SERVICE = "broker"
BROKER_PORT = 9999
BROKER_SCRIPT_TARGET = "/staging_broker.py"
# Every synthetic value carries this, so "did a real credential reach the
# staging stack?" is answerable by looking at the document.
MARKER = "staging-not-a-real-credential"
# `release_preflight.py` requires the literal production paper URL, so this
# deliberately does NOT satisfy `paper_endpoint_configured`. See the note in
# `staging_broker.py`: weakening the real gate to make staging convenient would
# defeat the gate.
BROKER_BASE = f"http://{BROKER_SERVICE}:{BROKER_PORT}"

FAULTS = {
    "crash-loop": "replace ws's command with one that exits immediately",
    "probe-failure": "point ws at a qualify URL that cannot resolve",
    "missing-bind": "drop ws's data bind mount",
    "drop-overlay-env": "remove IAC_ENABLED from ws, as a lost overlay would",
    "unbounded-logs": "clear ws's log rotation options",
    "broker-open-position": "make the fake broker report an open position",
    "broker-unreachable": "make every fake broker request fail",
}


def _load_adapter():
    """Import the sibling adapter by path, like the controller does."""
    path = Path(__file__).with_name("deployment_host.py")
    spec = importlib.util.spec_from_file_location("deployment_host", path)
    if spec is None or spec.loader is None:  # pragma: no cover - packaging error
        raise SystemExit(f"staging_rehearsal: cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault("deployment_host", module)
    spec.loader.exec_module(module)
    return module


adapter = _load_adapter()
ROLES = adapter.ROLES


def load_recipe(path) -> dict:
    """The captured baseline recipe, validated enough to derive from."""
    record = json.loads(Path(path).read_text(encoding="utf-8"))
    if (not isinstance(record, dict) or not isinstance(record.get("compose"), dict)
            or set((record["compose"].get("services") or {})) != ROLES
            or not isinstance(record.get("effective"), dict)):
        raise SystemExit("staging_rehearsal: that is not a captured baseline recipe")
    return record


def synthetic_env(dotenv_names=()) -> dict:
    """Every value a staged container may carry in place of a real one.

    The single source of truth for both halves of the fixture: the compose
    document's `environment:` entries and the stage `.env` the controller reads
    when it adopts the stack. Those two *must* agree -- see `synthetic_dotenv`
    for why -- and the only way to guarantee that is for neither to be written
    by hand.

    The four fixed names are assigned last, so they override a same-named
    `.env` variable exactly as the inline version they replaced did.
    """
    values = {name: f"{MARKER}-{name.lower()}" for name in sorted(dotenv_names)}
    # Long enough to satisfy the token-length assertion the real
    # pre-recreation check uses, without resembling a real token.
    values["STOCKSPOTTER_API_TOKEN"] = (MARKER + "-token").ljust(48, "x")
    values["ALPACA_TRADING_BASE"] = BROKER_BASE
    values["ALPACA_API_KEY"] = f"{MARKER}-key"
    values["ALPACA_API_SECRET"] = f"{MARKER}-secret"
    return values


def dotenv_failures(document: dict) -> list:
    """Why this document could not be adopted from a single stage `.env`.

    `deployment_host._role_env` attributes a container's environment by
    comparing digests against the host's own `.env`. Two of its rules decide
    the whole shape of this file:

    * a name in `.env` that is missing from *any* role is `dropped`, and
      adoption is `Held`;
    * a secretish name observed in a role that `.env` does not cover is
      `unreproducible`, and adoption is `Held` -- the controller refuses to
      copy a possible credential into its own files.

    So the file can only be the intersection of all five roles, and every
    secretish variable must be inside that intersection. A document where one
    is not cannot be adopted by any `.env` at all, and saying so here is far
    better than discovering it from a `Held` halfway through an adoption.
    """
    services = document.get("services") or {}
    missing_roles = sorted(ROLES - set(services))
    if missing_roles:
        return [f"document is missing roles {', '.join(missing_roles)}"]
    maps = {role: adapter._env_map(services[role].get("environment"))
            for role in sorted(ROLES)}
    shared = _shared_environment(maps)
    problems = []
    secretish = {name for values in maps.values() for name in values
                 if adapter._secretish(name)}
    for name in sorted(secretish - set(shared)):
        carriers = sorted(role for role, values in maps.items() if name in values)
        problems.append(
            f"{name} is secretish but not identical across all five roles (in "
            f"{', '.join(carriers)}), so no single .env can cover it"
        )
    for name, value in sorted(shared.items()):
        if not adapter.ENV_NAME.fullmatch(name):
            problems.append(f"{name} is not a usable environment variable name")
        if value != value.strip() or "\n" in value or "\r" in value:
            problems.append(f"{name} has a value env_file parsing would not preserve")
        elif len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            problems.append(f"{name} is quoted; env_file would strip the quotes")
    return problems


def _shared_environment(maps: dict) -> dict:
    """Name/value pairs every role carries identically."""
    ordered = [maps[role] for role in sorted(maps)]
    if not ordered:
        return {}
    return {name: value for name, value in ordered[0].items()
            if all(other.get(name) == value for other in ordered[1:])}


def synthetic_dotenv(document: dict) -> str:
    """The stage `.env` that makes a generated document adoptable.

    Built from the *finished* document rather than from the recipe, so a later
    transformation -- `staging_tls.derive` replacing the broker base with the
    exact preserved paper URL, for instance -- cannot leave the file and the
    running containers disagreeing. Refuses rather than emitting a file that
    would be `Held` at adoption.
    """
    problems = dotenv_failures(document)
    if problems:
        raise ValueError("cannot derive a stage .env: " + "; ".join(problems))
    services = document["services"]
    shared = _shared_environment({role: adapter._env_map(services[role].get("environment"))
                                  for role in sorted(ROLES)})
    header = ("# Synthetic stage values only. Generated by staging_rehearsal from the\n"
              f"# same map as the compose document; every secret carries {MARKER}.\n")
    return header + "".join(f"{name}={shared[name]}\n" for name in sorted(shared))


def derive(recipe: dict, *, data_dir, broker_script, faults=()) -> dict:
    """Build the isolated staging compose document from a captured recipe."""
    unknown = sorted(set(faults) - set(FAULTS))
    if unknown:
        raise SystemExit(f"staging_rehearsal: unknown fault(s) {', '.join(unknown)}")
    data_dir, broker_script = str(data_dir), str(broker_script)
    # The *union* of every role's `.env` names, given to every role.
    #
    # This is the `env_file` semantics the stage stack cannot use directly: a
    # role that reads the file reads all of it. It is also what makes the stack
    # adoptable at all. `deployment_host._role_env` marks a secretish variable
    # `unreproducible` -- and Holds -- unless the stage `.env` covers it, and a
    # name in that `.env` which some role lacks is `dropped`, which Holds too.
    # So the file must be a subset of every role, and must cover every
    # secretish variable; per-role name sets make those two demands
    # contradictory whenever a secret reaches four roles and not the fifth.
    # Giving every role the union makes them agree by construction, which is
    # why this is not merely a convenience.
    shared_names = sorted({name for values in (recipe.get("effective") or {}).values()
                           for name in values.get("dotenv") or ()})
    services = {}
    for role in sorted(ROLES):
        source = recipe["compose"]["services"][role]
        environment = dict(adapter._env_map(source.get("environment")))
        # Every variable the real stack takes from `.env`, replaced by a
        # synthetic value. Names are preserved because the application's
        # behaviour depends on them being set; values never are. The same map
        # writes the stage `.env`, so the two cannot drift -- see
        # `synthetic_env` and `synthetic_dotenv`.
        environment.update(synthetic_env(shared_names))
        service = {
            "image": source["image"],
            "restart": source.get("restart", "no"),
            "logging": json.loads(json.dumps(source.get(
                "logging", {"driver": "json-file", "options": {"max-size": "10m"}}))),
            "environment": [f"{name}={value}" for name, value in sorted(environment.items())],
        }
        for key in ("command", "entrypoint", "user", "working_dir"):
            if source.get(key):
                service[key] = json.loads(json.dumps(source[key]))
        if source.get("depends_on"):
            service["depends_on"] = list(source["depends_on"])
        volumes = []
        for entry in source.get("volumes") or []:
            _, destination, mode = adapter._bind_parts(entry)
            volumes.append(f"{data_dir}{destination}:{destination}"
                           + (f":{mode}" if mode else ""))
        if volumes:
            service["volumes"] = sorted(volumes)
        # Deliberately absent: `ports` and `env_file`.
        services[role] = service

    broker_environment = {"STAGING_BROKER_PORT": str(BROKER_PORT)}
    if "broker-open-position" in faults:
        broker_environment["STAGING_BROKER_POSITIONS"] = "1"
    if "broker-unreachable" in faults:
        broker_environment["STAGING_BROKER_UNREACHABLE"] = "1"
    services[BROKER_SERVICE] = {
        # Reuses an image that already has Python rather than building one: the
        # staging stack must not need a build to exist.
        "image": recipe["compose"]["services"]["qualify"]["image"],
        "restart": "no",
        "logging": {"driver": "json-file", "options": {"max-size": "10m"}},
        "entrypoint": ["python", BROKER_SCRIPT_TARGET],
        "command": [],
        "environment": [f"{name}={value}"
                        for name, value in sorted(broker_environment.items())],
        "volumes": [f"{broker_script}:{BROKER_SCRIPT_TARGET}:ro"],
    }

    if "crash-loop" in faults:
        services["ws"]["command"] = ["/bin/false"]
    if "probe-failure" in faults:
        services["ws"]["environment"] = sorted(
            entry for entry in services["ws"]["environment"]
            if not entry.startswith("QUALIFY_SERVICE_URL=")
        ) + ["QUALIFY_SERVICE_URL=http://no-such-service.invalid:8000"]
    if "missing-bind" in faults:
        services["ws"].pop("volumes", None)
    if "drop-overlay-env" in faults:
        services["ws"]["environment"] = sorted(
            entry for entry in services["ws"]["environment"]
            if not entry.startswith("IAC_ENABLED=")
        )
    if "unbounded-logs" in faults:
        services["ws"]["logging"] = {"driver": "json-file", "options": {}}

    document = {
        "name": STAGING_PROJECT,
        "services": services,
        # No egress: nothing in this stack can reach the internet, the real
        # broker or the real data feed.
        "networks": {"default": {"name": f"{STAGING_PROJECT}_default", "internal": True}},
    }
    document["x-stockspotter-staging"] = {
        "derived_from_recipe_images": dict(recipe["images"]),
        "faults": sorted(faults),
        "data_dir": data_dir,
        "exposure_gate": (
            "NOT exercisable here: ops/release_preflight.py requires "
            "ALPACA_TRADING_BASE == https://paper-api.alpaca.markets, so "
            "paper_endpoint_configured is false against the fake broker and "
            "exposure_zero() is false. Staging rehearses the refusal direction only."
        ),
    }
    return document


def verify_isolation(document: dict, *, production_root=None, production_data=None) -> list:
    """Every way the generated document could fail to be isolated."""
    problems = []
    if document.get("name") != STAGING_PROJECT:
        problems.append(f"compose project is {document.get('name')!r}, "
                        f"not {STAGING_PROJECT!r}")
    network = (document.get("networks") or {}).get("default") or {}
    if network.get("internal") is not True:
        problems.append("the staging network is not internal; the stack could reach "
                        "the real broker")
    services = document.get("services") or {}
    if set(services) != ROLES | {BROKER_SERVICE}:
        problems.append(f"services are {sorted(services)}, not the five roles plus "
                        f"{BROKER_SERVICE}")
    for name, service in sorted(services.items()):
        if service.get("ports"):
            problems.append(f"{name}: publishes {service['ports']}; the staging stack "
                            "must be unreachable from the host")
        if service.get("env_file"):
            problems.append(f"{name}: references an env_file; the real .env must never "
                            "reach the staging stack")
        if service.get("container_name"):
            problems.append(f"{name}: pins a container name, which could collide with "
                            "production")
        if not service.get("image"):
            problems.append(f"{name}: has no image")
        if "build" in service:
            problems.append(f"{name}: declares a build; staging must not build")
        for entry in service.get("volumes") or []:
            source = str(entry).split(":")[0]
            if production_root and (source == str(production_root)
                                    or source.startswith(str(production_root) + os.sep)
                                    or source.startswith(str(production_root) + "/")):
                problems.append(f"{name}: binds {source} from inside the production "
                                "checkout")
            if production_data and source.rstrip("/") == str(production_data).rstrip("/"):
                problems.append(f"{name}: binds the live data directory {source}")
        for entry in service.get("environment") or []:
            variable, _, value = str(entry).partition("=")
            if name == BROKER_SERVICE:
                continue
            if variable == "ALPACA_TRADING_BASE":
                # Not a credential but the one value that decides whether the
                # stack can reach a real broker at all, so it is checked for
                # equality rather than for the marker.
                if value != BROKER_BASE:
                    problems.append(f"{name}: ALPACA_TRADING_BASE is {value!r}, not the "
                                    f"fake broker {BROKER_BASE!r}")
                continue
            if adapter._secretish(variable) and MARKER not in value:
                problems.append(f"{name}: {variable} does not carry the staging marker; "
                                "it may be a real credential")
    return problems


def command_plan(compose_path, *, roles=None) -> list:
    """The exact commands an operator runs, in order. Nothing is run here."""
    compose_path = str(compose_path)
    base = ["docker", "compose", "-p", STAGING_PROJECT, "-f", compose_path]
    names = [f"{STAGING_PROJECT}-{role}-1" for role in sorted(roles or ROLES)]
    return [
        base + ["up", "-d", "--no-build", "--wait", "--wait-timeout", "180"],
        ["docker", "inspect", "--format", "{{json .}}", *names],
        base + ["logs", "--no-color", "--tail", "200"],
        base + ["down", "--remove-orphans"],
    ]


def compare(document: dict, documents: dict) -> list:
    """Fidelity differences, using the production proof's own comparison.

    `dotenv` is empty on purpose: the staging stack has no `env_file`, so every
    variable it carries must be declared in the document itself.
    """
    staging = {"services": {role: service for role, service in document["services"].items()
                            if role in ROLES}}
    return adapter.conformance_failures(staging, documents, {})


def read_inspect(path) -> dict:
    """`docker inspect --format {{json .}}` output, saved to a file.

    Keyed by each container's own compose service label, so the comparison does
    not depend on the order the names were passed.
    """
    documents = {}
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        document = json.loads(line)
        labels = ((document.get("Config") or {}).get("Labels") or {})
        project = labels.get(adapter.COMPOSE_PROJECT_LABEL)
        if project != STAGING_PROJECT:
            raise SystemExit(
                f"staging_rehearsal: refusing to compare a container from project "
                f"{project!r}; this tool only ever reads {STAGING_PROJECT!r}"
            )
        documents[labels.get(adapter.COMPOSE_SERVICE_LABEL)] = document
    return documents


def write_private(path, text) -> None:
    path = Path(path)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
        handle.write(text)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="staging_rehearsal.py",
        description="Derive and check an isolated staging rehearsal of the captured recipe.",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("plan", "write"):
        command = commands.add_parser(
            name, help="print the staging document and command plan" if name == "plan"
            else "write the staging document privately")
        command.add_argument("--recipe", required=True, type=Path)
        command.add_argument("--data-dir", required=True, type=Path,
                             help="throwaway directory for staging binds; never the live data")
        command.add_argument("--broker-script", type=Path,
                             default=Path(__file__).with_name("staging_broker.py"))
        command.add_argument("--production-root", type=Path, default=Path("/opt/apps/stockspotter"))
        command.add_argument("--production-data", type=Path, default=None)
        command.add_argument("--fault", action="append", default=[],
                             choices=sorted(FAULTS), help="inject a deliberate fault")
        if name == "write":
            command.add_argument("--out", required=True, type=Path)
    compare_command = commands.add_parser(
        "compare", help="compare a saved docker inspect against the staging document")
    compare_command.add_argument("--compose", required=True, type=Path)
    compare_command.add_argument("--inspect", required=True, type=Path)
    faults = commands.add_parser("faults", help="list the injectable faults")
    faults.add_argument("--json", action="store_true")
    return parser


def main(argv=None, out=None) -> int:
    arguments = build_parser().parse_args(argv)
    out = out if out is not None else sys.stdout
    if arguments.command == "faults":
        if arguments.json:
            print(json.dumps(FAULTS, indent=2, sort_keys=True), file=out)
        else:
            for name, description in sorted(FAULTS.items()):
                print(f"{name}: {description}", file=out)
        return 0
    if arguments.command == "compare":
        document = json.loads(arguments.compose.read_text(encoding="utf-8"))
        failures = compare(document, read_inspect(arguments.inspect))
        print(json.dumps({"fidelity_failures": failures,
                          "faithful": not failures}, indent=2), file=out)
        return 0 if not failures else 3
    recipe = load_recipe(arguments.recipe)
    document = derive(recipe, data_dir=arguments.data_dir,
                      broker_script=arguments.broker_script, faults=arguments.fault)
    problems = verify_isolation(document, production_root=arguments.production_root,
                                production_data=arguments.production_data)
    if problems:
        print(json.dumps({"isolation_violations": problems}, indent=2), file=sys.stderr)
        return 4
    rendered = json.dumps(document, indent=2, sort_keys=True)
    if arguments.command == "write":
        write_private(arguments.out, rendered)
        print(json.dumps({"written": str(arguments.out),
                          "plan": [" ".join(command)
                                   for command in command_plan(arguments.out)]},
                         indent=2), file=out)
        return 0
    print(json.dumps({"compose": document,
                      "plan": [" ".join(command)
                               for command in command_plan("<written staging document>")]},
                     indent=2, sort_keys=True), file=out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
