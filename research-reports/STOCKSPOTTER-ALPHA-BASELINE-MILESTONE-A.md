# STOCKSPOTTER ALPHA BASELINE — MILESTONE A

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909` @ `0544790`
**Scope:** investigation and measurement design only. No thresholds tuned, no strategy or
auto-trader logic modified, no code committed, no deployment, no production access.

**Evidence labelling used throughout — this distinction is load-bearing:**

- **[MEASURED]** — observed directly, this session or in a committed artifact, with a stated source.
- **[INFERRED]** — reasoned from code that I read but did not execute against data.
- **[NOT YET MEASURABLE]** — cannot be established with what exists locally today.

---

## 1. Executive conclusion

**Stockspotter's alpha cannot currently be measured, and that is the finding — not an obstacle to
reporting one.**

Three structural facts dominate everything below:

1. **There is no local empirical data. None.** [MEASURED] No `data/` directory, zero `.jsonl` files,
   zero fixtures, zero cached bars anywhere in the working tree. Every journal, pending-signal log,
   evaluated-signal log and discovery capture exists only inside the gitignored `/data/` bind-mount
   on the VPS, which this milestone forbids accessing. **Every quantitative question in the brief —
   forward returns, precision, MFE/MAE, earliness, detector interaction — is therefore
   [NOT YET MEASURABLE] today.** I will not manufacture numbers to fill those sections.

2. **Stockspotter does not rank signals.** [MEASURED] This is the single most consequential
   architectural finding. The brief asks whether higher-ranked candidates outperform lower-ranked
   ones, calling it "crucial". They cannot, because no such ranking exists. The visible "Top
   Gainers / Highly Trading" leaderboards rank by **`gap_pct` and session volume** — realized past
   move, not predicted future quality — and `movers.rs` states in its own header that these are
   *"rankings, not detection gates, so nothing here reads or feeds funnel"*. Signals are emitted as
   an unordered stream. Ranking quality is not merely unmeasured; **the mechanism is absent.**

3. **The system already knows it cannot claim recall, and says so in writing.** [MEASURED] A
   purpose-built recall recorder (`discovery_audit.rs` + `python/analyze_discovery.py`) exists, was
   run against the real market on 2026-09-07, and returned **`insufficient_evidence` with
   `whole_market_recall` explicitly `null`**. That is unusually honest engineering, and it means the
   precision-vs-recall question — *are we missing runners, or drowning in false positives?* — is
   currently unanswerable **by design of the evidence, not by oversight.**

**What can be said today:** the pipeline is coherent, the outcome model is transparent and largely
free of lookahead by construction, and the measurement scaffolding is better than the audit
suggested. **What cannot be said:** anything at all about whether Stockspotter finds exceptional
opportunities early, ranks them usefully, or converts them into value.

The correct next step is **not** strategy work. It is a market-session data capture, and a change to
what gets recorded at signal time.

---

## 2. The actual pipeline

Traced from code, not from crate names. [MEASURED — code reading]

```
Alpaca SIP feed (wss, full-market trades + statuses)
   │
   ├─► universe scan (market-data/universe.rs)
   │     ~12.9k symbols; rescan every 15s (UNIVERSE_RESCAN_INTERVAL)
   │
   ├─► TIERING (market-data/live.rs) — four bounded tiers, not one watchlist:
   │     • funnel qualifiers      (fast-funnel pass)
   │     • movers leaderboard     (top-N by gap%/volume)
   │     • quiet watch            (QUIET_TOTAL_CAP = 300, 300s transition grace)
   │     • universe tier          (IGNITION_UNIVERSE_MODE=1 in prod;
   │                               UNIVERSE_MAX_MONITORS = 6_000,
   │                               evict after UNIVERSE_MONITOR_IDLE_SECS = 300)
   │
   ├─► DETECTORS, per monitored symbol
   │     fast-funnel · ignition-detector · momentum-scorer
   │     consolidation-breakout · halt-detector
   │
   ├─► ScanEvent emitted (8 variants — §3)
   │
   ├─► broadcast (ws-server) ──► EventSnapshot retained ──► WS fanout to clients
   │        │
   │        ├─► live_signals::LiveSignalTracker  → data/live_pending_signals.jsonl
   │        │      (edge-triggered; later evaluated by bin/live_efficiency
   │        │       → data/live_evaluated_signals.jsonl)
   │        │
   │        └─► push notifier (ignition FollowThroughConfirmed only, per-symbol cooldown)
   │
   ├─► auto-trader (separate WS client, same broadcast)
   │     entry on 3 event kinds → paper order → journal
   │
   └─► qualify service (Python) — news catalyst tagging → catalyst_update
