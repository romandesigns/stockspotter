#!/usr/bin/env python3
"""Final Step-4 preregistration generator and freeze manifest (Step 4B-main.1 §13-14).

PREPARED, NOT USED: nothing in the repository runs this against real values.
The final `step4-preregistration-v1.json` and its freeze manifest are created
only after GPT/user approval, from an approved-values file, by a human running
this tool.

generate   template + approved values -> preregistration artifact. Refuses any
           unresolved placeholder: capture budget, condition-table SHA,
           status-policy SHA, implementation SHA, outcome-source identity, a
           known fixture identity, any leftover PENDING/PROPOSED/PLACEHOLDER/
           TBD/FIXTURE marker, a `_fixture` key. A freeze additionally
           requires freezeStatus == "FINAL". Never overwrites.

manifest   binds the frozen preregistration to every identity it depends on,
           re-deriving each one from the artifact files (never trusting the
           caller's strings), and is the single audit entry point.

Commands:
  generate <template.json> <approved.json> <out.json> [--freeze]
  manifest <prereg.json> <conditions.json> <status-policy.json> <protocol_sha256> <out.json>
"""
import copy
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import observation_archive as archive  # noqa: E402  (RFC 8785 identity, shared with the Rust side)

REQUIRED = {
    "captureMaxBytes": ("capture.maxBytes", int),
    "tradeConditionTableSha256": ("outcome.tradeConditionTableSha256", "hex64"),
    "statusPolicySha256": ("status.policySha256", "hex64"),
    "implementationSha": ("implementationSha", "hex40"),
    "outcomeSource": ("outcome.source", str),
    "freezeStatus": ("freezeStatus", str),
}

# Identities that are known test fixtures and can never be frozen.
FIXTURE_IDENTITIES = {
    "4811c24baee8b97cdbafb4b5baf971c0db410760d2171fffc7458fc9b62a428b",  # trade-conditions.fixture.json
    "d7977ab9939b548cd3b4dbec90b36089662c727d8eb1308205cba766442b8067",  # trading-status-policy.fixture.json
    "8da15b585e1cf823da012be7d017728072e3ecaec6520ccf0bc7fb43f84b6f3d",  # v1 synthetic table (retired)
}
CAPTURE_PLACEHOLDER = 8 * 1024 ** 3
MARKERS = re.compile(r"PENDING|PROPOSED|PLACEHOLDER|TBD|FIXTURE|NOT-FINAL", re.IGNORECASE)
HEX = {"hex64": re.compile(r"^[0-9a-f]{64}$"), "hex40": re.compile(r"^[0-9a-f]{40}$")}

MANIFEST_SCHEMA = "step4-freeze-manifest-v1"


class Refusal(Exception):
    def __init__(self, problems):
        super().__init__("; ".join(problems))
        self.problems = problems


def _set(doc, path, value):
    keys = path.split(".")
    for k in keys[:-1]:
        doc = doc.setdefault(k, {})
    doc[keys[-1]] = value


def _get(doc, path):
    for k in path.split("."):
        if not isinstance(doc, dict) or k not in doc:
            return None
        doc = doc[k]
    return doc


def _walk(value, path="$"):
    if isinstance(value, dict):
        for k, v in value.items():
            yield f"{path}.{k}", k, None
            yield from _walk(v, f"{path}.{k}")
    elif isinstance(value, list):
        for i, v in enumerate(value):
            yield from _walk(v, f"{path}[{i}]")
    else:
        yield path, None, value


def _trivial_hex(s):
    return len(set(s)) == 1


def build(template, approved, freeze):
    """Returns the artifact dict, or raises Refusal listing every problem."""
    problems = []
    doc = copy.deepcopy(template)
    doc.pop("_fixture", None)
    for key, (path, kind) in REQUIRED.items():
        if key not in approved:
            problems.append(f"unresolved: {key} ({path}) has no approved value")
            continue
        v = approved[key]
        if kind is int and (not isinstance(v, int) or isinstance(v, bool) or v <= 0):
            problems.append(f"{key}: not a positive integer")
        elif kind is str and (not isinstance(v, str) or not v.strip()):
            problems.append(f"{key}: empty")
        elif kind in HEX and (not isinstance(v, str) or not HEX[kind].match(v)):
            problems.append(f"{key}: not lowercase {kind}")
        elif kind in HEX and (_trivial_hex(v) or v in FIXTURE_IDENTITIES):
            problems.append(f"{key}: placeholder or fixture identity {v}")
        _set(doc, path, v)
    if approved.get("captureMaxBytes") == CAPTURE_PLACEHOLDER and not approved.get("captureMaxBytesConfirmed"):
        problems.append("captureMaxBytes: equals the unadopted 8 GiB placeholder (set captureMaxBytesConfirmed to adopt it deliberately)")
    for path, value in (approved.get("overrides") or {}).items():
        if _get(doc, path) is None:
            problems.append(f"override {path}: no such field in the template")
        _set(doc, path, value)
    if freeze and doc.get("freezeStatus") != "FINAL":
        problems.append(f"freezeStatus is {doc.get('freezeStatus')!r}; a freeze requires FINAL")
    for path, key, value in _walk(doc):
        if key is not None and key.startswith("_"):
            problems.append(f"{path}: annotation key left in the artifact")
        if isinstance(value, str) and MARKERS.search(value):
            problems.append(f"{path}: unresolved marker in {value[:60]!r}")
    try:
        archive.canonical_bytes(doc)
    except ValueError as e:
        problems.append(str(e))
    if problems:
        raise Refusal(problems)
    return doc


