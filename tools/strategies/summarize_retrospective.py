"""POST-HOC descriptive stratification; no detector tuning or inference.

Control totals must reproduce the original report before accepting a supplement.
"""
from collections import Counter, defaultdict
import json
import math
from pathlib import Path
import sys
from validate_captured import outcome, sha, split


def summarize(directory):
    directory = Path(directory).resolve(strict=True)
    result = json.loads((directory / "results.json").read_text())
    plan = json.loads((directory / "plan.json").read_text())
    for file, key in (("bars.ndjson", "normalizedInputSha256"), ("replay.ndjson", "replaySha256")):
        if sha(directory / file) != result[key]:
            raise ValueError("validated input identity mismatch")
    sessions = defaultdict(list)
    with (directory / "bars.ndjson").open() as file:
        for line in file:
            row = json.loads(line)
            sessions[row["sessionId"]].append(row["bar"])
    candidates = defaultdict(set)
    events = defaultdict(Counter)
    with (directory / "replay.ndjson").open() as file:
        for line in file:
            row = json.loads(line)
            if row["type"] != "experimental_strategy_update":
                continue
            session = f'{row["symbol"]}/{row["marketDay"]}'
            events[(session,row["strategy"])][row["kind"]] += 1
            if row["kind"] == "candidate":
                candidates[(session,row["strategy"])].add(row["eventTimeMs"])
    cells = defaultdict(Counter)
    with (directory / "outcomes.ndjson").open() as file:
        for line in file:
            row = json.loads(line)
            key = (row["sessionId"],row["strategy"])
            cells[key][f'{row["stage"]}_{row["classification"]}'] += 1
            if row["classification"] != "censored":
                cells[key][f'{row["stage"]}_drawdown_2pct'] += row["minExcursionPct"] <= -2
                cells[key][f'{row["stage"]}_reach_and_drawdown_2pct'] += row["classification"] == "reach" and row["minExcursionPct"] <= -2
    config = plan["config"]
    for session,bars in sessions.items():
        contiguous = 0
        previous = None
        for index,bar in enumerate(bars):
            if previous is not None and bar["marketOpenMs"] - previous != 60_000:
                contiguous = 0
            valid = all(math.isfinite(bar[field]) and bar[field] > 0 for field in ("open","high","low","close"))
            valid = valid and bar["high"] >= max(bar["open"],bar["close"]) and bar["low"] <= min(bar["open"],bar["close"])
            if valid:
                for strategy,window in (("iac",config["compressionBars"]),("extreme_momentum",config["extremeBars"]-1)):
                    needed = config["baselineBars"] + window
                    anchor = bar["marketOpenMs"] + 60_000
                    baseline = bars[index-needed:index-window] if contiguous >= needed else []
                    if baseline and sum(b["high"]-b["low"] for b in baseline) > 0 and sum(b["volume"] for b in baseline) > 0:
                        if anchor not in candidates[(session,strategy)]:
                            observed = outcome(bars,dict(eventTimeMs=anchor,observedPrice=bar["close"]))
                            cells[(session,strategy)][f'control_{observed["classification"]}'] += 1
                contiguous += 1
            else:
                contiguous = 0
            previous = bar["marketOpenMs"]
    groups = defaultdict(Counter)
    symbol_days = defaultdict(list)
    for (session,strategy),cell in cells.items():
        day=session.split('/')[1]
        group=f"{split(day)}/{strategy}"
        groups[group].update(cell)
        if events[(session,strategy)]["candidate"]:
            symbol_days[group].append((session,cell,events[(session,strategy)]["candidate"]))
    for key,expected in result["groups"].items():
        for field in ("candidate_reach","candidate_no_reach","candidate_censored","held_reach","held_no_reach","held_censored","control_reach","control_no_reach","control_censored"):
            if groups[key][field] != expected.get(field,0):
                raise ValueError(f"supplement failed to reproduce {key}/{field}")
    summary = {}
    for group,rows in symbol_days.items():
        counts=groups[group]
        total=counts["candidate_reach"]+counts["candidate_no_reach"]+counts["candidate_censored"]
        weighted_control=0
        matched=0
        by_symbol=Counter()
        by_day=Counter()
        for session,cell,candidate_count in rows:
            symbol,day=session.split('/')
            by_symbol[symbol]+=candidate_count;by_day[day]+=candidate_count
            n=cell["candidate_reach"]+cell["candidate_no_reach"]
            nc=cell["control_reach"]+cell["control_no_reach"]
            if nc:
                weighted_control+=n*cell["control_reach"]/nc
                matched+=n
        strategy=group.split('/')[1]
        summary[group]=dict(candidateSymbolDays=len(rows),candidateMarketDays=len(by_day),bySymbol=dict(by_symbol),byMarketDay=dict(by_day),
            candidateReachMissingnessBounds=[counts["candidate_reach"]/total,(counts["candidate_reach"]+counts["candidate_censored"])/total],
            candidateWeightedSameSymbolDayControlRate=weighted_control/matched if matched>=20 else None,
            matchedEvaluableCandidates=matched,
            candidateReachAndDrawdown2Pct=counts["candidate_reach_and_drawdown_2pct"],
            candidateAnyDrawdown2Pct=counts["candidate_drawdown_2pct"],
            pendingAtFileEnd=sum(events[(session,strategy)]["candidate"]-sum(events[(session,strategy)][kind] for kind in ("continued","failed","expired","invalidated")) for session,_,_ in rows))
    output=dict(schema="iac-extreme-posthoc-stratification-v1",postHoc=True,
        originalResultsSha256=sha(directory / "results.json"),outcomeFileSha256=sha(directory / "outcomes.ndjson"),
        scriptSha256=sha(Path(__file__)),evaluatorSha256=sha(Path(__file__).with_name("validate_captured.py")),
        groups=summary,perSession={f"{session}/{strategy}":dict(cell) for (session,strategy),cell in sorted(cells.items())},
        limitations=["post-hoc descriptive reporting requested after independent critique; not a preregistered effect estimate",
            "same-symbol-day weighting does not match time of day, volatility, liquidity or survivorship",
            "no independent observations, confidence intervals, significance, profitability or live causality claim",
            "reach and drawdown may occur in either order or the same minute; bar data do not establish execution",
            "detector/config/original evaluator and original result files unchanged"])
    destination=directory / "posthoc-stratified-summary.json"
    if destination.exists():raise ValueError("supplement already exists; refusing overwrite")
    destination.write_text(json.dumps(output,indent=2)+"\n",encoding="utf-8")
    print(json.dumps(summary,indent=2))


if __name__ == "__main__":summarize(sys.argv[1])
