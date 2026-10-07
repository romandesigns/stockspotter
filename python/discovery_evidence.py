"""Extract per-symbol pipeline-stage evidence from a reduced discovery archive.

Input:  discovery-reduced-<date>.ndjson.gz  (the change-based reduction)
Output: discovery-evidence-<date>.ndjson    (one SymbolEvidence per line)

Feeds `cargo run -p backtest-metrics --bin oi_attribute`, which does the
classification. The split is deliberate: the change-based decoding and the
per-symbol forward-fill belong with the tool that produced the encoding, and
the attribution rules belong where they can be unit-tested (§19 group L).

Mechanical and outcome-neutral. No symbol is filtered, nothing is ranked, and
no value is inferred.

THREE STATES, NOT TWO
---------------------
Each stage is emitted as true / false / omitted:

  true     positively observed at this stage
  false    the stage demonstrably ran and this symbol was not in it
  omitted  no evidence either way

The rule that matters: a stage is only ever written as ``false`` when this
archive contains positive proof the stage executed. Otherwise it is omitted.
Writing ``false`` for "we didn't see it" would turn a capture gap into a
finding about the market.

WHAT THIS ARCHIVE CANNOT SAY
----------------------------
``detectorProduced`` is emitted as ``true`` or omitted, never ``false``. The
discovery capture records ignition events only -- it is not a complete
detector log -- so the absence of an ``ign`` row is not evidence that no
detector fired. Momentum, consolidation, halt and funnel output are simply
outside this archive.

``opportunityRanked`` is never emitted here at all; it comes from the shadow
log on the Rust side.
"""
import argparse
import collections
import gzip
import io
import json
import os
import sys

STAGE_VISIBLE = "visible"
STAGE_SELECTION_INPUT = "selectionInput"
STAGE_QUALIFIED = "qualified"
STAGE_QUIET = "quietSelected"
STAGE_DETECTOR = "detectorProduced"


def opener(path):
    if path.endswith(".gz"):
        return gzip.open(path, "rt", encoding="utf-8")
    return io.open(path, "r", encoding="utf-8")


def extract(src, session_date):
    symbols = {}            # interned id -> ticker
    seen_visible = set()
    seen_selection = set()
    seen_qualified = set()
    seen_quiet = set()
    nosnap_counts = collections.Counter()
    ignition_counts = collections.Counter()
    requested = set()       # ids the scan asked about (nosnap rows)

    stats = {
        "rows": 0, "unknown_kinds": collections.Counter(),
        "scan_rows": 0, "scanc_rows": 0, "snap_rows": 0,
        "sel_rows": 0, "nosnap_rows": 0, "ign_rows": 0,
        "malformed_rows": 0,
    }

    with opener(src) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            stats["rows"] += 1
            try:
                r = json.loads(line)
            except ValueError:
                stats["malformed_rows"] += 1
                continue
            k = r.get("k")

            if k == "sym":
                symbols[r["i"]] = r["s"]
            elif k == "snap":
                # The reduction emits a row whenever the observed state DIFFERS
                # from that symbol's previous observation, and a symbol's first
                # observation always differs (there is no previous). So the
                # presence of any snap row proves visibility, even though the
                # absence of one in a later scan proves nothing.
                stats["snap_rows"] += 1
                seen_visible.add(r["s"])
            elif k == "nosnap":
                stats["nosnap_rows"] += 1
                for sid in r.get("s") or []:
                    nosnap_counts[sid] += 1
                    requested.add(sid)
            elif k == "sel":
                stats["sel_rows"] += 1
                seen_selection.add(r["s"])
            elif k == "scanc":
                stats["scanc_rows"] += 1
                for sid in r.get("qual") or []:
                    seen_qualified.add(sid)
                for sid in r.get("quiet") or []:
                    seen_quiet.add(sid)
            elif k == "ign":
                stats["ign_rows"] += 1
                ignition_counts[r["s"]] += 1
            elif k == "scan":
                stats["scan_rows"] += 1
            elif k in ("meta", "seg", "dt", "stage", "ctr", "ql", "cov",
                       "rcpt", "ev", "passthrough", "stats", "malformed"):
                pass
            else:
                # Counted, never silently ignored.
                stats["unknown_kinds"][str(k)] += 1

    # Positive proof that each stage executed, which is what licenses a
    # ``false``.
    selection_ran = stats["scanc_rows"] > 0 and stats["sel_rows"] > 0
    qualification_ran = stats["scanc_rows"] > 0

    rows = []
    for sid in sorted(set(symbols) | requested):
        ticker = symbols.get(sid)
        if ticker is None:
            # An id referenced without its interning row. Reported, not
            # guessed at -- a fabricated ticker would silently corrupt a join.
            stats["unknown_kinds"]["unresolved_symbol_id"] += 1
            continue

        stages = {}
        visible = sid in seen_visible
        if visible:
            stages[STAGE_VISIBLE] = True
        elif sid in requested:
            # The scan asked and got nothing back. An observation about data
            # availability, and the reason nosnap rows exist at all.
            stages[STAGE_VISIBLE] = False

        if sid in seen_selection:
            stages[STAGE_SELECTION_INPUT] = True
        elif visible and selection_ran:
            stages[STAGE_SELECTION_INPUT] = False

        if sid in seen_qualified:
            stages[STAGE_QUALIFIED] = True
        elif visible and qualification_ran:
            stages[STAGE_QUALIFIED] = False

        if sid in seen_quiet:
            stages[STAGE_QUIET] = True
        elif visible and qualification_ran:
            stages[STAGE_QUIET] = False

        # True or omitted. Never false -- see the module docstring.
        if ignition_counts[sid]:
            stages[STAGE_DETECTOR] = True

        detector_events = {}
        if ignition_counts[sid]:
            detector_events["IgnitionDetector"] = ignition_counts[sid]

        rows.append({
            "symbol": ticker,
            "sessionDate": session_date,
            "stages": stages,
            "nosnapEvents": nosnap_counts[sid],
            "detectorEvents": detector_events,
            "rankingWindows": 0,
        })

    return rows, stats


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("source", help="discovery-reduced-<date>.ndjson[.gz]")
    ap.add_argument("--session-date", required=True, help="YYYY-MM-DD")
    ap.add_argument("--out", required=True, help="output evidence NDJSON path")
    args = ap.parse_args()

    rows, stats = extract(args.source, args.session_date)

    # Bytes and an explicit b"\n": gzip/text mode on Windows translates "\n"
    # to os.linesep, which already cost one artifact its verifiable checksum.
    with io.open(args.out, "wb") as fh:
        for row in rows:
            fh.write(json.dumps(row, separators=(",", ":"), sort_keys=True).encode("utf-8"))
            fh.write(b"\n")

    summary = dict(stats)
    summary["unknown_kinds"] = dict(stats["unknown_kinds"])
    summary["symbols_out"] = len(rows)
    summary["out_bytes"] = os.path.getsize(args.out)
    json.dump(summary, sys.stderr, indent=1, sort_keys=True)
    sys.stderr.write("\n")


if __name__ == "__main__":
    main()
