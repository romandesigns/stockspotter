"""Tests for the isolated staging rehearsal generator.

No Docker here either. These tests are about the generated compose document and
its isolation invariants: that the staging stack carries the real recipe's
configuration *shape* while carrying none of its credentials, none of its
published ports, none of its data and no route to the real broker.
"""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest


def _load(name):
    path = Path(__file__).with_name(name + ".py")
    # One module object per process. Executing a second copy would create a
    # second set of Rejected/Deferred/Held classes, and `assertRaises` would
    # then quietly stop matching -- which is exactly what happened once another
    # test module imported these by name rather than by path.
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

SECRET = "alpaca-secret-value"
TOKEN = "x" * 40
PRODUCTION_ROOT = "/opt/apps/stockspotter"
LIVE_DATA = "/home/wavystack/stockspotter-iac-coverage-20261013/data"


def image_id(label):
    import hashlib
    return "sha256:" + hashlib.sha256(label.encode()).hexdigest()


def recipe():
    """A captured recipe shaped exactly like `capture_recipe` writes one."""
    services = {
        "web": {
            "image": image_id("baseline-web"),
            "restart": "unless-stopped",
            "logging": {"driver": "json-file", "options": {"max-size": "50m"}},
            "environment": ["VITE_PUBLIC_BASE=https://stockspotter.app"],
            "ports": ["127.0.0.1:3000:3000/tcp"],
            "command": ["bun", "run", "start"],
            "working_dir": "/app",
        },
        "ws": {
            "image": image_id("baseline-ws"),
            "restart": "unless-stopped",
            "logging": {"driver": "json-file", "options": {"max-size": "256m"}},
            "environment": ["IAC_ENABLED=true", "EXTREME_MOMENTUM_ENABLED=true",
                            "QUALIFY_SERVICE_URL=http://qualify:8000"],
            "env_file": [f"{PRODUCTION_ROOT}/.env"],
            "ports": ["127.0.0.1:8787:8787/tcp", "127.0.0.1:8788:8788/tcp"],
            "volumes": [f"{LIVE_DATA}:/app/data:rw"],
            "command": ["./ws-server"],
            "working_dir": "/app",
            "depends_on": ["qualify"],
        },
        "qualify": {
            "image": image_id("baseline-qualify"),
            "restart": "unless-stopped",
            "logging": {"driver": "json-file", "options": {"max-size": "50m"}},
            "env_file": [f"{PRODUCTION_ROOT}/.env"],
            "command": ["uvicorn", "app:app"],
        },
        "auto-trader": {
            "image": image_id("baseline-auto-trader"),
            "restart": "unless-stopped",
            "logging": {"driver": "json-file", "options": {"max-size": "50m"}},
            "environment": ["AUTO_TRADER_WS_URL=ws://ws:8787"],
            "env_file": [f"{PRODUCTION_ROOT}/.env"],
            "volumes": [f"{LIVE_DATA}:/app/data:rw"],
            "command": ["./auto-trader"],
        },
        "discovery-review": {
            "image": image_id("baseline-discovery-review"),
            "restart": "unless-stopped",
            "logging": {"driver": "json-file", "options": {"max-size": "50m"}},
            "env_file": [f"{PRODUCTION_ROOT}/.env"],
            "volumes": [f"{LIVE_DATA}:/app/data:rw"],
            "command": ["python", "/app/discovery_review.py"],
        },
    }
    dotenv = ["ALPACA_API_KEY", "ALPACA_API_SECRET", "STOCKSPOTTER_API_TOKEN",
              "AUTO_TRADER_EXECUTION_MODE"]
    return {
        "version": 1,
        "project": adapter.PROJECT,
        "images": {role: service["image"] for role, service in services.items()},
        "effective": {
            role: {"env_file": bool(service.get("env_file")),
                   "dotenv": dotenv if service.get("env_file") else [],
                   "literal": dict(adapter._env_map(service.get("environment"))),
                   "image": ["LANG", "PATH"], "identity": []}
            for role, service in services.items()
        },
        "env_binding": {role: {} for role in core.ROLES},
        "policy_digest": "0" * 64,
        "compose": {"name": adapter.PROJECT, "services": services},
    }


def derive(**kwargs):
    options = {"data_dir": "/var/tmp/stockspotter-staging",
               "broker_script": "/opt/stockspotter-controller/staging_broker.py"}
    options.update(kwargs)
    return staging.derive(recipe(), **options)


