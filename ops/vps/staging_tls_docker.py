"""Owned, isolated real-Docker preflight fixture; never a live-account proof."""
import argparse
import hashlib
import inspect
import json
import os
from pathlib import Path
import subprocess
import stat
import time
import uuid
import staging_tls
import deployment_host as adapter

HERE = Path(__file__).resolve().parent
OWNER = "stockspotter-tls-fixture-20261014"
LABEL = "io.stockspotter.fixture-owner"
ROLES = ("ws", "web", "qualify", "auto-trader", "discovery-review")

def run(argv, *, data=None, check=True):
    result = subprocess.run(argv, input=data, text=True, capture_output=True, timeout=60)
    if check and result.returncode:
        raise RuntimeError(f"command failed: {argv[:3]}: {result.stderr[-800:]}")
    return result

def snapshot():
    documents = json.loads(run(["docker", "inspect", *[f"stockspotter-vps-{r}-1" for r in ROLES]]).stdout)
    return {d["Name"]: {"id": d["Id"], "image": d["Image"], "started": d["State"]["StartedAt"],
                       "running": d["State"]["Running"],
                       "config": hashlib.sha256(json.dumps([d["Config"], d["HostConfig"], d["Mounts"]], sort_keys=True).encode()).hexdigest()}
            for d in documents}

def remove_owned(identity, network=False):
    kind = ["docker", "network", "inspect"] if network else ["docker", "inspect"]
    result = run(kind + [identity], check=False)
    if result.returncode: return
    document = json.loads(result.stdout)[0]
    labels = document.get("Labels", {}) if network else document.get("Config", {}).get("Labels", {})
    if labels.get(LABEL) != OWNER:
        raise RuntimeError("refuse cleanup of a resource whose ownership label differs")
    run(["docker", "network", "rm", identity] if network else ["docker", "rm", "-f", identity])

def remove_certificates(directory):
    directory = Path(directory)
    if not directory.exists(): return
    owner = directory.parent
    if directory.name != 'certificates' or not owner.name.startswith('tls-docker-'):
        raise RuntimeError('refuse certificate cleanup outside the generated fixture')
    if directory.is_symlink() or owner.is_symlink():
        raise RuntimeError('refuse symlink certificate cleanup')
    if os.name == 'posix':
        info = owner.lstat()
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise RuntimeError('fixture owner is no longer private')
    for name in ('key.pem', 'certificate.pem'):
        path = directory / name
        if path.is_symlink(): raise RuntimeError('refuse a substituted fixture file')
        if path.exists():
            if not path.is_file(): raise RuntimeError('fixture file became a directory')
            path.unlink()
    directory.rmdir()  # Never recursively deletes unknown contents.