def _write_new(path, data):
    with open(path, "x", encoding="utf-8", newline="\n") as f:  # never overwrite
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def generate(template_path, approved_path, out_path, freeze=False):
    with open(template_path, encoding="utf-8") as f:
        template = json.load(f)
    with open(approved_path, encoding="utf-8") as f:
        approved = json.load(f)
    doc = build(template, approved, freeze)
    _write_new(out_path, json.dumps(doc, indent=2, ensure_ascii=False) + "\n")
    return archive.prereg_sha(out_path)


def manifest(prereg_path, conditions_path, status_path, protocol_sha, out_path):
    """Builds the freeze manifest; every identity is re-derived from files."""
    with open(prereg_path, encoding="utf-8") as f:
        pre = json.load(f)
    problems = []
    if pre.get("freezeStatus") != "FINAL":
        problems.append("the preregistration is not FINAL")
    if not HEX["hex64"].match(protocol_sha or ""):
        problems.append("protocol SHA is not hex64")
    cond_sha, status_sha = archive.prereg_sha(conditions_path), archive.prereg_sha(status_path)
    # The bound tables must themselves be frozen: a PROPOSED table bound into
    # a FINAL manifest would freeze a classification nobody froze.
    for label, path in (("condition table", conditions_path), ("status policy", status_path)):
        with open(path, encoding="utf-8") as f:
            table = json.load(f)
        if table.get("freezeStatus") != "FINAL":
            problems.append(f"{label} freezeStatus is {table.get('freezeStatus')!r}, not FINAL")
        for tpath, key, value in _walk(table):
            if key is not None and key.startswith("_"):
                problems.append(f"{label} {tpath}: annotation key")
    if cond_sha != _get(pre, "outcome.tradeConditionTableSha256"):
        problems.append("condition table does not match the preregistered identity")
    if status_sha != _get(pre, "status.policySha256"):
        problems.append("status policy does not match the preregistered identity")
    try:
        build(pre, {k: _get(pre, p) for k, (p, _) in REQUIRED.items()} | {"captureMaxBytesConfirmed": True}, True)
    except Refusal as r:
        problems += ["preregistration: " + p for p in r.problems]
    if problems:
        raise Refusal(problems)
    info = pre["informativeness"]
    doc = {
        "schema": MANIFEST_SCHEMA,
        "preregistrationSha256": archive.prereg_sha(prereg_path),
        "implementationSha": pre["implementationSha"],
        "protocolVersion": pre["protocolVersion"],
        "protocolSha256": protocol_sha,
        "gateSha256": pre["gateSha256"],
        "tradeConditionTableSha256": cond_sha,
        "statusPolicySha256": status_sha,
        "outcomeFetchContract": pre["outcome"]["fetchContract"],
        "outcomeSource": pre["outcome"]["source"],
        "certificateSemantics": pre["certificate"]["semantics"],
        "campaignRules": {
            "maxDesignatedSessions": info["calendarCapSessions"],
            "qualifyingSessionsTarget": info["qualifyingSessions"],
            "minDiscriminatingWindowsPerSession": info["discriminatingWindowsPerSession"],
        },
        "artifacts": {
            os.path.basename(p): archive.prereg_sha(p) for p in (prereg_path, conditions_path, status_path)
        },
    }
    _write_new(out_path, json.dumps(doc, indent=2, sort_keys=True) + "\n")
    return archive.prereg_sha(out_path)


def main(argv):
    try:
        if len(argv) in (5, 6) and argv[1] == "generate" and (len(argv) == 5 or argv[5] == "--freeze"):
            print(generate(argv[2], argv[3], argv[4], freeze=len(argv) == 6))
            return 0
        if len(argv) == 7 and argv[1] == "manifest":
            print(manifest(*argv[2:7]))
            return 0
    except Refusal as r:
        print(json.dumps({"refused": r.problems}, indent=1))
        return 1
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