class IsolationTests(unittest.TestCase):
    def setUp(self):
        self.document = derive()

    def test_the_generated_document_passes_its_own_isolation_check(self):
        self.assertEqual(
            staging.verify_isolation(self.document, production_root=PRODUCTION_ROOT,
                                     production_data=LIVE_DATA),
            [])

    def test_it_is_a_different_compose_project_so_no_live_container_is_touched(self):
        self.assertEqual(self.document["name"], staging.STAGING_PROJECT)
        self.assertNotEqual(self.document["name"], adapter.PROJECT)
        for name in self.document["services"]:
            self.assertNotIn("container_name", self.document["services"][name])

    def test_nothing_is_published_to_the_host(self):
        for name, service in self.document["services"].items():
            self.assertNotIn("ports", service, name)
        rendered = json.dumps(self.document)
        for port in ("3000:3000", "8787:8787", "8788:8788"):
            self.assertNotIn(port, rendered)

    def test_the_network_is_internal_so_the_real_broker_is_unreachable(self):
        self.assertTrue(self.document["networks"]["default"]["internal"])

    def test_the_real_env_file_is_never_referenced(self):
        rendered = json.dumps(self.document)
        self.assertNotIn(f"{PRODUCTION_ROOT}/.env", rendered)
        for service in self.document["services"].values():
            self.assertNotIn("env_file", service)

    def test_every_dotenv_variable_keeps_its_name_but_loses_its_value(self):
        ws = adapter._env_map(self.document["services"]["ws"]["environment"])
        for name in ("ALPACA_API_KEY", "ALPACA_API_SECRET", "STOCKSPOTTER_API_TOKEN",
                     "AUTO_TRADER_EXECUTION_MODE"):
            self.assertIn(name, ws)
            self.assertIn(staging.MARKER, ws[name])
        self.assertNotIn(SECRET, json.dumps(self.document))
        self.assertNotIn(TOKEN, json.dumps(self.document))

    def test_the_staging_token_is_long_enough_for_the_real_length_assertion(self):
        ws = adapter._env_map(self.document["services"]["ws"]["environment"])
        self.assertGreaterEqual(len(ws["STOCKSPOTTER_API_TOKEN"]), 32)

    def test_binds_point_at_the_staging_directory_not_the_live_journals(self):
        volumes = self.document["services"]["ws"]["volumes"]
        self.assertEqual(volumes, ["/var/tmp/stockspotter-staging/app/data:/app/data:rw"])
        self.assertNotIn(LIVE_DATA, json.dumps(self.document))

    def test_the_overlay_configuration_shape_is_carried_over(self):
        ws = self.document["services"]["ws"]
        self.assertIn("IAC_ENABLED=true", ws["environment"])
        self.assertIn("EXTREME_MOMENTUM_ENABLED=true", ws["environment"])
        self.assertEqual(ws["command"], ["./ws-server"])
        self.assertEqual(ws["logging"]["options"]["max-size"], "256m")
        self.assertEqual(ws["image"], image_id("baseline-ws"))

    def test_the_fake_broker_is_wired_in_and_publishes_nothing(self):
        broker = self.document["services"][staging.BROKER_SERVICE]
        self.assertEqual(broker["entrypoint"], ["python", staging.BROKER_SCRIPT_TARGET])
        self.assertNotIn("ports", broker)
        for role in core.ROLES:
            environment = adapter._env_map(self.document["services"][role]["environment"])
            self.assertEqual(environment["ALPACA_TRADING_BASE"], staging.BROKER_BASE)

    def test_staging_cannot_satisfy_the_real_exposure_gate_and_says_so(self):
        """The real preflight requires the literal production paper URL."""
        environment = adapter._env_map(self.document["services"]["ws"]["environment"])
        self.assertNotEqual(environment["ALPACA_TRADING_BASE"],
                            "https://paper-api.alpaca.markets")
        self.assertIn("NOT exercisable here",
                      self.document["x-stockspotter-staging"]["exposure_gate"])

    def test_no_service_can_build(self):
        for service in self.document["services"].values():
            self.assertNotIn("build", service)

    def test_the_isolation_check_catches_every_violation_it_claims_to(self):
        cases = {
            "publishes": lambda d: d["services"]["ws"].__setitem__(
                "ports", ["127.0.0.1:8787:8787"]),
            "env_file": lambda d: d["services"]["ws"].__setitem__(
                "env_file", [f"{PRODUCTION_ROOT}/.env"]),
            "not internal": lambda d: d["networks"]["default"].__setitem__("internal", False),
            "not 'stockspotter-staging'": lambda d: d.__setitem__("name", adapter.PROJECT),
            "live data directory": lambda d: d["services"]["ws"].__setitem__(
                "volumes", [f"{LIVE_DATA}:/app/data:rw"]),
            "production checkout": lambda d: d["services"]["ws"].__setitem__(
                "volumes", [f"{PRODUCTION_ROOT}/data:/app/data:rw"]),
            "may be a real credential": lambda d: d["services"]["ws"].__setitem__(
                "environment", [f"ALPACA_API_SECRET={SECRET}"]),
            "collide with production": lambda d: d["services"]["ws"].__setitem__(
                "container_name", "stockspotter-vps-ws-1"),
            "must not build": lambda d: d["services"]["ws"].__setitem__(
                "build", {"context": "."}),
        }
        for expected, break_it in cases.items():
            with self.subTest(expected=expected):
                document = derive()
                break_it(document)
                problems = staging.verify_isolation(
                    document, production_root=PRODUCTION_ROOT, production_data=LIVE_DATA)
                self.assertTrue(any(expected in problem for problem in problems),
                                f"{expected} not reported in {problems}")