def _main(cleanup):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence-root', type=Path, required=True)
    arguments = parser.parse_args()
    if os.name != 'posix':
        raise RuntimeError('this VPS Docker fixture requires POSIX ownership checks')
    raw = arguments.evidence_root
    info = raw.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('evidence root must be a private, owned, non-symlink directory')
    evidence_root = raw.resolve()
    source_root = HERE.parent.parent if (HERE.parent / 'release_preflight.py').is_file() else HERE
    for protected_root in (source_root.resolve(), adapter.PRODUCTION_ROOT.resolve()):
        if evidence_root == protected_root or evidence_root.is_relative_to(protected_root):
            raise RuntimeError('evidence root must be outside the source and production checkouts')
    before = snapshot()
    image = before["/stockspotter-vps-qualify-1"]["image"]
    assert image.startswith("sha256:")
    root = evidence_root / ("tls-docker-" + uuid.uuid4().hex[:12])
    root.mkdir(mode=0o700)
    cleanup['certificates'] = root / 'certificates'
    fixture = staging_tls.provision(root / "certificates")
    certificate, key = fixture["certificate"], fixture["key"]
    preflight_path = HERE.parent / 'release_preflight.py'
    if not preflight_path.is_file(): preflight_path = HERE / 'release_preflight.py'
    preflight = preflight_path.read_text()
    evidence = {"scope": "synthetic isolated real-Docker HTTPS and actual exposure gate only; not a complete controller rehearsal or live account proof",
                "image": image, "preflight_sha256": hashlib.sha256(preflight.encode()).hexdigest(),
                "certificate_sha256": fixture['certificate_sha256'],
                "exposure_gate_source_sha256": hashlib.sha256(inspect.getsource(adapter.LinuxHost.exposure_zero).encode()).hexdigest(),
                "cases": []}
    for name, fault in [("zero", {}), ("positions", {"STAGING_BROKER_POSITIONS": "1"}),
                        ("orders", {"STAGING_BROKER_OPEN_ORDERS": "1"}),
                        ("blocked", {"STAGING_BROKER_BLOCKED": "1"}),
                        ("unreachable", {"STAGING_BROKER_UNREACHABLE": "1"}),
                        ("simulated", {"STAGING_BROKER_SIMULATED_POSITIONS": "1"}),
                        ("untrusted", {})]:
        prefix = "stockspotter-stage-tls-" + uuid.uuid4().hex[:12]
        network = None
        containers = []
        case_root = root / prefix
        case_root.mkdir(mode=0o700)
        try:
            network = prefix  # Register the name before a create can partially fail.
            network = run(["docker", "network", "create", "--internal", "--label", f"{LABEL}={OWNER}", prefix]).stdout.strip()
            assert json.loads(run(["docker", "network", "inspect", network]).stdout)[0]["Internal"] is True
            common = ["--label", f"{LABEL}={OWNER}", "--network", prefix, "--read-only",
                      "--memory", "128m", "--cpus", "0.25", "--pids-limit", "64",
                      "--tmpfs", "/tmp:rw,noexec,nosuid,size=8m"]
            broker_env = {"STAGING_BROKER_PORT": "443", "STAGING_BROKER_TLS_CERT": "/fixture-cert.pem",
                          "STAGING_BROKER_TLS_KEY": "/fixture-key.pem",
                          "STAGING_BROKER_STATUS_PORT": "8788", **fault}
            broker_args = ["docker", "create", "--name", prefix + "-broker", *common,
                           "--network-alias", "paper-api.alpaca.markets", "--network-alias", "ws", "--entrypoint", "python"]
            for n, v in broker_env.items(): broker_args += ["--env", f"{n}={v}"]
            broker_args += ["--mount", f"type=bind,src={HERE / 'staging_broker.py'},dst=/fixture.py,readonly",
                            "--mount", f"type=bind,src={certificate},dst=/fixture-cert.pem,readonly",
                            "--mount", f"type=bind,src={key},dst=/fixture-key.pem,readonly", image, "/fixture.py"]
            containers.append(prefix + '-broker')
            broker = run(broker_args).stdout.strip(); containers[-1] = broker
            run(["docker", "start", broker])
            readiness = "import ssl,urllib.request,urllib.error; c=ssl.create_default_context(cafile='/fixture-cert.pem');\ntry: urllib.request.urlopen('https://localhost/v2/account',context=c,timeout=1).close()\nexcept urllib.error.HTTPError as e: assert e.code==503; e.close()"
            for attempt in range(20):
                if run(["docker", "exec", broker, "python", "-c", readiness], check=False).returncode == 0: break
                time.sleep(0.2)
            else: raise RuntimeError("synthetic broker never became ready")
            client_env = {"ALPACA_TRADING_BASE": staging_tls.PAPER_URL,
                          "ALPACA_API_KEY": "staging-not-a-real-credential-key",
                          "ALPACA_API_SECRET": "staging-not-a-real-credential-secret",
                          "STOCKSPOTTER_API_TOKEN": "staging-not-a-real-credential-token".ljust(48, "x"),
                          "SSL_CERT_FILE": "/fixture-cert.pem", "NO_PROXY": "*",
                          "HTTP_PROXY": "", "HTTPS_PROXY": "", "ALL_PROXY": ""}
            if name == 'untrusted': client_env['SSL_CERT_FILE'] = '/missing-fixture-ca.pem'
            args = ["docker", "create", "-i", "--name", prefix + "-client", *common, "--entrypoint", "python"]
            for n, v in client_env.items(): args += ["--env", f"{n}={v}"]
            args += ["--mount", f"type=bind,src={certificate},dst=/fixture-cert.pem,readonly", image, "-c", "import time; time.sleep(300)"]
            containers.append(prefix + '-client')
            client = run(args).stdout.strip(); containers[-1] = client
            run(['docker', 'start', client])
            for identity in containers:
                d = json.loads(run(["docker", "inspect", identity]).stdout)[0]
                assert not d["HostConfig"].get("PortBindings")
                assert d["Image"] == image
                assert d['HostConfig']['Memory'] == 128 * 1024 * 1024
                assert d['HostConfig']['NanoCpus'] == 250_000_000
                assert d['HostConfig']['PidsLimit'] == 64
                assert d['HostConfig']['ReadonlyRootfs'] is True
                assert d['HostConfig']['NetworkMode'] == prefix
                assert d['Config']['Entrypoint'] == ['python']
            result = run(["docker", "exec", "-i", client, 'python', '-'], data=preflight, check=False)
            if name in ("unreachable", "untrusted"):
                assert result.returncode != 0
                assert ('503' in result.stderr) if name == 'unreachable' else ('CERTIFICATE_VERIFY_FAILED' in result.stderr)
                evidence["cases"].append({"name": name, "refused": True})
            else:
                assert result.returncode == 0, result.stderr[-800:]
                report = json.loads(result.stdout)
                assert report["paper_endpoint_configured"] is True and report["account_active"] is True
                assert report["api_token_present_and_long_enough"] is True
                expected = (1 if name == "positions" else 0, 1 if name == "orders" else 0, name == "blocked")
                assert (report["positions"], report["open_orders"], report["trading_blocked"]) == expected
                evidence["cases"].append({"name": name, "positions": report["positions"],
                                           "open_orders": report["open_orders"], "blocked": report["trading_blocked"]})
            def fixture_runner(argv, *, cwd=None, stdin=None, timeout=120):
                # The real gate may execute Python only in our owned client.
                # Any new probe/role/command fails before reaching Docker.
                if tuple(argv) != ('docker', 'exec', '-i', client, 'python', '-'):
                    raise RuntimeError('gate attempted a command outside the owned fixture')
                result = subprocess.run(list(argv), input=stdin, text=True, capture_output=True, timeout=min(timeout, 60))
                return adapter.Result(tuple(argv), result.returncode, result.stdout, result.stderr)
            host = adapter.LinuxHost(project=prefix, stage_owner_root=case_root,
                                     root=case_root / 'repo', controller_root=preflight_path.parent,
                                     control_dir=case_root / 'unused-control',
                                     staging=case_root / 'unused-candidate', runner=fixture_runner)
            host.containers = {'qualify': client}
            allowed = host.exposure_zero()
            assert allowed is (name == 'zero'), 'actual exposure gate disagreed with the injected fault'
            evidence['cases'][-1]['exposureGateAllowed'] = allowed
        finally:
            errors = []
            for identity in reversed(containers):
                try: remove_owned(identity)
                except Exception as error: errors.append(type(error).__name__)
            if network:
                try: remove_owned(network, network=True)
                except Exception as error: errors.append(type(error).__name__)
            try:
                if snapshot() != before: errors.append('ProductionChanged')
            except Exception as error: errors.append(type(error).__name__)
            try: case_root.rmdir()
            except OSError: errors.append('UnexpectedFixtureFiles')
            if errors: raise RuntimeError('fixture cleanup/protection failed: ' + ','.join(errors))
    remove_certificates(cleanup['certificates'])
    evidence['ephemeralCertificateKeyRemoved'] = True
    evidence['resourceLimitsVerified'] = {'memoryBytes': 134217728, 'nanoCpus': 250000000, 'pids': 64, 'readOnly': True}
    evidence["production_unchanged"] = True
    path = root / "evidence.json"
    path.write_text(json.dumps(evidence, indent=2)); os.chmod(path, 0o600)
    print(json.dumps({"evidence": str(path), **evidence}))

def main():
    if not __debug__:
        raise RuntimeError('fixture requires assertions enabled; refuse python -O')
    cleanup = {}
    try:
        _main(cleanup)
    finally:
        if 'certificates' in cleanup: remove_certificates(cleanup['certificates'])


if __name__ == "__main__": main()
