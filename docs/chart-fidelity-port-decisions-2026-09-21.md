# Chart-fidelity port: open decisions and classifications

Branch `port/chart-fidelity-onto-7e36586`. Not merged, not deployed.
Production remains `7e365866ce5ae18ea77cce05d6d8f45bc69c7faa`.

## 1. Out-of-order print handling — DEFENSIVE, not demonstrated

**Classification: keep the protection, keep the test, do not let this
justify a deployment.**

Measured on the 2026-09-21 regular session, from `discovery_audit` ignition
records checked per symbol in emission order:

```
prints examined                        83,229
symbols                                 4,881
symbols with >=1 backward market_at         0
total backward prints                       0   (0.0000%)
```

The condition the earlier audit reproduced synthetically has **zero live
incidence**. It is a correctness property worth holding, not a problem worth
shipping for.

What the measurement does *not* cover, stated so the zero is not
over-read:

- It sees ignition prints — monitored symbols, after internal filtering —
  not every raw trade on every symbol.
- Equal timestamps are not "backward" and were not counted. They are
  common, and `chart_bars` resolves them by arrival order because `Trade`
  carries no exchange sequence or trade ID. That remains a genuine fidelity
  caveat: two trades at the same nanosecond can be ordered differently than
  the exchange ordered them, which can change a bucket's open or close.
  Fixing it needs trade identity on the wire, which is a protocol change.
- One session. A halt-resume or a venue replay could plausibly produce
  backward prints on a day this one did not.

The protection in `chart_bars` is already clean — a late trade updates its
own bucket and cannot rewind the current one, covered by
`rollover_keeps_tail_and_late_trade_does_not_rewind_current_bucket`. Nothing
to remove and nothing to weaken.

## 2. Mid-bucket coverage — CONTRACT PROPOSED, NOT IMPLEMENTED

Per the 2026-09-21 brief, the contract is reported for a decision before any
semantic change is made. **No fix is implemented on this branch.** The
characterization test
`mid_bucket_coverage_start_publishes_a_partial_bar_indistinguishable_from_a_full_one`
pins the current behaviour so a future change has something to break.

### The witness

DDC, 15:37 UTC: provisional volume 96,108 against an authoritative 119,482.
23,374 shares (19.6%) missing, with a silent tail of only ~0.3 s before the
boundary. 23,374 shares cannot trade in 300 ms, so the rollover-tail
mechanism does not explain it.

### Root cause

Not arithmetic. `ChartBars` is correct about what it saw, and `Bucket.first`
already records that coverage began 44.9 s into the minute in the
reproduction. The defect is that **`ScanEvent::BarUpdate` has no field able
to express partial coverage**, so a bucket observed for 60 seconds and one
observed for 15 serialise identically. `is_final: false` is the only
qualifier available, and it means "provisional", not "partial" — every
locally aggregated bar is provisional, so it carries no information here.

This is therefore a protocol gap, which is why it is not a one-line fix.

### Why coverage starts late at all

Three paths, all real:

1. A symbol becomes chart-eligible mid-minute (the `momentum_windows` gate),
   so aggregation begins part-way through a bucket.
2. The client connects, or reconnects, mid-minute.
3. ws-server restarts mid-minute.

### Options considered

| Option | Verdict |
|---|---|
| Merge authoritative REST history over the live partial bar | Wrong layer. The client already does this for 1m via `/bars/:symbol`, and it is why the 1m chart self-corrects within ~100 ms of the boundary. It cannot help 30s, which has no authoritative source. |
| Backfill the start of the bucket server-side | Needs a sub-minute authoritative source. None exists. Would mean inventing data. |
| Suppress the bar until coverage is complete | Rejected. Makes a live chart blank for up to a minute after a symbol becomes eligible, which is worse than a short bar and hides real price action. |
| **Mark the bucket's coverage explicitly** | **Proposed.** |

### Proposed contract

Add coverage provenance to the bar, and let the client decide how to render
it:

```
BarUpdate {
    ...
    /// First trade timestamp actually observed in this bucket. Equal to the
    /// bucket start when coverage was complete from the boundary.
    coverage_from: DateTime<Utc>,
    /// True when coverage_from is materially after the bucket start, i.e.
    /// this OHLCV describes part of the interval, not all of it.
    partial_coverage: bool,
}
```

Semantics:

- `partial_coverage` is **never** true for a provider-official bar
  (`is_final: true`), because those describe the whole interval by
  construction.
- It is set from `Bucket.first` against `floor_to_interval`, which the
  aggregator already has — no new state.
- A threshold is needed, since the first trade of a minute is never exactly
  at the boundary. Proposal: material if `coverage_from - bucket_start`
  exceeds 10% of the interval (6 s for 1m, 3 s for 30s). This is a
  presentation threshold, not a detection parameter, and touches nothing
  the measurement session depends on.
- Additive and backward compatible: an older client ignores both fields and
  behaves exactly as today.

Client side, once available: render a partial candle distinctly (the
existing freshness surface is the natural place) and never treat it as a
complete interval. On 1m it will be replaced by the official bar within
~100 ms anyway; on 30s the marking is the only honest option available.

**Decision required before implementation.** It changes a shared wire type,
so it needs the mobile and desktop clients released in step, and it must not
be bundled with today's session.

## 3. 50 ms cadence — SAFE, BUT WEAKLY MOTIVATED

Load-tested in `crates/ws-server/examples/chart_cadence_load.rs`. No
backpressure at any modelled rate, including a ceiling 195× the projected
load: zero lagged receivers, 0.42 µs and 226 bytes per frame, derived fanout
CPU of 0.018% of one core at the projected rate.

The weak part is the benefit, not the cost. Of 691 observed symbol-streams,
683 (98.8%) are trade-bound — they already publish as often as trades
arrive, well below the 500 ms throttle — so 50 ms cannot make them fresher.
Only 8 streams can go faster and only 4 are genuinely at the cap.

Recommendation: deploy it as part of the aggregation fix rather than for its
own sake. It is a prerequisite for the rollover-tail fix being visible at
sub-second granularity, and it costs effectively nothing. It should not be
described as a general freshness improvement, because for 98.8% of the
universe it is not one.