class FaultTests(unittest.TestCase):
    def test_every_documented_fault_is_injectable_and_changes_the_document(self):
        baseline = json.dumps(derive(), sort_keys=True)
        for fault in sorted(staging.FAULTS):
            with self.subTest(fault=fault):
                document = derive(faults=[fault])
                self.assertNotEqual(json.dumps(document, sort_keys=True), baseline)
                self.assertEqual(document["x-stockspotter-staging"]["faults"], [fault])
                # A fault must never break isolation.
                self.assertEqual(
                    staging.verify_isolation(document, production_root=PRODUCTION_ROOT,
                                             production_data=LIVE_DATA), [])

    def test_the_faults_do_what_they_say(self):
        self.assertEqual(derive(faults=["crash-loop"])["services"]["ws"]["command"],
                         ["/bin/false"])
        self.assertNotIn("volumes", derive(faults=["missing-bind"])["services"]["ws"])
        self.assertFalse(any(
            entry.startswith("IAC_ENABLED=") for entry in
            derive(faults=["drop-overlay-env"])["services"]["ws"]["environment"]))
        self.assertEqual(
            derive(faults=["unbounded-logs"])["services"]["ws"]["logging"]["options"], {})
        self.assertIn("STAGING_BROKER_POSITIONS=1",
                      derive(faults=["broker-open-position"])
                      ["services"][staging.BROKER_SERVICE]["environment"])
        self.assertIn("QUALIFY_SERVICE_URL=http://no-such-service.invalid:8000",
                      derive(faults=["probe-failure"])["services"]["ws"]["environment"])

    def test_an_unknown_fault_is_refused(self):
        with self.assertRaises(SystemExit):
            derive(faults=["delete-production"])


