#!/usr/bin/env python3
"""JavaScript advisory gate, scoped to what a release actually ships.

`bun audit` reports against the whole lockfile, which covers two products
with different release pipelines: the server release (web client + shared
types, deployed to the VPS) and the mobile app (Expo/EAS, built manually).
A high advisory that only the mobile toolchain can reach used to block every
server release, even though no server image installs or contains the package.

This gate keeps the audit unsuppressed -- no `--ignore`, no allowlist of
advisory ids -- and instead answers one question per surface: can any
workspace of THIS surface reach a package with a high or critical advisory?
Reachability is the dependency closure computed from bun.lock, using the same
nearest-ancestor resolution the lockfile's nested keys encode.

It fails closed. Any of these is a failure, not a pass:
  * a workspace in bun.lock that no surface claims (a new workspace must be
    classified here before anything can ship);
  * a regular dependency that cannot be resolved in the lockfile;
  * an advised package that no workspace's closure reaches (reachability is
    then unknown, so nothing is certified);
  * audit output that cannot be parsed.

Advisories are matched by package NAME, not version: if a surface reaches any
copy of an advised package, it is blocked. That can over-block, never
under-block.

usage: js_advisory_gate.py --surface {server,mobile} [--lock bun.lock] [--audit-json FILE]
Exit 0 = surface clean, 1 = blocked or cannot decide.
"""
import argparse
import json
import re
import subprocess
import sys

# Every workspace in bun.lock must appear in exactly one surface.
SURFACES = {
    "server": ["", "apps/client", "packages/shared-types"],
    "mobile": ["apps/mobile"],
}
BLOCKING = {"high", "critical"}
DEP_FIELDS = ("dependencies", "devDependencies", "optionalDependencies", "peerDependencies")


class CannotDecide(Exception):
    pass


def load_lock(path):
    with open(path, encoding="utf-8") as f:
        text = f.read()
    # bun.lock is JSON with trailing commas.
    return json.loads(re.sub(r",(\s*[}\]])", r"\1", text))


def split_key(key):
    """A lock key is a path of package names; scoped names span two segments."""
    parts, out, i = key.split("/"), [], 0
    while i < len(parts):
        if parts[i].startswith("@") and i + 1 < len(parts):
            out.append(parts[i] + "/" + parts[i + 1])
            i += 2
        else:
            out.append(parts[i])
            i += 1
    return out


def resolve(packages, chain, name):
    """Nearest-ancestor lookup: `<chain>/<name>`, then each shorter prefix, then `<name>`."""
    for n in range(len(chain), -1, -1):
        key = "/".join(chain[:n] + [name])
        if key in packages:
            return key
    return None


def meta_of(entry):
    return next((x for x in entry if isinstance(x, dict)), {})


def closure(lock, workspace):
    packages, ws = lock["packages"], lock["workspaces"][workspace]
    start = [ws["name"]] if workspace else []
    seen, stack = set(), []

    def push(chain, deps, required):
        for name in deps:
            key = resolve(packages, chain, name)
            if key is None:
                if required:
                    raise CannotDecide(f"dependency {name!r} of {'/'.join(chain) or workspace!r} is not in the lockfile")
                continue
            stack.append(key)

    for field in DEP_FIELDS:
        # Peers and optionals may legitimately be absent; regular and dev
        # dependencies of a workspace may not.
        push(start, ws.get(field, {}), required=field in ("dependencies", "devDependencies"))
    while stack:
        key = stack.pop()
        if key in seen:
            continue
        seen.add(key)
        meta = meta_of(packages[key])
        if packages[key][0].split("@")[-1].startswith("workspace:"):
            continue  # another workspace: classified and audited as its own surface member
        chain = split_key(key)
        push(chain, meta.get("dependencies", {}), required=True)
        push(chain, meta.get("optionalDependencies", {}), required=False)
        push(chain, meta.get("peerDependencies", {}), required=False)
    return seen


def names(lock, keys):
    return {split_key(k)[-1] for k in keys}


def blocking_advisories(audit):
    out = {}
    if not isinstance(audit, dict):
        raise CannotDecide("audit output is not an object")
    for package, advisories in audit.items():
        hits = [a for a in advisories if str(a.get("severity", "")).lower() in BLOCKING]
        if hits:
            out[package] = hits
    return out


def decide(lock, audit, surface):
    claimed = [w for ws in SURFACES.values() for w in ws]
    unclaimed = sorted(set(lock["workspaces"]) - set(claimed))
    if unclaimed:
        raise CannotDecide(f"workspaces not assigned to any surface: {unclaimed}")
    duplicated = sorted({w for w in claimed if claimed.count(w) > 1})
    missing = sorted(set(claimed) - set(lock["workspaces"]))
    if duplicated or missing:
        raise CannotDecide(f"surface map is inconsistent (duplicated {duplicated}, missing {missing})")
    reach = {s: set().union(*(names(lock, closure(lock, w)) for w in ws)) for s, ws in SURFACES.items()}
    advised = blocking_advisories(audit)
    unlocated = sorted(p for p in advised if not any(p in r for r in reach.values()))
    if unlocated:
        raise CannotDecide(f"advised packages no workspace reaches (reachability unknown): {unlocated}")
    blocked = {p: a for p, a in advised.items() if p in reach[surface]}
    elsewhere = {p: sorted(s for s, r in reach.items() if p in r) for p in advised if p not in blocked}
    return blocked, elsewhere


def run_audit():
    proc = subprocess.run(["bun", "audit", "--json"], capture_output=True, text=True)
    try:
        return json.loads(proc.stdout or "{}")
    except ValueError:
        raise CannotDecide(f"bun audit output is not JSON (exit {proc.returncode}): {proc.stderr.strip()[:300]}")


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--surface", required=True, choices=sorted(SURFACES))
    ap.add_argument("--lock", default="bun.lock")
    ap.add_argument("--audit-json")
    args = ap.parse_args(argv)
    try:
        lock = load_lock(args.lock)
        if args.audit_json:
            with open(args.audit_json, encoding="utf-8") as f:
                audit = json.load(f)
        else:
            audit = run_audit()
        blocked, elsewhere = decide(lock, audit, args.surface)
    except (CannotDecide, OSError, ValueError, KeyError) as e:
        print(f"CANNOT DECIDE ({args.surface}): {e}")
        return 1
    for package, surfaces in sorted(elsewhere.items()):
        print(f"not reachable from {args.surface}: {package} (reachable from: {', '.join(surfaces)})")
    for package, advisories in sorted(blocked.items()):
        for a in advisories:
            print(f"BLOCKED {args.surface}: {package} {a.get('severity')} {a.get('url')} {a.get('title')}")
    if blocked:
        return 1
    print(f"{args.surface}: no high or critical advisory reachable")
    return 0


if __name__ == "__main__":
    sys.exit(main())
