# IAC and extreme momentum v1: experimental implementation contract

Base source: master 5807d6116f5c793d6fd62ac255f443620ce21fa8.
Approved scope: isolated implementation and retrospective testing on existing
captured sessions; no production activation, deployment, or frozen experiment edits.

## Separate strategies

IAC detects compression BEFORE a bullish expansion and structural break. Extreme
momentum detects exceptional trailing price/participation expansion independently.
Neither consumes the other's outputs or gates the other. Float is unavailable
annotation, never substituted with outstanding shares or inferred from a provider
date. Existing ignition, consolidation, micropullback, OI and auto-trader remain
unchanged. Shared bounded raw bar history is immutable at decision time.

## Declared research hypotheses, before retrospective outcomes

These are transparent initial research values, NOT fitted or validated thresholds.
No threshold sweep or WFF-specific rule is authorized by this contract.

| Parameter | v1 value |
|---|---:|
| Prior baseline | 20 completed one-minute bars |
| IAC compression window | 5 bars, immediately before candidate |
| Compression mean range / preceding baseline mean range | <= 0.60 |
| Compression envelope / preceding baseline mean range | <= 2.0 |
| Anchor range / compression mean range | >= 2.0 |
| Anchor volume / compression mean volume | >= 2.0 |
| Anchor bullish body / range | >= 0.60 |
| Break | close above every compression high |
| Extreme trailing window | 3 completed bars |
| Extreme return | >= max(3%, 3 x baseline mean fractional range) |
| Extreme mean volume / preceding baseline mean volume | >= 3.0 |
| Extreme path efficiency (net gain / sum absolute close changes) | >= 0.70 |
| Extreme bullish bars | >= 2 of 3 |
| Last close location within last bar range | >= 0.75 |
| Candidate hold observation | 120 seconds of market time AND >= 2 subsequently received bars |
| Continuation observation | close >= anchor close + anchor range |
| Structural failure | subsequent bar low below anchor low |
| Lifecycle expiry | 300 seconds |
| Per-strategy cooldown from candidate | 600 seconds of market time |
| Missing minute, correction, stale source, invalid input | invalidate active lifecycle and reset warmup |
| Maximum source age at decision | 90 seconds from bar close |

All baselines exclude the candidate (and exclude the extreme trailing window for
extreme momentum). Zero range/volume/path denominators yield unavailable, not a
clipped ratio. No cross-symbol or full-session future distributions are read.
Both detectors run anywhere in the observed session after enough contiguous bars.
Every event has event/receipt/decision timestamps, frozen anchor measurements,
contract version, stable lifecycle identity, and append-only supersession links.
Held/continued/failed are descriptive observations, not predicted profitable trades.
Bar OHLC cannot establish path ordering inside a minute or executable fills.

## Isolation and delivery

IAC_ENABLED=true and EXTREME_MOMENTUM_ENABLED=true independently opt in. Any other
value is off. Both are off by default. An independent bounded subscriber consumes
already-published final 60-second BarUpdate records, never changing ScanEvent or
EventFrame, frozen ranking, observer, push notifications or orders. Dedicated
experimental WebSocket messages use a SEPARATE authenticated web/desktop socket
with experimentalOnly=true and experimentalStrategies=true. The ordinary socket
never receives them. The saved Research panels preference opts the web/desktop
client into this second socket and overrides the build recipe. Without a saved
choice, VITE_EXPERIMENTAL_STRATEGIES=true selects the build default; all other
values are off. The web Dockerfile accepts that non-secret build argument.
Native mobile delivery is deferred.
With both flags off, the original 64-slot admission/connection behavior is preserved.
With either flag on, handshake admission is bounded at 96, with separate post-auth
caps of 64 ordinary connections and 32 research sockets. Capacity rejection is
explicit; the research client retries with bounded exponential backoff and jitter.
Independent rows have no inherited $20/$25 price ceiling and are labeled
experimental. No candidate implies an empty observed sample, not market-wide absence.

Collector capacity: at most 256 symbols, 64 bars per symbol, 100 retained events;
LRU eviction invalidates any active lifecycle and is counted, with the last 100
evicted symbols retained. Output channel 128. Lag resets detector state and emits
explicit degraded health; the health/gap warning remains sticky for the run.
Processing budgets: 5ms per bar warning; three consecutive 25ms breaches shut down
this subscriber only, publishing already-computed events before invalidation.
Budget breaches are counted and the UI reports a paused research feed. These are new
research budgets; frozen observer budgets are untouched. They are not VPS latency
proof. Reconnect/input gaps force new warmup. No recorder or retention change.

## Historical evaluation plan

Use existing audit-validation SIP session files, read-only. Predeclare April 20
through June 30 as development; July 1 through September 4 as retrospective
evaluation. This is a date split, NOT a claim the latter was previously untouched:
these captures have already supported prior research and are selected symbols.
Freeze these defaults before loading outcomes; do not retune after either split.
Hash every source input, retain metadata and exclusions, and hash the fixed config.
Replay the exact core used by the live subscriber with assumed availability at bar
close when original receipt evidence is absent, explicitly labeled as such.

Hash the outcome evaluator and reverify it, the binary and core at completion.
Report per-strategy/session candidate, hold, continuation, failure, invalidation,
expiry, warmup/gap/correction and runtime counts. Separately measure +2% reach
within 300 seconds after candidate and after hold using only subsequent bars,
anchor close denominator. Incomplete/gapped future horizons are censored. Use
descriptive summaries by date and symbol; no profitability, fill, whole-market
recall or prospective efficacy claim. Hold comparisons have their own later anchor.
Historical bars cannot recreate the revision/receipt sequence; corrected end-state
history is idealized bar replay, not as-received reproduction.

September 21–24 capped V2/V2.1 captures, frozen consumer-received-protocol-v1,
Step4 cohort/budget/rank-completion anchor/30s dual freshness gates/300s outcome
contract remain immutable. This experiment does not enter that campaign. A future
confirmation cohort needs separate preregistration and approval to capture/enable.

Predeclared descriptive comparator: all contiguous-warmup, non-candidate bars in
the same archived symbol/session, separately per strategy, with the identical
five-minute censoring and target rule. Report this base rate beside candidate
counts, not as matched causal inference or detector-equivalent rejection. Controls
include active-lifecycle/cooldown bars and zero compressed-window denominators;
only the prior baseline range and volume must be positive. Post-hold horizons may
extend beyond lifecycle expiry and overlap other horizons; no independence claim.
Reconnect snapshots retain at most 100 old lifecycles, with visible price age.
Ratio cells with fewer than 20 evaluable
observations are suppressed; counts and censoring always remain visible. The two
date splits are descriptive strata, not pristine out-of-sample validation. The
first close-path leg is first trailing-bar open to close. Source symbol-selection
provenance is unavailable; the sample is known to have supported mover research.