class SyntheticEnvironmentTests(unittest.TestCase):
    """One source for the document's environment and for the stage `.env`.

    The controller attributes a container's environment by digest against the
    host's own `.env` (`deployment_host._role_env`). A staging stack carries no
    `env_file`, so unless that file and the document agree exactly, every
    synthetic secret is `unreproducible` and adoption is `Held` -- which reads
    like a production misconfiguration rather than a fixture mistake.
    """

    def setUp(self):
        self.document = derive()

    def test_the_fixed_synthetic_values_reach_every_role(self):
        for role in core.ROLES:
            values = adapter._env_map(self.document["services"][role]["environment"])
            with self.subTest(role=role):
                self.assertEqual(values["ALPACA_TRADING_BASE"], staging.BROKER_BASE)
                self.assertTrue(values["ALPACA_API_SECRET"].startswith(staging.MARKER))
                self.assertGreaterEqual(len(values["STOCKSPOTTER_API_TOKEN"]), 48)

    def test_synthetic_env_is_the_only_source_of_those_values(self):
        """`derive` must not grow a second, drifting copy of this map."""
        shared = staging.synthetic_env()
        for role in core.ROLES:
            values = adapter._env_map(self.document["services"][role]["environment"])
            for name, value in shared.items():
                with self.subTest(role=role, name=name):
                    self.assertEqual(values[name], value)

    def test_a_dotenv_name_is_rendered_synthetically_not_copied(self):
        values = staging.synthetic_env(["ALPACA_API_SECRET", "AUTO_TRADER_EXECUTION_MODE"])
        self.assertEqual(values["AUTO_TRADER_EXECUTION_MODE"],
                         f"{staging.MARKER}-auto_trader_execution_mode")
        # The four fixed names are assigned last and win, as before.
        self.assertEqual(values["ALPACA_API_SECRET"], f"{staging.MARKER}-secret")

    def test_the_generated_dotenv_covers_every_secretish_variable(self):
        text = staging.synthetic_dotenv(self.document)
        covered = {line.split("=", 1)[0] for line in text.splitlines()
                   if line and not line.startswith("#")}
        for role in core.ROLES:
            observed = adapter._env_map(self.document["services"][role]["environment"])
            secretish = {name for name in observed if adapter._secretish(name)}
            with self.subTest(role=role):
                self.assertTrue(secretish)
                self.assertTrue(secretish <= covered, sorted(secretish - covered))

    def test_the_generated_dotenv_is_a_subset_of_every_role(self):
        """A name in `.env` that a role lacks is `dropped`, and adoption Holds."""
        text = staging.synthetic_dotenv(self.document)
        named = {line.split("=", 1)[0] for line in text.splitlines()
                 if line and not line.startswith("#")}
        for role in core.ROLES:
            observed = adapter._env_map(self.document["services"][role]["environment"])
            with self.subTest(role=role):
                self.assertTrue(named <= set(observed), sorted(named - set(observed)))

    def test_the_generated_dotenv_round_trips_through_the_real_parser(self):
        """Digests must match what `Files.env_pairs` reads back, or adoption Holds."""
        text = staging.synthetic_dotenv(self.document)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env"
            path.write_text(text, encoding="utf-8")
            pairs = adapter.Files().env_pairs(path)
        observed = adapter._env_map(self.document["services"]["ws"]["environment"])
        self.assertTrue(pairs)
        for name, digest in pairs.items():
            with self.subTest(name=name):
                self.assertEqual(digest, adapter._digest_text(observed[name]))

    def test_every_role_carries_the_union_of_dotenv_names(self):
        """`web` has no env_file in the recipe, yet must still carry them all.

        The fixture's `AUTO_TRADER_EXECUTION_MODE` reaches four roles through
        `env_file` and not `web`. Per-role name sets would make the stack
        unadoptable: `.env` must cover every secretish variable *and* be a
        subset of every role, and those two cannot both hold.
        """
        names = [set(adapter._env_map(self.document["services"][role]["environment"]))
                 for role in sorted(core.ROLES)]
        shared = set.intersection(*names)
        self.assertIn("AUTO_TRADER_EXECUTION_MODE", shared)
        for role in core.ROLES:
            values = adapter._env_map(self.document["services"][role]["environment"])
            with self.subTest(role=role):
                self.assertIn("AUTO_TRADER_EXECUTION_MODE", values)
        self.assertEqual(staging.dotenv_failures(self.document), [])

    def test_a_recipe_with_a_split_secret_is_still_adoptable(self):
        """The case that used to be refused: a secret in four roles, not five."""
        record = recipe()
        record["effective"]["web"]["dotenv"] = []
        record["effective"]["ws"]["dotenv"] = ["SENTRY_DSN", "ALPACA_API_KEY"]
        document = staging.derive(record, data_dir="/var/tmp/stockspotter-staging",
                                  broker_script="/var/tmp/stockspotter-staging/broker.py")
        self.assertEqual(staging.dotenv_failures(document), [])
        covered = {line.split("=", 1)[0] for line in
                   staging.synthetic_dotenv(document).splitlines()
                   if line and not line.startswith("#")}
        self.assertIn("SENTRY_DSN", covered)
        for role in core.ROLES:
            self.assertIn("SENTRY_DSN",
                          adapter._env_map(document["services"][role]["environment"]))

    def test_a_secretish_variable_missing_from_one_role_is_refused(self):
        """No single `.env` can cover it, so say so instead of emitting one."""
        document = derive()
        document["services"]["web"]["environment"] = [
            entry for entry in document["services"]["web"]["environment"]
            if not entry.startswith("ALPACA_API_SECRET=")
        ]
        problems = staging.dotenv_failures(document)
        self.assertTrue(any("ALPACA_API_SECRET" in problem for problem in problems), problems)
        with self.assertRaises(ValueError) as caught:
            staging.synthetic_dotenv(document)
        self.assertIn("cannot derive a stage .env", str(caught.exception))

    def test_a_value_env_file_parsing_would_change_is_refused(self):
        document = derive()
        for role in core.ROLES:
            service = document["services"][role]
            service["environment"] = [entry for entry in service["environment"]
                                      if not entry.startswith("ALPACA_API_KEY=")]
            service["environment"].append('ALPACA_API_KEY="staging-not-a-real-credential"')
        problems = staging.dotenv_failures(document)
        self.assertTrue(any("quoted" in problem for problem in problems), problems)

    def test_a_document_missing_a_role_is_refused(self):
        document = derive()
        del document["services"]["ws"]
        self.assertTrue(any("missing roles" in problem
                            for problem in staging.dotenv_failures(document)))