```

**Where the brief's assumed pipeline differs from reality:**

- **"ranking/prioritization" does not exist as a stage.** Nothing orders signals by expected value.
- **Qualification is not a gate.** The Python `qualify` layer emits `catalyst_update` events —
  informational tags consumed by a UI panel. It does not filter, block or score candidates.
- **Client visibility is not selective.** Every client receives the identical broadcast; there is no
  per-client filtering (`server.rs` states this as an explicit guarantee).

---

## 3. Detector and strategy inventory

[MEASURED — code reading]. Roles determined from consumers, not names.

| Component | Real role | Key thresholds (defaults) | Feeds auto-trader? | Outcome measured later? |
|---|---|---|---|---|
| **fast-funnel** | Candidate generator | price 0.25–20.0, rel-vol ≥ 5.0, gap ≥ 10% | No | Yes (edge-triggered signal) |
| **ignition-detector** | Candidate + confirmation | gate ≤ $0.25, lookback 20 trades, range ≤ 3%, trade-freq spike 3.0×, spread tighten 0.5, ask absorption 0.6 | **Yes** (`FollowThroughConfirmed` only) | Yes |
| **momentum-scorer** | **Gate + informational**, not ranking | 4 weighted factors → `overall`; qualify ≥ 0.60 | **Yes**, as a gate (`MOMENTUM_CONFIRM_THRESHOLD = 0.6`) and an exit trigger (`MOMENTUM_DETERIORATION_THRESHOLD = 0.4`) | Yes |
| **consolidation-breakout** | Candidate + trade trigger | surge ≥ 8% move, vol ratio ≥ 3.0, consolidation 2–20 candles, range ≤ 0.6 of surge | **Yes** (`EntryTriggered`) | Yes |
| **halt-detector** | **Warning only** | amber 0.5 / red 0.8 proximity, rel-vol 3.0, hysteresis 0.10 | Indirectly — `HaltRiskTooHigh` is a *skip* reason | No |
| **movers** | Ranking — **of realized past move** | top-N leaderboard, rolling best-observed | No | No |
| **qualify (Python)** | Informational tag | news catalyst categories | No | No |

**Critical role corrections (names mislead):**

- **momentum-scorer is not a ranker.** Despite producing a continuous 0–1 score — the one component
  that *could* rank — it is used only as a **binary gate** at 0.6 and an exit trigger at 0.4. The
  ordering information in that score is computed and then discarded.
- **halt-detector is not a trade signal.** It only ever *blocks*.
- **movers ranks the past, not the future.** It is the only ranked surface in the product, and it
  ranks by what has already happened.

**[MEASURED — from a code comment documenting a real session]** momentum `overall` had a **median of
0.46** across a full real trading session, against a 0.60 qualify threshold. So the gate rejects
somewhat more than half of scored observations. Sample size and date not recorded in the comment —
this is a documented observation, not a reproducible measurement.

**Deduplication / cooldown** exists at three distinct layers, each with different semantics:
- Push: per-symbol `IGNITION_PUSH_COOLDOWN` quiet window.
- Signal logging: **edge-triggering** — only the *flip* into a qualifying state counts.
- Trading: `AlreadyEnteredToday` — one entry per symbol per day.

---

## 4. Ranking architecture

**There isn't one for signals.** [MEASURED]

The only ranked artifacts are `TodayMovers` gainers/most-active leaderboards, which:
- rank by `gap_pct` / session volume (realized move),
- retain a **rolling best-observed** value per symbol rather than the current snapshot,
- explicitly do not feed detection (`movers.rs` header),
- keep top-N only.

One consequence worth flagging: the rolling-best design means *"a stock that gapped hard at 5am
premarket and has since faded can keep out-ranking a newer, still-accumulating one until it ages
out"* — the module's own words. For a leaderboard that is a defensible choice; if this were ever
used as a candidate-quality ranking it would be actively misleading.

**Implication for the brief's §6 "ranking quality" request:** top-1/3/5/10-vs-remainder analysis is
**[NOT YET MEASURABLE]**, and would remain so even with full production data, because no ranked
signal list is ever produced or recorded.

---

## 5. Outcome / evaluation architecture

[MEASURED — `crates/backtest-metrics/src/outcome.rs`, `live_signals.rs`, `signals.rs`]

**Model:** a signal is a *hit* if price gains `target_pct` before losing `stop_pct`, within
`lookforward_bars` bars.

| Profile | target | stop | lookforward | Applied to |
|---|---|---|---|---|
| `default()` ("swing") | +5.0% | −3.0% | 20 bars | funnel, momentum |
| "scalp" | tuned 2026-08-30 against real SWVL data | — | — | ignition |

**Recorded per signal:** `hit`, `max_favorable_pct` (MFE), `bars_to_target`, `kind`
(`Hit` / `StoppedOut` / `TimedOut` / `Unknown`), `final_pct`.

**Strengths, genuinely:**
- **Live and backtest share one definition.** `live_signals.rs` is deliberately kept in sync with
  `signals.rs` so a live hit-rate and a backtest hit-rate are comparable rather than being two
  different notions of "a signal fired".
- **Edge-triggering is explicit and consistent.** Only state *flips* count; `SurgeDetected` and
  `ConsolidationConfirmed` are diagnostic-only.
- **`final_pct` was added specifically so expectancy uses real loss/timeout magnitudes** rather than
  assuming every non-hit cost exactly `stop_pct`.

**Gaps in the outcome model — these matter for the brief's metric list:**

| Brief asks for | Status |
|---|---|
| Forward return at 30s/1/3/5/10/15/30 min | **Absent.** Only a single target/stop resolution and MFE. No horizon grid. |
| **MAE (max adverse excursion)** | **Absent.** Only `max_favorable_pct` is recorded — confirmed by code comment: *"`max_favorable_pct` but no max-adverse figure"*. |
| Time to +2% / +5% / +10% | Partial: `bars_to_target` for the one configured target only. |
| Time to MFE / drawdown before MFE | **Absent.** |
| % of eventual move completed before first detection | **Absent** — requires pre-detection price history that is not retained. |
| Executable vs headline price | `quote_execution.rs` exists (294 lines) — a quote-based execution model is implemented, so this is **partially available** for backtests. |

---

## 6. Available empirical datasets

**[MEASURED] Locally: nothing.**

| Dataset | Location | Present locally? |
|---|---|---|
| `auto_trader_journal.jsonl` | `data/` (VPS mount) | **No** |
| `live_pending_signals.jsonl` | `data/` | **No** |
| `live_evaluated_signals.jsonl` | `data/` | **No** |
| `backtest_log.jsonl` | `data/` | **No** |
| `alpaca_paper_ledger.jsonl` | `data/` | **No** |
| `auto_trader_strategy_config.json` | `data/` | **No** |
| discovery audit captures | `DISCOVERY_AUDIT_DIR` (set in VPS `.env`) | **No** |
| replay fixtures / cached bars | — | **None exist anywhere in the repo** |

Verified by: `ls data` (no such directory), `find . -name "*.jsonl"` (zero results outside
`node_modules`/`target`), `find . -name "*.csv" -o -name "*.parquet"` (zero). `.gitignore` line 39
excludes `/data/`.

**The one committed empirical artifact** is the 2026-09-07 discovery census, summarised in
`docs/discovery-coverage-results-2026-09-07.md`. **[MEASURED]**

| Check | Result |
|---|---|
| Requested universe symbols | 12,934 |
| Symbols returning a raw snapshot | 12,628 |
| Missing snapshot responses | 306 |
| **Raw snapshots rejected for stale trade timestamps** | **12,627** |
| Audit records / recorded losses | 67 / 0 |
| Analyzer status | **`insufficient_evidence`** |
| `whole_market_recall` | **`null`** |

That near-total staleness rejection is itself a finding: a one-shot snapshot census reads
`latestTrade`, and for most of a 12.9k-symbol universe the latest trade is minutes or hours old, so
a ≤60s freshness filter rejects essentially everything. **A snapshot census cannot establish
coverage for illiquid names.** Continuous session recording is required instead.

**[MEASURED — this session]** Live production event rate, 20-second sample, single WS client, market
open, 2026-09-09: `ignition_event` 5,925 · `momentum_update` 3,893 · `halt_warning` 3,122 ·
`bar_update` 356 · `funnel_health` 1 → **≈ 650 events/second sustained**. This is the only live
throughput figure in this report and it was captured before this milestone began.

---

## 7. Data-quality and bias risks

Reviewed specifically for lookahead and survivorship. [MEASURED where code was read; flagged where
unverifiable]

**Clean by construction:**
- `evaluate_outcome` receives only `following_prices` — the series *after* the signal. Future bars
  cannot influence the signal itself. Evaluated "over whatever exists, not padded or extrapolated".
- `analyze_discovery.py` uses **receipt time, not backdated market time**, to compute alert lead —
  its comment says so explicitly. It also refuses to fill missing prices and requires bounded gaps.
- Edge-triggering prevents one continuous move from producing many "signals" **in the signal log**.

**Real risks, ranked:**

1. **Selection bias — severe, and acknowledged in-repo.** `live_pending_signals.jsonl` records only
   symbols Stockspotter *signalled*. Recall is therefore structurally unmeasurable from it. This is
   precisely why `discovery_audit` was built. **Any precision figure computed from the signal log
   alone is conditioned on detection and says nothing about misses.**
2. **Censoring conflated with timeout.** `OutcomeKind::TimedOut` covers both a genuine 20-bar
   non-resolution *and* a signal that simply ran out of data near session end. No censoring flag is
   recorded. **This biases hit-rate downward by an unknown amount**, concentrated in late-session
   signals. [INFERRED from code structure]
3. **Catalyst timestamps unverified.** `catalyst_update` tags carry a `timestamp`, but I could not
   establish whether it is *publication* time or *fetch* time. If fetch time, attaching a catalyst
   label to an earlier signal would leak later knowledge backward. **Flagged, not proven.**
4. **Movers rolling-best staleness.** Best-observed values persist until aged out. Harmless for a
   leaderboard; would be a contaminated feature if ever used at signal time.
5. **Repeated-signal inflation at the event layer.** Edge-triggering protects the *signal* log, but
   the ~650 ev/s stream, the client UI, and any event-level analysis are **not** protected. Treating
   events as independent samples would badly inflate any statistic.
6. **`Unknown` outcome kind on historical data.** Pre-existing VPS records predate `kind`/`final_pct`
   and deserialize as `Unknown`/`0.0`. Aggregation must exclude them — `metrics::aggregate` appears
   to check this, but the mixed-vintage log is a hazard for naïve analysis.

---

## 8. Quantitative baseline results available today

**[NOT YET MEASURABLE]** — and I want to be unambiguous rather than approximate.

Not computable today: forward returns at any horizon; precision at +2/+5/+10%; MFE/MAE
distributions; time-to-target; earliness; detector interaction; segmentation by any dimension;
auto-trader P&L, slippage or capture ratios.

**Reason:** zero local data (§6). The measurement code exists and appears sound; it has no input.

The complete set of real numbers available today is:

| Figure | Value | Source | Caveat |
|---|---|---|---|
| Live event rate | ≈650/sec | 20s sample, 1 client, 2026-09-09 | Single sample, market open |
| Universe size | 12,934 symbols | 2026-09-07 census | One capture |
| Snapshot coverage | 12,628 / 12,934 (97.6%) | same | Snapshot only, not subscription |
| Stale-rejected snapshots | 12,627 | same | Method artifact, see §6 |
| Discovery recall | **null** | analyzer output | Deliberately unclaimed |
| Momentum `overall` median | 0.46 | code comment, one session | No n, no date |
| Qualify threshold | 0.60 | `DEFAULT_QUALIFY_THRESHOLD` | Config, not outcome |

**Nothing here supports any claim about signal quality, and none of it should be cited as such.**

---

## 9. Ranking-quality results

**[NOT YET MEASURABLE — and structurally so.]** See §4. No ranked signal list exists or is recorded.
This is the highest-leverage gap in the brief, because the brief's own reasoning is correct: *a
detector can generate many false positives and still be extremely valuable if its ranking reliably
places the true runners at the top.* Stockspotter currently forfeits that possibility. The
momentum-scorer's continuous score is the obvious raw material and is presently thresholded away.

## 10. Earliness results

**[NOT YET MEASURABLE].** Requires (a) the eventual move's full path and (b) first-detection time.
The infrastructure for exactly this exists — `analyze_discovery.py` computes
`alert_lead_seconds = crossing − alert` using receipt time — but the 2026-09-07 capture yielded zero
labelled candidates. **The method is built and validated; only data is missing.** Note the brief's
"% of eventual move already completed before first detection" additionally requires pre-detection
price history, which nothing currently retains.

## 11. Noise / redundancy results

**Partially [MEASURED], mostly [NOT YET MEASURABLE].**

Measured: ≈650 events/sec, of which `ignition_event` alone is ~296/sec and `momentum_update`
~195/sec. Against `UNIVERSE_MAX_MONITORS = 6_000`, that is roughly one event per monitored symbol
every ~9 seconds.

Not measurable without data: events per symbol per episode, repeated signals before meaningful price
change, cross-detector correlation, alerts per actionable opportunity.

**[INFERRED]** The three-layer dedup design (push cooldown, edge-triggering, one-entry-per-day)
suggests redundancy was already recognised as a problem and addressed *per consumer* rather than at
the source. The event stream itself is undeduplicated.

## 12. Detector-interaction results

**[NOT YET MEASURABLE].** Worse than merely lacking data: **detector features are not recorded at
signal time.** `PendingSignal` carries `{symbol, strategy, timestamp, signal_price, captured_at}` —
no momentum score, no catalyst state, no halt state, no funnel features. So combinations such as
"ignition + high momentum" or "ignition + catalyst" **cannot be reconstructed retrospectively even
from a complete production journal.** This is a recording gap, not an analysis gap, and it is the
single most valuable thing to fix.

## 13. Missed-opportunity analysis

**[NOT YET MEASURABLE], for a well-understood reason that is already documented in-repo.**

The signal log contains only detected symbols, so it cannot distinguish "no runners occurred" from
"runners occurred and we missed them". `discovery_audit` exists precisely to close this by recording
the whole requested universe including symbols the scanner never selected — and it correctly refused
to claim recall on the one capture taken.

The label it uses is a reasonable opportunity definition to build on: **price $0.25–$3, a 5-minute
sampled flat base (max/min ≤ 1.02), followed by a +10% rise within 20 minutes**, chosen before
examining outcomes. It deliberately does *not* require a funnel pass, quiet-watch selection, float
or an alert — which is what makes it usable for recall.

**Precision-vs-recall verdict: unknown, and not guessable.** Anyone claiming either today is
reasoning from absent evidence.

## 14. Auto-Trader: signal quality vs execution quality

[MEASURED — code reading; no journal data]

**Entry:** three event kinds only — Micropullback (`ConsolidationEvent/EntryTriggered`),
ConsolidationBreakout (same kind, different strategy), and IgnitionDetector
(`IgnitionEvent/FollowThroughConfirmed`). Gated by `MOMENTUM_CONFIRM_THRESHOLD = 0.6`.

**Sizing:** $500 default, max 4 concurrent, adaptive after 20 trades — scale up at >55% win rate
(×1.1), down at <45% (×0.8), floor $100, cap ×1.5.

**Exit:** `TargetHit` · `StopHit` · `Timeout` · `MomentumDeteriorated` (momentum < 0.4), with a
**trailing stop** recomputed off `highest_price_since_entry` using the same bracket `stop_pct`.

**Skip taxonomy — genuinely valuable, and the best-instrumented decision surface in the system:**
`MomentumGateFailed`, `OutsideRegularHours`, `MaxConcurrentPositions`, `AlreadyEnteredToday`,
`ZeroQuantity`, `HaltRiskTooHigh`, `StrategyDisabled`.

**The separation the brief demands is *architecturally* possible and *empirically* blocked.** The
journal records skips with reasons and exits with reasons, so signal quality and execution quality
could be disentangled cleanly — with the journal. Two specific hypotheses worth testing once data
exists:

- **`MomentumDeteriorated` may be destroying good detector expectancy.** It exits on a *scorer*
  reading, not price. If momentum decays during healthy consolidation, this exits winners early.
- **`MaxConcurrentPositions = 4` makes outcomes path-dependent.** Rejected opportunities are
  logged, so counterfactual capture is computable — this is the cleanest available test of whether
  the constraint or the signal is the binding limit.

**[NOT YET MEASURABLE]:** latency, slippage, realized P&L, MFE captured, MAE endured, unrealized
opportunity after exit.

## 15. Proposed "OpportunityEpisode" unit — evidence **supports** it

**[INFERRED, with three concrete pieces of supporting evidence]:**

1. Edge-triggering already implements an implicit episode boundary for signal logging — the concept
   is present but partial and per-consumer.
2. `AlreadyEnteredToday` is an episode collapse in all but name, applied only to trading.
3. The discovery protocol's label (*flat base → +10% within 20 min*) is already episode-shaped:
   symbol + session + start + outcome.

**The event stream is measured at ~650/sec while episodes are plausibly a few hundred per session.**
Treating events as samples would overstate n by orders of magnitude and violate independence — the
brief is right to prohibit it.

**Recommended unit:** `symbol + session_date + episode_start`, accumulating detector confirmations,
peak, invalidation and final outcome. **Not implemented, per instruction.** One caveat: episode
*boundaries* are a modelling choice that will materially affect every downstream statistic, and must
be fixed **before** outcomes are examined to avoid choosing boundaries that flatter results.

## 16. Five largest alpha bottlenecks

| # | Bottleneck | Class | Why it dominates |
|---|---|---|---|
| **1** | **No signal-time feature recording** | **Measurement** | `PendingSignal` stores 5 fields. Detector interaction, segmentation and ranking calibration are **unreconstructable even with perfect production data**. Everything else waits on this. |
| **2** | **No ranking of signals** | **Ranking** | The brief's own key insight — precision matters less than ordering. The momentum score exists and is thresholded away. Highest upside, moderate cost. |
| **3** | **Recall unmeasured; no session capture exists** | **Data** | Cannot distinguish a precision problem from a recall problem. The recorder is built and validated; it has never had a real session run through it. |
| **4** | **Event-level architecture, not episode-level** | **Architecture** | ~650 ev/s of non-independent observations. Statistically invalid as a sample unit, and plausibly the root of the noise complaint. |
| **5** | **Outcome model lacks MAE, horizon grid and censoring flags** | **Measurement** | Cannot compute risk-adjusted quality, time-structure of moves, or unbiased hit rates. Small code change, large analytical unlock. |

Infrastructure and security findings are deliberately excluded — none currently impede alpha
measurement or signal delivery.

## 17. Highest-value next experiments, ranked

Each emerges from a finding above. **None implemented.**

1. **Record detector features at signal time** (from §12). Extend `PendingSignal` with momentum
   score, catalyst state, halt state, funnel features, tier of origin, and price history since
   episode start. *Prerequisite for experiments 3–6.* Low risk, additive.
2. **Run one full market-session discovery capture** (from §13). `DISCOVERY_AUDIT_DIR` is already set
   in the VPS `.env` and the analyzer is validated. Note the results doc's own warning: **~4 GB per
   regular session** — plan capacity first.
3. **Episode aggregation, boundaries fixed before outcomes are examined** (from §15).
4. **Ranking calibration** (from §4/§9): stop discarding the momentum score's ordering; test whether
   top-k by score outperforms the remainder. This is the first experiment that could plausibly move
   alpha rather than only measure it.
5. **Exit-policy separation** (from §14): re-evaluate detector expectancy with exits held fixed,
   specifically isolating `MomentumDeteriorated`.
6. **Add MAE, a horizon grid, and an explicit censoring flag to the outcome model** (from §5/§7).

## 18. Exact measurement gaps

| Gap | Blocks | Fix |
|---|---|---|
| No local data of any kind | All quantitative analysis | Export production JSONL to a local analysis copy |
| Features not recorded at signal time | §12, §7, ranking calibration | Extend `PendingSignal` |
| No signal ranking produced or recorded | §9 entirely | Emit and persist a ranked candidate list |
| No whole-universe session capture | §13 recall | One market-session `DISCOVERY_AUDIT_DIR` run |
| MAE absent | Risk-adjusted quality | Add to `SignalOutcome` |
| No horizon grid | §6 forward returns | Add multi-horizon sampling |
| Censoring indistinguishable from timeout | Unbiased hit rate | Add a censored flag |
| Pre-detection history not retained | Earliness "% of move completed" | Retain a lookback window per episode |
| Catalyst timestamp semantics unverified | Lookahead risk #3 | Inspect `python/app/news.py` provenance |

## 19. Recommendation for Alpha Milestone B

**Milestone B should be "Make It Measurable", not "Make It Better".**

Concretely, in order:

1. Extend signal-time recording (experiment 1) — the gate on everything else.
2. Capture one full market session with discovery audit enabled (experiment 2), capacity planned.
3. Export production JSONL to a local analysis copy so this analysis can actually run.
4. Define episode boundaries **before** looking at outcomes (experiment 3).
5. Only then re-run Milestone A's questions against real data.

**Do not tune anything until step 5 produces numbers.** Tuning against the current evidence base
would be optimising against an unmeasured baseline, and the discovery analyzer's own refusal to claim
recall is the right precedent to follow.

**One operational prerequisite worth stating plainly:** steps 2 and 3 both require production data
access, which this milestone forbade and which currently has no established export path. That is the
first thing Milestone B needs to arrange — before any strategy question can be asked empirically.

---

### Constraints observed

No thresholds tuned · no strategy logic modified · no auto-trader logic modified · no code committed
or pushed · nothing deployed · no SSH or production access · no new production data fetched · no
dependency changes · no infrastructure remediation continued · **no profitability claimed** · repeated
events explicitly **not** treated as independent samples.

Working tree unchanged: `git status --short` shows only `?? .claude/` and `?? AUDIT-2026-09-09.md`.
No analysis scripts were created — there was no data to analyse.
