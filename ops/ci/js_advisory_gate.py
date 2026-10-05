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
  * an audit that failed, printed nothing, or printed anything but a
    well-formed advisory response (see `interpret_audit`).

Advisories are matched by package NAME, not version: if a surface reaches any
copy of an advised package, it is blocked. That can over-block, never
under-block.

With `--installed-root`, the gate also blocks on any advised package that is
physically installed in the calling checkout. The server CI jobs and the web
image builder install only the server workspaces
(`bun install --filter ...`), so this is the check that the declared boundary
is the real one: build time and gate time see the same tree.

usage: js_advisory_gate.py --surface {server,mobile} [--lock bun.lock] [--audit-json FILE] [--installed-root DIR]
Exit 0 = surface clean, 1 = blocked or cannot decide.
"""
import argparse
import json
import os
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


def strict_json(text, what):
    """JSON with no room for interpretation, or CannotDecide.

    Python's parser accepts a repeated member name and silently keeps the last
    value, so `{"braces": [high], "braces": [moderate]}` would hide the high
    advisory. Every object, at every depth, is therefore rejected if a name
    repeats. NaN/Infinity (not JSON) are rejected too.
    """
    def pairs(items):
        seen = set()
        for key, _ in items:
            if key in seen:
                raise CannotDecide(f"{what} repeats the member name {key!r}; refusing to guess which value counts")
            seen.add(key)
        return dict(items)

    def constant(name):
        raise CannotDecide(f"{what} contains {name}, which is not JSON")

    try:
        return json.loads(text, object_pairs_hook=pairs, parse_constant=constant)
    except ValueError:
        raise CannotDecide(f"{what} is not JSON: {text.strip()[:200]}")


def load_lock(path):
    with open(path, encoding="utf-8") as f:
        text = f.read()
    # bun.lock is JSON with trailing commas.
    return strict_json(re.sub(r",(\s*[}\]])", r"\1", text), "bun.lock")


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


WORKSPACE_MARK = "@workspace:"


def closure(lock, workspace):
    """Every lock entry `workspace` can reach, transitively.

    Edge rules, each failing closed:
      * `dependencies` (and a workspace's `devDependencies`) must resolve;
      * `peerDependencies` must resolve too -- Bun installs peers -- unless
        the lock marks that peer optional (`optionalPeers`);
      * `optionalDependencies` may be absent (platform-specific packages);
      * a dependency on another workspace is followed INTO that workspace:
        whatever it depends on is reachable from here as well, whichever
        surface that workspace is assigned to. A shared workspace therefore
        appears in the closure of every surface that uses it.
    """
    packages, workspaces = lock["packages"], lock["workspaces"]
    seen, visited_ws, stack = set(), set(), []

    def push(owner, chain, deps, required, optional=()):
        for name in deps:
            key = resolve(packages, chain, name)
            if key is None:
                if required and name not in optional:
                    raise CannotDecide(f"dependency {name!r} of {owner!r} is not in the lockfile")
                continue
            stack.append(key)

    def enter(path):
        if path in visited_ws:
            return
        if path not in workspaces:
            raise CannotDecide(f"workspace {path!r} is referenced but not in the lockfile")
        visited_ws.add(path)
        ws = workspaces[path]
        chain = [ws["name"]] if path else []
        owner = ws.get("name") or "the root workspace"
        push(owner, chain, ws.get("dependencies", {}), True)
        push(owner, chain, ws.get("devDependencies", {}), True)
        push(owner, chain, ws.get("optionalDependencies", {}), False)
        push(owner, chain, ws.get("peerDependencies", {}), True, set(ws.get("optionalPeers", [])))

    enter(workspace)
    while stack:
        key = stack.pop()
        if key in seen:
            continue
        seen.add(key)
        spec = packages[key][0]
        if WORKSPACE_MARK in spec:
            enter(spec.split(WORKSPACE_MARK, 1)[1])
            continue
        meta = meta_of(packages[key])
        chain = split_key(key)
        push(key, chain, meta.get("dependencies", {}), True)
        push(key, chain, meta.get("optionalDependencies", {}), False)
        push(key, chain, meta.get("peerDependencies", {}), True, set(meta.get("optionalPeers", [])))
    return seen


def names(lock, keys):
    """Every name an advisory could use for these lock entries.

    Both the name the entry is installed under (the last segment of its key)
    and the package it really is (`<name>@<version>` in the entry): an npm
    alias such as `"string-width-cjs": ["string-width@4.2.3", ...]` must be
    matched by an advisory against `string-width`.
    """
    out = set()
    for key in keys:
        out.add(split_key(key)[-1])
        spec = lock["packages"][key][0]
        real = spec[: spec.rfind("@")] if spec.rfind("@") > 0 else spec
        out.add(real)
    return out


def blocking_advisories(audit):
    out = {}
    validate_audit(audit)
    for package, advisories in audit.items():
        hits = [a for a in advisories if str(a.get("severity", "")).lower() in BLOCKING]
        if hits:
            out[package] = hits
    return out


def decide(lock, audit, surface, installed=None):
    """Blocks `surface` on any advised package it can reach OR that is installed here.

    `installed` (from `--installed-root`) is what the calling environment
    actually has on disk. The server jobs install only the server workspaces,
    so an advised package turning up there means the boundary leaked, and that
    blocks regardless of what the lockfile closure says.
    """
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
    present = installed or set()
    blocked = {p: a for p, a in advised.items() if p in reach[surface] or p in present}
    elsewhere = {p: sorted(s for s, r in reach.items() if p in r) for p in advised if p not in blocked}
    return blocked, elsewhere


SEVERITIES = {"info", "low", "moderate", "high", "critical"}


def interpret_audit(returncode, stdout, stderr):
    """Turns one `bun audit --json` invocation into advisories, or refuses.

    Bun exits 1 both when it found advisories and when the registry request
    failed, so the exit status alone decides nothing. Observed (bun 1.4.0):
    findings -> exit 1, JSON on stdout, empty stderr; clean -> exit 0, `{}`;
    registry unreachable -> exit 1, EMPTY stdout, `error: ...` on stderr.
    Anything that is not exactly one of the first two shapes is refused: a
    failed audit must never read as a clean one.

    Both accepted shapes have an EMPTY stderr, so any stderr at all refuses.
    There is deliberately no list of failure wordings to match and no list of
    benign ones to allow: a diagnostic this gate has never seen is exactly the
    case it must not guess about. If a future Bun prints something harmless
    there, the gate fails loudly and this function is updated on purpose.
    """
    diagnostics = (stderr or "").strip()
    if returncode not in (0, 1):
        raise CannotDecide(f"bun audit exited {returncode}: {diagnostics[:300]}")
    if diagnostics:
        raise CannotDecide(f"bun audit printed diagnostics (exit {returncode}), so its result is not trusted: {diagnostics[:300]}")
    if not (stdout or "").strip():
        raise CannotDecide(f"bun audit produced no output (exit {returncode}): {diagnostics[:300] or 'no diagnostics'}")
    audit = strict_json(stdout, f"bun audit output (exit {returncode})")
    validate_audit(audit)
    if returncode == 0 and audit:
        raise CannotDecide("bun audit exited 0 but listed advisories")
    if returncode == 1 and not audit:
        raise CannotDecide("bun audit exited 1 without listing any advisory")
    return audit


def validate_audit(audit):
    """`{package: [{id, url, severity, ...}, ...]}` and nothing else."""
    if not isinstance(audit, dict):
        raise CannotDecide("audit response is not an object")
    for package, advisories in audit.items():
        if not isinstance(package, str) or not package or not isinstance(advisories, list) or not advisories:
            raise CannotDecide(f"audit response has an unexpected entry for {package!r}")
        for a in advisories:
            if (not isinstance(a, dict) or str(a.get("severity", "")).lower() not in SEVERITIES
                    or not isinstance(a.get("url"), str) or "id" not in a):
                raise CannotDecide(f"audit response has an unrecognised advisory for {package!r}")


def run_audit():
    try:
        proc = subprocess.run(["bun", "audit", "--json"], capture_output=True, text=True, timeout=300)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise CannotDecide(f"bun audit could not be run: {e}")
    return interpret_audit(proc.returncode, proc.stdout, proc.stderr)


def installed_packages(root):
    """Package names physically present under `root`'s node_modules trees.

    Ground truth for "what this environment installed", independent of the
    lockfile resolver. Supported layouts, and nothing else:
      * Bun's isolated store, `node_modules/.bun/<name>@<version>...` (scopes
        written `@scope+name`), read from the store's directory names;
      * ordinary `node_modules/<name>` and `node_modules/@scope/<name>`
        directories, at any depth, anywhere under `root`.

    Links (symlinks, and directory junctions on Windows) are how Bun wires
    packages together, so they are expected -- but only INSIDE the checkout,
    where the walk already sees whatever they point at. A link whose target is
    outside `root` is something this scan cannot inventory, so it refuses
    rather than report a tree it did not read. The walk prunes nothing and
    refuses on any directory it cannot read, so an in-checkout target is
    always inventoried at its real location. Links are never followed, so a
    link cycle cannot loop.
    """
    found = set()
    real_root = os.path.realpath(root)
    top = os.path.join(root, "node_modules")
    if not os.path.isdir(top):
        raise CannotDecide(f"{top} does not exist: nothing is installed to check")

    def is_link(path):
        if os.path.islink(path):
            return True
        # Junctions (Windows) are not symlinks to Python; compare the resolved
        # path with where the entry would be if it were an ordinary directory.
        plain = os.path.join(os.path.realpath(os.path.dirname(path)), os.path.basename(path))
        return os.path.normcase(os.path.realpath(path)) != os.path.normcase(plain)

    def inside(path):
        target = os.path.realpath(path)
        try:
            return os.path.commonpath([real_root, target]) == real_root
        except ValueError:  # different drive
            return False

    def check(path):
        """True if `path` is a link (never descended); refuses if it leaves the checkout."""
        if not is_link(path):
            return False
        if not inside(path):
            raise CannotDecide(f"{path} links outside the checkout ({os.path.realpath(path)}); the installed tree cannot be inventoried")
        return True

    def unreadable(error):
        raise CannotDecide(f"cannot read {getattr(error, 'filename', None) or error}: the installed tree cannot be inventoried")

    def listing(path):
        try:
            return os.listdir(path)
        except OSError as e:
            unreadable(e)

    # Nothing is pruned -- not even `.git`. An in-checkout link is accepted
    # precisely because the walk reads its target at its real location, so
    # there must be no location the walk refuses to read; an unreadable
    # directory is a refusal, not a silent gap.
    for base, dirs, files in os.walk(root, followlinks=False, onerror=unreadable):
        in_modules = os.path.basename(base) == "node_modules"
        if in_modules:
            for f in files:
                # A link the walk filed under "files" does not resolve to a
                # directory: dangling, or pointing at something unreadable.
                if os.path.islink(os.path.join(base, f)):
                    raise CannotDecide(f"{os.path.join(base, f)} is a link that does not resolve to a readable directory")
        keep = []
        for d in dirs:
            path = os.path.join(base, d)
            linked = check(path)
            if os.path.basename(base) == ".bun" and os.path.basename(os.path.dirname(base)) == "node_modules":
                # A store entry: `<name>@<version>[+hash]`.
                name = d.replace("+", "/", 1) if d.startswith("@") else d
                cut = name.rfind("@")
                if cut > 0:
                    found.add(name[:cut])
            elif in_modules and d.startswith("@"):
                for x in listing(path):
                    check(os.path.join(path, x))
                    found.add(f"{d}/{x}")
            elif in_modules and not d.startswith("."):
                found.add(d)
            if not linked:
                keep.append(d)
        dirs[:] = keep
    if not found:
        raise CannotDecide(f"no installed package found under {root}: refusing to treat an empty inventory as a clean one")
    return found


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--surface", required=True, choices=sorted(SURFACES))
    ap.add_argument("--lock", default="bun.lock")
    ap.add_argument("--audit-json")
    ap.add_argument("--installed-root", help="also block on any advised package installed under this checkout")
    args = ap.parse_args(argv)
    try:
        lock = load_lock(args.lock)
        if args.audit_json:
            with open(args.audit_json, encoding="utf-8") as f:
                audit = strict_json(f.read(), "saved audit")
            validate_audit(audit)
        else:
            audit = run_audit()
        installed = installed_packages(args.installed_root) if args.installed_root else None
        blocked, elsewhere = decide(lock, audit, args.surface, installed)
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
    scope = "reachable or installed" if args.installed_root else "reachable"
    print(f"{args.surface}: no high or critical advisory {scope}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