class FidelityTests(unittest.TestCase):
    """The comparison uses the production proof's own implementation."""

    def setUp(self):
        self.document = derive()

    def observed(self, role, **changes):
        service = self.document["services"][role]
        binds = [f"{entry.split(':')[0]}:{entry.split(':')[1]}:rw"
                 for entry in service.get("volumes") or []]
        document = {
            "Image": service["image"],
            "Config": {
                "Env": list(service["environment"]) + ["PATH=/usr/bin", "LANG=C.UTF-8"],
                "Cmd": service.get("command"),
                "Entrypoint": service.get("entrypoint"),
                "User": "",
                "WorkingDir": service.get("working_dir", ""),
                "Labels": {adapter.COMPOSE_PROJECT_LABEL: staging.STAGING_PROJECT,
                           adapter.COMPOSE_SERVICE_LABEL: role},
            },
            "HostConfig": {
                "Binds": binds,
                "PortBindings": {},
                "RestartPolicy": {"Name": service["restart"]},
                "LogConfig": {"Type": service["logging"]["driver"],
                              "Config": dict(service["logging"]["options"])},
            },
        }
        document["Config"].update(changes.pop("config", {}))
        document["HostConfig"].update(changes.pop("host_config", {}))
        document.update(changes)
        return document

    def all_observed(self, **overrides):
        return {role: overrides.get(role, self.observed(role)) for role in core.ROLES}

    def test_a_faithful_staging_stack_reports_no_differences(self):
        self.assertEqual(staging.compare(self.document, self.all_observed()), [])

    def test_the_broker_service_is_not_part_of_the_fidelity_comparison(self):
        failures = staging.compare(self.document, self.all_observed())
        self.assertFalse(any(staging.BROKER_SERVICE in failure for failure in failures))

    def test_a_lost_overlay_variable_is_reported(self):
        broken = self.observed("ws")
        broken["Config"]["Env"] = [entry for entry in broken["Config"]["Env"]
                                   if not entry.startswith("IAC_ENABLED=")]
        failures = staging.compare(self.document, self.all_observed(ws=broken))
        self.assertTrue(any("IAC_ENABLED" in failure for failure in failures))

    def test_a_lost_bind_a_wrong_image_and_dropped_rotation_are_reported(self):
        broken = self.observed("ws", host_config={"Binds": [],
                                                  "LogConfig": {"Type": "json-file",
                                                                "Config": {}}},
                               Image=image_id("something-else"))
        failures = staging.compare(self.document, self.all_observed(ws=broken))
        self.assertTrue(any("bind mounts" in failure for failure in failures))
        self.assertTrue(any("logging differs" in failure for failure in failures))
        self.assertTrue(any("not the pinned" in failure for failure in failures))

    def test_a_missing_container_is_a_failure_not_a_crash(self):
        documents = self.all_observed()
        del documents["ws"]
        self.assertIn("ws: no such container", staging.compare(self.document, documents))

    def test_an_undeclared_variable_from_the_image_is_reported(self):
        broken = self.observed("ws")
        broken["Config"]["Env"].append("UNEXPECTED_PROVIDER=http://elsewhere")
        failures = staging.compare(self.document, self.all_observed(ws=broken))
        self.assertTrue(any("UNEXPECTED_PROVIDER" in failure for failure in failures))

    def test_reading_an_inspect_file_refuses_production_containers(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "inspect.json"
            document = self.observed("ws")
            document["Config"]["Labels"][adapter.COMPOSE_PROJECT_LABEL] = adapter.PROJECT
            path.write_text(json.dumps(document) + "\n", encoding="utf-8")
            with self.assertRaises(SystemExit) as caught:
                staging.read_inspect(path)
            self.assertIn("stockspotter-vps", str(caught.exception))

    def test_reading_an_inspect_file_keys_by_the_compose_service_label(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "inspect.json"
            path.write_text("\n".join(json.dumps(self.observed(role))
                                      for role in sorted(core.ROLES)) + "\n",
                            encoding="utf-8")
            self.assertEqual(set(staging.read_inspect(path)), set(core.ROLES))


class CommandLineTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.recipe = Path(self.directory.name) / "baseline-recipe.json"
        self.recipe.write_text(json.dumps(recipe()), encoding="utf-8")
        self.out = io.StringIO()

    def run_main(self, argv):
        return staging.main(argv, out=self.out)

    def test_plan_prints_the_document_and_the_exact_commands(self):
        self.assertEqual(self.run_main(
            ["plan", "--recipe", str(self.recipe), "--data-dir", "/var/tmp/staging",
             "--production-root", PRODUCTION_ROOT, "--production-data", LIVE_DATA]), 0)
        report = json.loads(self.out.getvalue())
        self.assertEqual(report["compose"]["name"], staging.STAGING_PROJECT)
        self.assertTrue(any(f"-p {staging.STAGING_PROJECT}" in command
                            for command in report["plan"]))
        self.assertTrue(any("up -d --no-build --wait" in command
                            for command in report["plan"]))
        self.assertTrue(any("down --remove-orphans" in command for command in report["plan"]))
        # Never the production project, in any command of the plan.
        for command in report["plan"]:
            self.assertNotIn(f"-p {adapter.PROJECT}", command)

    def test_write_saves_the_document_privately_and_plans_against_it(self):
        out = Path(self.directory.name) / "staging.json"
        self.assertEqual(self.run_main(
            ["write", "--recipe", str(self.recipe), "--data-dir", "/var/tmp/staging",
             "--out", str(out)]), 0)
        document = json.loads(out.read_text(encoding="utf-8"))
        self.assertEqual(document["name"], staging.STAGING_PROJECT)
        if sys.platform != "win32":
            self.assertEqual(out.stat().st_mode & 0o777, 0o600)
        self.assertEqual(json.loads(self.out.getvalue())["written"], str(out))

    def test_compare_exits_nonzero_when_the_staging_stack_is_not_faithful(self):
        compose = Path(self.directory.name) / "staging.json"
        self.run_main(["write", "--recipe", str(self.recipe), "--data-dir", "/var/tmp/staging",
                       "--out", str(compose)])
        inspect = Path(self.directory.name) / "inspect.json"
        document = {
            "Image": image_id("wrong"),
            "Config": {"Env": [], "Cmd": None, "Entrypoint": None, "User": "",
                       "WorkingDir": "/app",
                       "Labels": {adapter.COMPOSE_PROJECT_LABEL: staging.STAGING_PROJECT,
                                  adapter.COMPOSE_SERVICE_LABEL: "ws"}},
            "HostConfig": {"Binds": [], "PortBindings": {},
                           "RestartPolicy": {"Name": "no"},
                           "LogConfig": {"Type": "json-file", "Config": {}}},
        }
        inspect.write_text(json.dumps(document) + "\n", encoding="utf-8")
        self.out = io.StringIO()          # the `write` above already printed
        self.assertEqual(self.run_main(["compare", "--compose", str(compose),
                                        "--inspect", str(inspect)]), 3)
        report = json.loads(self.out.getvalue())
        self.assertFalse(report["faithful"])
        self.assertTrue(any("ws" in failure for failure in report["fidelity_failures"]))

    def test_faults_can_be_listed_for_review(self):
        self.assertEqual(self.run_main(["faults", "--json"]), 0)
        self.assertEqual(set(json.loads(self.out.getvalue())), set(staging.FAULTS))

    def test_a_recipe_that_is_not_a_captured_recipe_is_refused(self):
        bad = Path(self.directory.name) / "bad.json"
        bad.write_text(json.dumps({"version": 1}), encoding="utf-8")
        with self.assertRaises(SystemExit):
            self.run_main(["plan", "--recipe", str(bad), "--data-dir", "/var/tmp/staging"])

    def test_an_unknown_fault_is_refused_by_the_parser(self):
        with self.assertRaises(SystemExit):
            self.run_main(["plan", "--recipe", str(self.recipe), "--data-dir", "/tmp/x",
                           "--fault", "delete-production"])


if __name__ == "__main__":
    unittest.main()
