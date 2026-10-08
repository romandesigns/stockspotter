#!/usr/bin/env python3
"""Advisory gate for the chart lifecycle harness's own tooling.

tools/chart-recovery has a package.json and bun.lock of its own (one package,
playwright-core) that is deliberately not a workspace of the repository root,
so the root lockfile -- and with it `js_advisory_gate.py --surface server` and
`--surface mobile` -- never sees it. Keeping test tooling out of the release
surfaces must not also keep it out of the advisory audit.

This is NOT a second gate. It runs js_advisory_gate.main() itself -- the same
strict parsing, the same refusal of an audit that failed, printed nothing or
printed diagnostics, the same lockfile closure, the same inventory of what is
physically installed -- against that directory's lockfile and node_modules.
The one thing it supplies is the surface map: that lockfile has exactly one
workspace (its root), where the repository lockfile has four, and the gate
refuses any lockfile whose workspaces do not match its map exactly. The map
is swapped for the duration of the call and restored afterwards; nothing here
can change what the server or mobile surface decides.

It lives in its own file because js_advisory_gate.py is part of the mobile
surface's change filter (validate.yml, `mobile_scope`), and the surface map
for this tooling is not a mobile concern.

Run after `bun install --frozen-lockfile --cwd tools/chart-recovery`:
usage: chart_harness_advisory_gate.py [--audit-json FILE]
Exit 0 = no high or critical advisory reachable or installed, 1 = blocked or
cannot decide. `--audit-json` exists for the self-tests; CI never passes it.
"""
import argparse
import os
import pathlib
import sys

import js_advisory_gate as gate

REPO = pathlib.Path(__file__).resolve().parents[2]
HARNESS_DIR = REPO / "tools" / "chart-recovery"
SURFACE = "chart-harness"
# The harness lockfile's only workspace is its own root.
SURFACES = {SURFACE: [""]}


def main(argv=None, harness_dir=HARNESS_DIR):
    ap = argparse.ArgumentParser()
    ap.add_argument("--audit-json")
    args = ap.parse_args(argv)
    gate_args = ["--surface", SURFACE, "--lock", "bun.lock", "--installed-root", "."]
    if args.audit_json:
        gate_args += ["--audit-json", os.path.abspath(args.audit_json)]
    previous_surfaces, previous_cwd = gate.SURFACES, os.getcwd()
    try:
        # `bun audit` reads the lockfile of the directory it runs in.
        os.chdir(harness_dir)
        gate.SURFACES = SURFACES
        return gate.main(gate_args)
    except OSError as e:
        print(f"CANNOT DECIDE ({SURFACE}): {e}")
        return 1
    finally:
        gate.SURFACES = previous_surfaces
        os.chdir(previous_cwd)


if __name__ == "__main__":
    sys.exit(main())
