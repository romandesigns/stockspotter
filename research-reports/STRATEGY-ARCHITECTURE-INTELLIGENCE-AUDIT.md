# STOCKSPOTTER — STRATEGY & ARCHITECTURE INTELLIGENCE AUDIT

**Read-only audit. No code, config, data, threshold, service, or deployment was modified.**

| | |
|---|---|
| Frozen baseline | `ba722698af4fa2b387339eb017a2d58734f969c7` |
| Branch | `release/operating-run-20260907` |
| Verified at start and end of audit | **MATCH ✓** (working tree carries only untracked `.claude/` and `AUDIT-2026-09-09.md`) |
| Date | 2026-09-09 |
| Purpose | Give an external architect a code-level model of how Stockspotter decides a stock is becoming interesting — **formed before any BASELINE SESSION 001 outcome data is seen** |
| Outcome data inspected | **None.** No episode file, no export, no journal, no hit rate, no MFE distribution |

---

## 1. Scope, method, and honest coverage

### What this document is

A map of the *decision machinery*. Every threshold, formula, gate, suppression rule, timing
boundary and fail-direction quoted below was read out of the source at the frozen commit, not
inferred from documentation or memory. Where the code's own doc comments assert a performance
number, that number is quoted **as a claim made by the code**, clearly labelled, never as an audit
finding.

### Files read completely

`crates/fast-funnel/src/{types,filters}.rs` · `crates/momentum-scorer/src/{scorer,candle}.rs` ·
`crates/ignition-detector/src/{detect,flat_base,follow_through,monitor}.rs` ·
`crates/consolidation-breakout/src/surge.rs` · `crates/halt-detector/src/{lib,bands}.rs` ·
`crates/market-data/src/{trading_session,session,movers}.rs` ·
`crates/backtest-metrics/src/{outcome,strategy_config}.rs` (decision paths) ·
`crates/auto-trader/src/engine.rs` (full `on_event`/`try_enter`/`on_bar` tree) ·
`packages/shared-types/src/index.ts` · `python/app/{news,assess}.py` · `python/export_session.py`

### Files read in the parts that carry decisions

`crates/market-data/src/live.rs` (2,050 lines — constants block, `run_live_scan` setup, the `Bar`
/`Trade`/`Quote`/`Status` handlers, the rescan/promotion arm, tier configs) ·
`crates/market-data/src/universe.rs` (990 lines — `fetch_universe`, `scan_shortlist`, `FloatCache`,
`select_quiet_watch`) · `crates/ws-server/src/{server,measurement,push}.rs` ·
`crates/backtest-metrics/src/{episode,context,horizon}.rs`

### Not read

Client rendering code (`apps/client`, `apps/mobile`) beyond the shared protocol contract;
`crates/replay-engine`; `crates/backtest-metrics/src/bin/*`; `crates/auto-trader/src/paper*.rs`
beyond its interface; test bodies except where a test pins a behaviour I cite. **Conclusions below
are therefore about the detection and decision path, not about presentation or about the offline
backtest binaries.**

### One methodological note

This codebase is unusually well commented, and its comments contain real measured history (sweeps,
sample sizes, live incidents). That is a genuine asset. It is also a hazard for an audit: it is easy
to read a doc comment and record it as a fact about the system's *behaviour* when it is a fact about
a *past measurement*. I have tried to keep those separate throughout, and §26 collects the
performance claims in one place precisely so they can be checked against BASELINE SESSION 001 rather
than absorbed as truth.

---

## 2. System topology — five processes, and what each one is allowed to decide

```
Alpaca SIP feed ──WS──┐
Alpaca REST  ─────────┤
FMP (float)  ─────────┤ market-data crate (in-process, inside ws-server)
                      │   ├─ universe.rs   : periodic REST universe scan  → shortlist
                      │   ├─ movers.rs     : independent REST scan        → leaderboards
                      │   └─ live.rs       : the single live event loop; owns every detector
                      │        ├─ SessionTracker      (per funnel symbol)
                      │        ├─ momentum_windows    (per tracked symbol)
                      │        ├─ ignition_monitors   (per tracked symbol)
                      │        ├─ halt_monitors       (per tracked symbol)
                      │        ├─ consolidation_monitors
                      │        ├─ micropullback_monitors
                      │        └─ universe_monitors   (opt-in tier)
                      │
                      └──► broadcast::Sender<ScanEvent>  (capacity 16_384)
                                 │
      ┌──────────────────────────┼───────────────────────────┬─────────────────┐
      ▼                          ▼                           ▼                 ▼
 ws-server::server        ws-server::measurement      ws-server::push    discovery_audit
 (fan-out to clients,     (research capture,          (Expo push,        (raw NDJSON
  EventSnapshot resend)    episodes + horizons)        15-min cooldown)   coverage log)
                                 │
                                 ▼
                          data/research/*.ndjson
      │
      ▼ (separate OS process, over the public WebSocket, as a client)
 auto-trader  ── Alpaca paper API
```

**The critical architectural fact: `auto-trader` is a separate process that consumes the same
public `ScanEvent` wire every phone and browser consumes.** It has no privileged access to detector
internals. Anything a detector computes but does not put on the wire is invisible to the trading
decision — permanently, by construction. §17 inventories exactly what that loses.

The second critical fact: **all detection happens in one `tokio::select!` loop in one task**
(`run_live_scan`). Detectors are not independent services; they are `HashMap`s in one function's
local scope. This is why per-symbol state (momentum windows, ignition history) is destroyed the
moment a symbol leaves its coverage tier (§4), and why detector state is entirely non-durable across
a restart.

---

## 3. Data intake — what Stockspotter sees, and what it never sees

| Input | Source | Where consumed | Reaches the event wire? |
|---|---|---|---|
| Trade prints (price, size, ts) | Alpaca WS `t` | ignition, halt, live candles | Only derived (`BarUpdate`, ignition/halt events) |
| **Quotes (bid/ask/sizes)** | Alpaca WS `q` | **ignition only** | **No — never** |
| 1-minute bars | Alpaca WS `b` | funnel, momentum, consolidation ×2 | `BarUpdate` (`intervalSecs: 60`, `isFinal: true`) |
| Corrected bars | Alpaca WS `u` | `UpdatedBar` handler | — |
| Trading status (halt/resume) | Alpaca WS `s` | ignition halt-lift, halt detector | Indirectly |
| LULD bands | Alpaca WS `l` | halt detector (real bands vs estimated) | `HaltWarning.estimatedBands` |
| Daily snapshots (~12.6k symbols) | Alpaca REST `/snapshots` | universe scan, movers | No |
| 20-day daily bars | Alpaca REST | `avg_daily_volume`, `prior_close` | No |
| Free float | FMP `shares-float` | Stage 1 only | `FunnelSignal.floatOk` (bool only) |
| News headlines (10 most recent) | Alpaca `/v1beta1/news` | Python `qualify` | `CatalystUpdate` |
| LLM assessment | Anthropic API + web search | chart panel only | No (HTTP, not WS) |

### Structural blind spots at the intake layer

1. **Quotes never leave the ignition detector.** Spread, bid/ask sizes, order-book imbalance are
   computed (`detect.rs::spread_ratio`, `ask_absorbed`) and then discarded. The auto-trader cannot
   see the spread it is about to pay, and the measurement engine cannot record it as a feature.
   Since the auto-trader's own strategy-selection maths subtracts a *hardcoded assumed*
   `DEFAULT_ROUND_TRIP_COST_PCT = 0.5` (§22), and the real spread is measured but thrown away, the
   system assumes a cost it is capable of observing.
2. **No sub-minute official bars exist.** Alpaca rejects `timeframe=30Sec`
   (`live.rs:329` doc comment, confirmed live). The 30-second candle is synthesised from trade ticks
   and is **permanently uncorrected** — it never receives an authoritative replacement the way the
   60-second `LiveBar` does.
3. **No overnight session data.** `trading_session.rs` defines `Overnight` for completeness and
   states plainly that real overnight ATS trading is not visible through Alpaca at all.
4. **No order-book depth, no Level 2, no tape-by-exchange.**
5. **Float is a single scalar with no history.** Cached per ET day (`FloatCache`), so a genuine
   intraday dilution event is invisible until the next session.

---

## 4. Universe construction and the four coverage tiers

### The universe

`universe.rs::fetch_universe` → Alpaca `/v2/assets?status=active&asset_class=us_equity`, filtered to
`tradable && status == "active" && !is_warrant(name)`. Warrants are excluded by **name substring
`"warrant"`**, deliberately not by ticker suffix. Production logs 12,635 symbols.

Snapshots are fetched in chunks of `SNAPSHOT_CHUNK_SIZE = 200`.

### The four tiers that decide which symbols get detectors at all

This is the single most consequential structure in the system, because **a symbol with no monitor
cannot generate a signal, no matter what it does.**

| Tier | Selection rule | Refresh | Cap | Which detectors |
|---|---|---|---|---|
| **1. Funnel** | Full Stage 1+2 pass (§5) | `UNIVERSE_RESCAN_INTERVAL = 15s` | none | SessionTracker, momentum, ignition, halt, consolidation, micropullback |
| **2. Movers** | Top 25 gainers ∪ top 25 most-active, from rolling-24h best (§14) | `HALT_WATCH_REFRESH_INTERVAL = 60s` | 50 | momentum, ignition, halt, consolidation, micropullback (**no SessionTracker, no FunnelSignal**) |
| **3. Quiet watch** | `select_quiet_watch` — the **inverse** profile (§4.1) | 15s (same scan) | `max_symbols = 150`, `QUIET_TOTAL_CAP = 300` | **ignition only** |
| **4. Universe** | Every symbol that prints a trade — opt-in via `IGNITION_UNIVERSE_MODE=1` | on first print | `UNIVERSE_MAX_MONITORS = 6_000` | **ignition only, trades-only** |

### 4.1 Quiet watch — the deliberate inverse

```rust
QuietWatchConfig {
    min_price: 0.25, max_price: 3.00,
    max_relative_volume: 1.0,     // must be QUIET
    max_abs_gap_pct: 5.0,         // must NOT be gapping
    min_avg_daily_volume: 100_000,
    max_symbols: 150,
}
```
Ranked by **trailing average daily volume descending** — explicitly *not* by how quiet a name is
("quietest optimizes for dead, which is the opposite of useful", `universe.rs:675` region). Unknown
baseline → `rel_vol = INFINITY` → fails closed.

This tier exists because the ignition detector was structurally blind to the flat-base pattern: a
flat, quiet, non-gapping stock can never clear Stage 2, so it never got an `IgnitionMonitor`.

`QUIET_TRANSITION_GRACE = 300s` keeps a symbol subscribed for five minutes after it stops being
selected, so a stock that ignites (and therefore stops being quiet) is not unsubscribed at the exact
moment it becomes interesting. **This is one of the sharper pieces of design in the codebase** — it
is the only place where a tier deliberately holds on to a symbol *because* it stopped matching the
tier's own criteria.

### 4.2 Tier membership is fragile, and the code knows it

`MAX_CONSECUTIVE_WATCHLIST_MISSES = 2` exists because of a real logged incident (`live.rs:262`
region): **YQ, a genuine large mover, flapped in and out of Stage 1/2 qualification every 15–45
seconds, and every re-add reset its momentum window to `candles_buffered = 1`.** The momentum
scorer needs 21 candles for a non-zero `ma_slope` (§7). A symbol that flaps therefore *can never
produce a complete momentum score*, and the only detector that ever caught YQ was halt-warning,
whose coverage was already decoupled.

The fix was the movers tier plus a 2-miss tolerance. The underlying fragility — **detector state
lives and dies with tier membership** — is unchanged.

### 4.3 The universe tier is not detection-equivalent, despite the comment

`universe_monitor_config()` (`live.rs:212`) sets `max_trades: 120`, **`max_quotes: 0`**, everything
else default. Its doc comment states: *"The detection logic is deliberately identical to every other
tier — a symbol must not become more or less likely to fire based on which coverage tier happened to
pick it up."*

That claim does not hold, and the reason is arithmetic. `detect.rs:194`:

```rust
triggered: trade_frequency_spiked || spread_tightened || ask_absorbed_flag
```

It is an **OR**. With `max_quotes: 0`, `spread_ratio` and `ask_absorbed` both return `None` and can
never contribute. A universe-tier symbol therefore has **one of three tick-level signals available**
and is strictly *less* likely to open a candidate than the same symbol in tiers 1–3. The
configuration is defensible (the tier subscribes trades only, so no quote data exists to feed it);
the comment's equivalence claim is not.

---

## 5. The Fast Funnel — Stage 1 / Stage 2 arithmetic

`fast-funnel` is pure, synchronous, and has no state. `FilterThresholds::default()`:

```rust
min_price: 0.25,  max_price: 20.0,
max_float_shares: 20_000_000,
min_relative_volume: 5.0,
min_gap_pct: 10.0,
```

`filters.rs::explain`:

| Check | Expression | Fail direction on missing data |
|---|---|---|
| `price_ok` | `min_price <= price <= max_price` | — |
| `float_ok` | `float_shares` is `Some(f)` and `f <= 20_000_000` | **fails CLOSED** on `None` |
| `rel_vol_ok` | `relative_volume() >= 5.0` | **fails CLOSED** (`None` when `avg_daily_volume == 0`) |
| `gap_ok` | `gap_pct >= 10.0` | — |

- **Stage 1** = `price_ok && float_ok`
- **Stage 2** = `rel_vol_ok && gap_ok`
- **`passed()` = `stage1_passed() && rel_vol_ok && gap_ok`** — all four.

### 5.1 The gap gate is signed, not absolute

`gap_ok: t.gap_pct >= thresholds.min_gap_pct` — **`>=`, on a signed value.** A stock down 40% on 20×
volume with a news catalyst is structurally invisible to the funnel. **Stockspotter is a long-only,
up-gap-only scanner at the funnel layer**, and nothing downstream reintroduces short or reversal
candidates: `movers.rs` ranks `change_pct` descending (top gainers only), and the auto-trader has no
short path at all. This is a design decision, not a defect — but it is a decision that removes an
entire half of the return distribution from ever being measured.

### 5.2 Float is the funnel's binding constraint, and it is rate-limited

Float comes from FMP, and the budget is severe:

```rust
MAX_FLOAT_CHECKS_PER_SCAN: usize = 60;            // per 15s scan
DEFAULT_FMP_DAILY_REQUEST_BUDGET: u32 = 240;      // per DAY (free tier is 250)
FLOAT_LOOKUP_FAILURE_COOLDOWN: Duration = 600s;
```

Because unknown float **fails Stage 1 closed**, exhausting the daily budget makes the funnel pass
*nothing*, and an empty Gap & Go panel becomes indistinguishable from a quiet market. That failure
mode is why `ScanEvent::FunnelHealth` exists (`starvedCandidates`, `apiKeyMissing`) — the system
publishes its own blindness. That is genuinely good instrumentation.

When demand exceeds budget, candidates are prioritised by:

```rust
score = |gap_pct| * (session_volume / max(avg_daily_volume, 1))
```

**This is the only place in the entire detection path where candidates are ranked against each other
by an explicit priority score.** Note what it implies: when the budget binds, the funnel's effective
selectivity silently increases, and *which* symbols survive is decided by a formula that was never
backtested and is nowhere surfaced as a strategy parameter. On a genuinely busy morning the funnel is
not running its documented thresholds — it is running its documented thresholds *plus* a
budget-induced top-60-by-`|gap|×relvol` cut.

Successful lookups are cached for the ET day (`FloatCache.known`), including a legitimate
`Ok(None)` ("FMP has no float for this security class"), so a permanently-uncoverable ticker cannot
re-burn budget. `PLUN.RT` is the named real incident behind the cooldown.

---

## 6. Relative volume — the most consequential un-normalised quantity in the system

```rust
// crates/fast-funnel/src/types.rs
pub fn relative_volume(&self) -> Option<f64> {
    if self.avg_daily_volume == 0 { return None; }
    Some(self.session_volume as f64 / self.avg_daily_volume as f64)
}
```

`session_volume` is **cumulative since the session began**. `avg_daily_volume` is a **20-day
full-day average** (`DAILY_LOOKBACK = 20`). There is **no time-of-day normalisation anywhere** —
no minute-of-session curve, no expected-volume-by-now denominator, no U-shape adjustment.

The threshold is `>= 5.0`.

### Why this matters more than any other single line of code

To clear Stage 2 at 09:45 ET, a stock must have already printed **five times its entire average
daily volume in fifteen minutes**. The same stock clears the identical gate far more easily at 15:30
having done nothing new, purely because the numerator has had six hours to accumulate.

The consequences are all in the same direction:

- **The funnel is structurally biased late.** Detection probability rises monotonically through the
  session for a fixed level of genuine unusualness.
- **The premarket and opening window — the exact period this project's own framing calls out
  ("opening at 4AM", `trading_session.rs`) — is where the gate is hardest to clear**, and where
  low-float runners actually make their initial move.
- **Any measured relationship between "time of first detection" and "how much move remained" is
  confounded by this**, because the detector's own sensitivity is a function of time-of-day. This is
  the single biggest interpretive hazard for BASELINE SESSION 001 (§28).
- Every other consumer of relative volume inherits it: the halt detector's
  `volume_escalation_rel_volume = 3.0`, `select_quiet_watch`'s `max_relative_volume = 1.0`, and the
  float-budget priority score.

Note the compounding effect: `select_quiet_watch` requires `rel_vol <= 1.0`. Early in the session
*almost every stock* satisfies that, so the quiet tier's 150 slots are, in the first hour, allocated
essentially by `avg_daily_volume` descending across a near-unfiltered universe rather than by
genuine quietness. Late in the session the same rule is genuinely selective. **The quiet tier is a
different tier at 09:35 than it is at 15:35.**

---

## 7. The Momentum Scorer — exact formula and warm-up

```rust
overall = volume_confirmation * 0.4
        + structure           * 0.3
        + ma_slope            * 0.2
        + wick_rejection      * 0.1
```

| Component | Definition | Range |
|---|---|---|
| `volume_confirmation` | `up_volume / (up_volume + down_volume)` over the window | 0..1 |
| `structure` | fraction of consecutive candle pairs with **higher high AND higher low** | 0..1 |
| `ma_slope` | hits / 3 over {9-MA rising, 20-MA rising, price above both} | {0, ⅓, ⅔, 1} |
| `wick_rejection` | `1 − (candles with upper_wick/range >= 0.5) / total` | 0..1 |

`DEFAULT_QUALIFY_THRESHOLD = 0.60`. Window is a capacity-bounded `RollingWindow` of
`MOMENTUM_WINDOW = 30` candles.

### 7.1 The warm-up cliff

`ma_slope` **requires `MA_LONG + 1 = 21` candles or it returns `0.0`.** Not `None` — zero. Because
its weight is 0.2, a symbol with fewer than 21 bars has a **hard ceiling of 0.80** on `overall`, and
its score is depressed by up to 20 points purely by insufficient history.

Combine with §4.2: a symbol that flaps in and out of a tier resets its window. **A flapping symbol
is arithmetically incapable of reaching a high momentum score**, independent of its price action.
The auto-trader's entry gate requires `overall >= 0.6` (§20), so the momentum gate is partly a test
of *tier-membership stability*, not only of momentum.

### 7.2 What the code says about the score's real distribution

`scorer.rs`'s own doc comments record a measured distribution on a **+41% gap day**: maximum
observed `0.80`, **median `0.46`** — i.e. the median bar on a huge up-day fails the 0.60 gate.

They also record a threshold sweep that is **not monotonic** in hit rate vs. threshold in the way a
clean signal would be:

| Threshold | Signals | Hit rate |
|---|---|---|
| 0.55 | 248 | 14.9% |
| 0.60 | 175 | 20.0% |
| 0.65 | 110 | 22.7% |

Hit rate rises with the threshold while the sample collapses — consistent with a weak monotone
signal, and consistent with the code's own decision (§22) that `MomentumScorer` is *not* an
actionable standalone trigger.

### 7.3 A structural observation about the weights

The two highest-weighted components (0.4 + 0.3 = 70%) are both **path-shape** measures over the
window. Neither is scaled by *magnitude*. A stock grinding up 0.4% in a perfect staircase can score
identically to one up 25% in the same shape. `overall` is a **quality-of-trend** score with no
notion of *size* of trend — and nothing downstream reintroduces magnitude into the momentum gate.

---

## 8. The Ignition Detector — tick-level microstructure

Four signals. Three are computed fresh from rolling windows (`detect.rs`, pure); the fourth is a
transition and lives in the stateful monitor.

```rust
IgnitionThresholds {
    trade_frequency_spike_ratio: 3.0,
    spread_tighten_ratio: 0.5,
    ask_absorption_min_drop_ratio: 0.6,
    min_recent_trades_for_spike: 3,
}
MonitorConfig {
    max_trades: 500, max_quotes: 500,
    recent_window_secs: 1.0, baseline_window_secs: 20.0,
    spread_recent_n: 5, spread_baseline_n: 20,
    confirmation_trade_count: 20,
    flat_base: Some(FlatBaseThresholds::default()),   // ON since 2026-09-06
    alert_cooldown_secs: 300.0,
}
```

| Signal | Computation | `None` when |
|---|---|---|
| Trade-frequency spike | `(recent trades / 1.0s) / (baseline trades / 20.0s) >= 3.0`, and `>= 3` recent trades | history doesn't reach back a full baseline window |
| Spread tightening | mean spread of last 5 quotes / mean of prior 20 `<= 0.5` | `quotes.len() < 25` |
| Ask absorption | ask size dropped by `>= 60%` **AND** `price_held_or_rose` | fewer quotes than needed |
| Halt-lift resumption | `on_status()` saw halted→resumed | n/a (stateful) |

`triggered = A || B || C` (halt-lift handled separately, and it **bypasses the cooldown**).

### Observations

1. **The recent/baseline ratio is 1s vs 20s.** This is a genuinely fast, genuinely microstructural
   detector — appropriately matched to the `scalp` outcome bracket (§21). But a 1-second numerator on
   a thin, low-float name is a very small sample: three prints in a second against a 20-second
   baseline of, say, four prints yields a ratio of 15. **The detector's sensitivity is inversely
   related to baseline liquidity**, so it fires most readily on exactly the names where a 3× rate
   increase is least informative. There is no minimum-baseline-activity floor, only
   `min_recent_trades_for_spike: 3` on the numerator.
2. **`ask_absorbed` is a conjunction with a price condition** (`price_held_or_rose && drop_ratio >=
   0.6`), which is the correct shape — size disappearing on a *falling* price is not absorption.
3. Signals fire on an **OR**, so the three are not combined, weighted, or scored — they are
   alternatives. There is no notion of "two signals agreeing is stronger than one" anywhere in the
   pipeline, and `IgnitionSignals` (which carries all three readings) is **not** placed on the event
   wire — only the resulting `kind` is (§17). **Which signal fired is unrecoverable downstream.**

---

## 9. The ignition state machine — candidate, confirmation, cooldown

`IgnitionMonitor::on_trade`, in evaluation order:

1. If a `pending` candidate exists → append price to `prices_after`; once
   `prices_after.len() == confirmation_trade_count (20)`, run `confirm()` → emit
   `FollowThroughResolved`. **On `confirmed`, and only then, start the cooldown clock.**
2. Else if `resume_awaiting_first_trade` (a halt lift was flagged) → this print becomes the breakout
   level; open a candidate with `halt_lift: true`, **bypassing the cooldown**.
3. Else run `detect()`. If `triggered`: check `in_alert_cooldown(now)` **and** the flat-base gate;
   if either blocks, emit nothing. Otherwise open a candidate.

`follow_through.rs`: `round_trip_tolerance: 0.02`, `dip_recovery_margin: 0.005`; `confirm()` returns
`held_above_breakout`, `dips_bought`, `confirmed`.

### 9.1 The cooldown is checked at candidate time, not confirmation time

This is deliberate and documented: suppressing the *candidate* means the burst of re-triggers riding
the same move never enters follow-through at all. It is the right call for noise, and it has a
measurable side effect — **the 20-trade confirmation window is a variable amount of wall-clock time.**
On a fast name, 20 prints is well under a second; on a thin one it can be minutes. Every confirmed
ignition signal therefore carries an **unrecorded and highly variable latency between the microstructure
event and the tradable alert** (§15). Nothing on the wire distinguishes a 200 ms confirmation from a
90-second one.

### 9.2 The cooldown sweep cannot have measured what it claims

`alert_cooldown_secs = 300.0`. The doc comment states 300s "cuts alert volume 5x while raising hit
rate past the 50% breakeven line", then records a re-sweep on the broad sample and says:

> *"Every cooldown from 0 to 300s leaves the sample untouched — proof the 300s gate is already
> binding — and every value ABOVE it makes things worse (450s: 36.5%, 600s: 37.1%, 1200s: 34.6%, all
> below 300s's 38.6%)."*

Read carefully, that is an admission that **the broad sample was generated with the 300s cooldown
already active**, so re-applying any cooldown ≤ 300s is a no-op on it. Values *below* 300s were
therefore **not testable on that data at all**. The conclusion "300 s is a measured optimum" is
supported only on the upper side. This is a real epistemic gap, and it is the kind of thing an
outcome dataset generated under the same 300s gate also cannot fix (§28).

### 9.3 The cooldown is per-symbol and interacts with the tiers

Cooldown state lives in the `IgnitionMonitor`, which is destroyed when a symbol leaves its tier. **A
symbol that leaves and re-enters a tier gets a fresh, empty cooldown** — and also a fresh, empty
trade history, so it must re-accumulate 20 seconds of baseline before the frequency signal can fire
again. Tier churn therefore both *resets suppression* and *blinds the detector*, in opposite
directions, for the same event.

---

## 10. The flat-base low-float gate — enabled, and almost entirely inert

```rust
FlatBaseThresholds { max_price_for_gate: 0.25, lookback_trades: 20, max_range_ratio: 0.03 }
pub fn in_gated_price_band(price: f64, t: &FlatBaseThresholds) -> bool {
    price > 0.0 && price <= t.max_price_for_gate     // price <= 0.25
}
```

The gate only ever **suppresses** (`flat_base_gate_blocks` → return `MonitorEvent::None`), and it was
switched on in `MonitorConfig::default()` on 2026-09-06 with the explicit reasoning that the doc's
headline low-float pattern "had shipped as `None` (fully off) … implemented, tested, and never
actually running in production."

**But look at the price bands of every tier that can hold an ignition monitor:**

| Tier | Price floor |
|---|---|
| Funnel | `min_price: 0.25` (`price >= 0.25`) |
| Quiet watch | `min_price: 0.25` |
| Movers | no explicit floor, but ranked from the same universe snapshots |
| Universe (opt-in) | none |

The gate applies at `price <= 0.25`. The funnel and quiet tiers admit at `price >= 0.25`. **The
overlap is the single point $0.25.** In the bounded tiers the gate is effectively inert — it can
only bite when a trade prints at or below $0.25 for a symbol admitted at ≥ $0.25, i.e. a symbol that
has ticked down through the boundary since selection.

And the pattern the gate was built for — the architecture doc's *"low-priced penny stocks as low as
~$0.15–$0.25"* — sits **almost entirely below the funnel's own `min_price` floor**. The refinement
for the headline low-float setup is gated to a price band the primary discovery path structurally
excludes.

Where it *does* run for real is tier 4, `IGNITION_UNIVERSE_MODE` (which inherits
`flat_base: Some(default)` — pinned by the test at `live.rs:1831`). So the gate's real behaviour
depends on an env var, and in that same tier the ignition detector has only one of three signals
available (§4.3).

`is_flat_base` also **fails closed on insufficient history** (`recent_trades.len() < 20 → false` →
gate blocks). A sub-$0.25 symbol whose monitor was just created therefore has its ignition
candidates suppressed until 21 prints have accumulated. In the universe tier, monitors are created
on first print and evicted after 300 s idle — so a genuinely quiet sub-$0.25 name may **never**
accumulate 21 prints inside one monitor lifetime, and would be permanently suppressed. This is the
strongest candidate in the whole audit for "the system cannot detect the thing it was specifically
built to detect."

---

## 11. Consolidation Breakout and Micropullback — one field apart

```rust
SurgeThresholds {
    lookback_candles: 5, baseline_candles: 20,
    min_move_pct: 8.0, min_volume_ratio: 3.0,
}
ConsolidationThresholds {
    min_consolidation_candles: 2,      // ← the ONLY difference
    max_consolidation_candles: 20,
    max_range_ratio_of_surge: 0.6,
    max_consecutive_invalid: 2,
}
```

```rust
// live.rs:298
pub fn micropullback_config() -> ConsolidationBreakoutConfig {
    ConsolidationBreakoutConfig {
        consolidation: ConsolidationThresholds { min_consolidation_candles: 1, ..Default::default() },
        ..Default::default()
    }
}
```

**Micropullback and ConsolidationBreakout differ by exactly one integer.** Both monitors run in
parallel on the identical candle for every tracked symbol (one shared closure, `run_consolidation`,
so they cannot drift). They emit the same `ConsolidationEvent` with different `strategy` tags, and
downstream they receive **different outcome brackets** — `Micropullback` gets `scalp` (2%/2%/10
bars), `ConsolidationBreakout` gets the `swing` default (5%/3%/20 bars) (§21).

Two things follow that matter a great deal for interpreting outcome data:

1. **Their signals are near-perfectly nested, not independent.** Any pattern with ≥ 2 consolidation
   candles satisfies `min >= 1` as well. The two strategies will co-fire on most events, offset only
   by timing and by the state machines' internal paths. Treating them as two independent strategies
   with two independent hit rates will **double-count the same underlying market events**, and any
   comparison of "which is better" is a comparison on overlapping samples.
2. **They are judged against different targets.** The same market event, seen by both, is scored a
   hit at +2% by one and requires +5% from the other. A raw hit-rate table across strategies is not
   comparing like with like.

The code's own recorded evidence (`live.rs:262` region, `tune_broad` 2026-09-03) is honest about the
thinness: micropullback 37 surges → 27 confirmed → 15 entries; default 36 → 20 → 11. It explicitly
declines to conclude anything from 11 vs 15.

Surge detection is `min_move_pct: 8.0` over 5 candles with `min_volume_ratio: 3.0` against a
20-candle baseline. Note that this is the **third** independent definition of "unusual volume" in the
system (funnel `rel_vol >= 5.0` vs full-day average; ignition `3.0×` over 1s/20s trade *rate*;
surge `3.0×` over 5/20 candle *volume*). None of them are reconciled, and none share a denominator.

### Strategy Isolation, stated in the code

`consolidation-breakout`'s header: *"Deliberately does not consume ignition_detector's alerts… it
identifies its own surge independently from the same raw bar data."* See §23 for what this principle
buys and what it costs.

---

## 12. The Halt Early-Warning system

```rust
AlertLevelThresholds {
    amber_proximity_ratio: 0.5,
    red_proximity_ratio: 0.8,
    volume_escalation_rel_volume: 3.0,
    hysteresis_margin: 0.10,
}
```

`bands.rs` implements real LULD mechanics: tiered `band_width_dollars` (sub-$0.75 uses a flat dollar
amount, above that a percentage), `luld_in_effect(utc_time)`, and `band_doubles(reference_price,
in_closing_window)`.

Distinctive properties:

- **This is the only detector that is a continuous gauge, not an edge-triggered signal.**
  `HaltWarning` is emitted per trade (throttled — §18), because a proximity meter needs its current
  value, not just transitions.
- **`luld_in_effect: false` pins `level` to `Calm` regardless of `proximityRatio`.** The
  shared-types comment is unusually sharp about the consequence: *"premarket is exactly when a big
  mover looks most interesting and is least halt-able"* — so a client must read the flag rather than
  infer quiet from a calm level.
- `estimatedBands` distinguishes real LULD bands (from the `l` message) from computed estimates.
  **This flag is on the wire and available as a data-quality discriminator** — one of the few places
  the system exports its own uncertainty.
- Halt coverage was the **first** thing to be decoupled from funnel qualification (the "FAMI case"),
  and it is the reason the movers tier exists at all.

The halt reading is consumed by the auto-trader as a **veto** (`Amber`/`Red` → `HaltRiskTooHigh`),
never as a positive signal. `relative_volume` inside the halt reading inherits §6's normalisation
gap.

---

## 13. Catalysts — keyword tagging, and a large coverage hole

`python/app/news.py`. Seven categories, plain lowercased substring matching, deliberately **not**
NLP or sentiment:

`earnings` · `fda` · `merger_acquisition` · `offering_dilution` · `halt_resumption` ·
`analyst_action` · `partnership_contract`

`fetch_recent_news(cfg, symbol, limit=10)` → Alpaca `/v1beta1/news`, **10 most recent items, no time
window.** The code is candid that false negatives are expected.

### 13.1 The recency problem, and the field that fixes it

Because there is no time window, a tag carries **no inherent recency guarantee** — a weeks-old
headline tags identically to one from four minutes ago. `mostRecentPublishedAt` (added in Milestone
B) is the only field that distinguishes them, and it is **optional**. Any analysis that treats
`catalystTags` as a "has a catalyst right now" indicator without conditioning on
`mostRecentPublishedAt` is measuring something closer to "has had news in recent memory."

### 13.2 The coverage hole

`spawn_catalyst_lookup` is called from exactly one place: `live.rs:1224`, on `actually_added` in the
**funnel promotion path**. Consequences:

- A symbol tracked only via the **movers** tier has **no catalyst data at all**, ever.
- A symbol on the **quiet** tier — the tier built for the flat-base low-float setup, where a news
  catalyst is arguably most decisive — has **no catalyst data at all**.
- A **universe-tier** symbol has none.
- The lookup runs **once at promotion and is never refreshed**, so a catalyst that publishes while a
  symbol is already tracked is never picked up. (It *will* re-fire if the symbol drops out and is
  re-promoted — so catalyst freshness is a side effect of tier churn.)

So `catalystTags` is not missing-at-random across signals: **its presence is correlated with
"arrived via the funnel"**, which per §5–6 is correlated with time-of-day and with float-budget
state. Any measured "catalyst effectiveness" will partly be measuring funnel membership.

`assess.py` (the Claude/web-search chart assessment, `claude-sonnet-5`, 10-minute TTL cache) is
**UI-only** — it never touches detection, never reaches the event wire, and is not recorded.

---

## 14. The movers leaderboards — the only real ranking, and it is not a detector

`movers.rs` runs its **own independent** universe scan (same `fetch_universe` + `fetch_snapshots`),
explicitly decoupled from the funnel: *"these are informational rankings, not detection gates, so
nothing here reads or feeds funnel state."*

- `TOP_N = 25` per list (Top Gainers by `change_pct`, Highly Trading by `volume`).
- **Rolling 24-hour best-observed reading**, not a live snapshot. `update_rolling_best` keeps each
  symbol's peak reading, tagged with the `TradingSession` it occurred in, replacing it only when
  absent, aged out of the 24h window, or genuinely exceeded.

The design intent is good: a stock that gapped hard at 05:00 premarket and cooled off stays visible
with an honest session label rather than vanishing.

### The property that matters for interpretation

`session_volume` resets to zero at each new session. So a *completed* session with high volume can
out-rank a newer, still-accumulating one until it ages out of the 24-hour window. The code calls this
out and accepts it deliberately.

But the leaderboards are not merely informational: **the movers tier is one of the four gates that
decides which symbols get detectors** (§4). So the "Highly Trading" list — whose ranking is
contaminated by cross-session volume comparison, and which shares §6's lack of intraday
normalisation — is directly upstream of detection coverage. `movers.rs`'s claim that it is
"informational, not a detection gate" is true of `movers.rs` in isolation and false of the system:
`live.rs`'s `halt_watch_ticker` diff creates and destroys momentum, ignition, halt, consolidation and
micropullback monitors from that leaderboard.

**This is the clearest instance in the codebase where Strategy Isolation holds at the module boundary
but not at the system boundary.**

---

## 15. Timing — when each decision first becomes knowable

| Decision | Data required | Earliest it can fire | Notes |
|---|---|---|---|
| Funnel Stage 1/2 (scan) | REST snapshot + 20d seed + FMP float | up to **15 s** after truth, plus float-budget queueing | scan is periodic, not event-driven |
| Funnel Stage 1/2 (live) | one 1-min bar + SessionTracker | **on bar close** | ≤ 60 s stale by construction |
| Momentum score | 1 bar (partial), **21 bars for full** | 1 min / **21 min** | `ma_slope = 0.0` until 21 |
| Consolidation surge | 5 candles + 20 baseline | **~25 min from a cold window** | backfilled at promotion (below) |
| Micropullback entry | surge + ≥1 consolidation candle + breakout | ~7+ min | |
| ConsolidationBreakout entry | surge + ≥2 + breakout | ~8+ min | |
| Ignition candidate | 20 s of trade baseline (or 25 quotes) | **~20 s** | genuinely fast |
| Ignition confirmed | + **20 further prints** | 20 s + *unbounded* | §9.1 |
| Halt warning | 1 trade + reference price | **immediate** | fastest path in the system |
| Catalyst | funnel promotion + HTTP round trip | seconds after promotion | funnel-only (§13.2) |

### 15.1 One genuinely important mitigation

At funnel promotion, `live.rs:1195` region **backfills** the momentum window and *both* consolidation
monitors from `session_bars` — but only `if window.len() == 0`. So a **newly** promoted symbol starts
with real history instead of a cold window, while a symbol re-promoted after a flap (whose window is
non-empty) does not get re-backfilled. Combined with §4.2, this means the backfill protects the
first promotion and not the churn case — which is precisely the case the YQ incident was about.

### 15.2 The scan/live split creates two different funnel verdicts

The same symbol is evaluated by Stage 1/2 twice, from two different data paths: `scan_shortlist`
(REST snapshots, `gap_pct` recomputed from `seed.prior_close`, float from FMP) and the live `Bar`
handler (`SessionTracker::on_bar`, float carried through from the scan). These use different volume
accumulations — the scan uses Alpaca's `dailyBar.v`, the tracker sums streamed bar volumes since
*its own* first bar. **A live-tracked symbol's `session_volume` is only complete if the tracker was
seeded with `fetch_session_bars` and has not missed bars.** `FunnelSignal.passed` on the wire is the
live path's verdict; the tier membership that produced it is the scan path's verdict. They can
disagree, and a disagreement is not surfaced anywhere.

---

## 16. Timestamp semantics — what "when" means on the wire

This is subtle enough to change the sign of a latency measurement, so it is worth stating exactly.

| Event | `timestamp` field carries |
|---|---|
| `FunnelSignal` | `bar.timestamp + 1 minute` (bar **close**) |
| `MomentumUpdate` | `bar.timestamp + 1 minute` |
| `ConsolidationEvent` | `bar.timestamp + 1 minute` |
| `BarUpdate` (final, 60 s) | `bar.timestamp` (bar **open**) |
| `BarUpdate` (live, 60/30 s) | `state.bucket_start` (bucket **open**) |
| `IgnitionEvent` | `trade.timestamp` (the actual print) |
| `HaltWarning` | `trade.timestamp` |
| `FunnelHealth` | `Utc::now()` (**wall clock, not market time**) |
| `CatalystUpdate` | server receive time; `mostRecentPublishedAt` is publication time |

Three consequences:

1. **Bar-derived detections and bar data are stamped one minute apart for the same bar.** Joining
   `MomentumUpdate` to `BarUpdate` on timestamp requires knowing this offset.
2. `FunnelHealth` is the only detection-path event stamped in wall-clock time rather than market
   time — it cannot be ordered against the others under replay.
3. The auto-trader relies on this: it manages positions off
   `BarUpdate { is_final: true, interval_secs: 60 }` and calls `on_bar(..., timestamp + 1 minute)`
   itself, reconstructing bar-close time from bar-open time. Two different conventions for the same
   quantity, converted in two different places.

---

## 17. The event wire — what escapes, and what dies inside the detectors

Nine `ScanEvent` variants exist (`events.rs`), mirrored exactly in
`packages/shared-types/src/index.ts`: `FunnelSignal`, `MomentumUpdate`, `IgnitionEvent`,
`ConsolidationEvent`, `FunnelHealth`, `HaltWarning`, `BarUpdate`, `CatalystUpdate` (+ handshake
frames).

**Everything downstream — every client, the auto-trader, the measurement engine, push — sees only
these.** So the following are computed and then permanently lost:

| Computed | Where | Never on the wire |
|---|---|---|
| `IgnitionSignals` (which of the three fired, `trade_frequency_ratio`, `spread_ratio`, `ask_absorbed`) | `detect.rs` | ✗ only `kind` survives |
| `FollowThroughResult.held_above_breakout` / `dips_bought` | `follow_through.rs` | ✗ only `confirmed`/`rejected` |
| Bid, ask, bid size, ask size, spread | `live.rs` Quote handler | ✗ never, in any form |
| Surge magnitude, consolidation range ratio, candle counts | `surge.rs` | ✗ only `kind` + `strategy` |
| `float_shares` (the actual number) | Stage 1 | ✗ only `floatOk: bool` |
| `avg_daily_volume`, `prior_close`, `relative_volume` | `SessionTracker` | ✗ (`HaltWarning` carries a `relativeVolume`, the funnel does not) |
| Which coverage tier a symbol came from | `live.rs` tier sets | ✗ never |
| Quiet-watch selection membership | `select_quiet_watch` | ✗ (audit log only) |
| Float budget priority score | `scan_shortlist` | ✗ |

The last two rows are the ones I would most want and cannot have. **Tier provenance is not on the
wire**, which means that for any signal in BASELINE SESSION 001, *it is not possible to determine
from the event stream which coverage tier made that signal possible.* Since the tiers have
materially different detector availability (§4.3), different catalyst coverage (§13.2), and
different price bands (§10), tier provenance is arguably the single most important missing covariate.

It *is* partially recoverable: a symbol that ever emits `FunnelSignal` was in the funnel tier at that
moment, and `data/discovery-audit` records `quiet_selected` and `qualified` per scan. So a
reconstruction is possible by joining the discovery audit to the episode stream on symbol + time. It
is not available in the episode records themselves.

---

## 18. Suppression, deduplication, and throttling — complete inventory

Every rule that stops a computed thing from being emitted or acted on. This inventory matters because
**each one is a place where a true positive can be silently removed**, and none of them are recorded
as suppression events.

| # | Rule | Value | Scope | Recorded? |
|---|---|---|---|---|
| 1 | Ignition alert cooldown | `300 s` after a **confirmed** alert | per symbol, per monitor | ✗ |
| 2 | Flat-base gate | suppresses candidates at `price <= 0.25` without a flat base | per symbol | ✗ |
| 3 | Halt-level log throttle | log only on level change | per symbol | n/a |
| 4 | `HaltWarning` send throttle | level change **or** `>= 500 ms` since last | per symbol | ✗ |
| 5 | Live-candle broadcast throttle | `LIVE_BAR_BROADCAST_INTERVAL = 500 ms` | per symbol per interval | ✗ |
| 6 | Float check per scan | `MAX_FLOAT_CHECKS_PER_SCAN = 60` | global per scan | ✓ `starvedCandidates` |
| 7 | Float daily budget | `240` requests/day | global per day | ✓ `floatBudgetRemaining` |
| 8 | Float failure cooldown | `600 s` | per symbol | ✗ |
| 9 | Watchlist miss tolerance | `MAX_CONSECUTIVE_WATCHLIST_MISSES = 2` | per symbol | ✗ |
| 10 | Quiet transition grace | `300 s` | per symbol | ✗ |
| 11 | Quiet caps | `max_symbols = 150`, `QUIET_TOTAL_CAP = 300` | global | ✗ |
| 12 | Movers `TOP_N` | `25` per list | global | n/a |
| 13 | Universe monitor cap | `6_000`, LRU by last trade | global | log only |
| 14 | Universe monitor idle eviction | `300 s`, swept every `60 s` | per symbol | log only |
| 15 | Broadcast channel capacity | `16_384` (both hops) | global | ✓ `stream_lagged` |
| 16 | Snapshot retention | `2_000` alerts, `5_000` latest-state | global | ✗ |
| 17 | Push cooldown | `IGNITION_PUSH_COOLDOWN = 15 min` | per symbol | ✗ |
| 18 | Auto-trader `AlreadyEnteredToday` | one entry per symbol per day | per symbol | ✓ journal |
| 19 | Auto-trader `MaxConcurrentPositions` | `4` | global | ✓ journal |
| 20 | Measurement queue | `QUEUE_DEPTH = 64`, `MAX_PENDING_OUTCOMES = 4_096` | global | ✓ `dropped` |

### 18.1 The asymmetry worth naming

**Rules 18–20 are recorded. Rules 1, 2, 8–11, 16, 17 are not.** The auto-trader journals every skip
with a `SkipReason`; the detectors journal nothing when they suppress. So the outcome dataset will
contain a complete record of *trades not taken* and no record whatever of *signals not emitted*.

Concretely: if the 300-second ignition cooldown suppresses a candidate that would have been the
session's best signal, nothing anywhere records that it happened. The system can measure the
precision of what it emitted; it structurally cannot measure the recall cost of its own suppression.

### 18.2 An `EventSnapshot` retention quirk

`server.rs:37`:
```rust
self.latest.insert(key, frame);                        // key = "type:symbol:intervalSecs"
while self.latest.len() > 5000 { self.latest.pop_first(); }
```
`latest` is a `BTreeMap`, so `pop_first()` evicts the **lexicographically smallest key**, not the
oldest entry. Above 5,000 distinct keys, eviction is alphabetical (`"bar_update":"AAAA"` goes first),
not LRU. Alerts (`alerts`, a `VecDeque` capped at 2,000) are correctly FIFO, and a test pins that
telemetry cannot evict retained entry alerts. This affects **client state recovery on reconnect
only** — not detection, not the auto-trader, not measurement. Worth knowing before concluding
anything from what a reconnecting client displayed.

---

## 19. Fail-open vs fail-closed — the complete inventory

The codebase is explicit and unusually consistent about this, which makes the exceptions
interesting.

**Fails CLOSED (missing data ⇒ no signal):**

| Case | Location |
|---|---|
| Unknown float ⇒ Stage 1 fails | `fast-funnel/types.rs` |
| `avg_daily_volume == 0` ⇒ `relative_volume() = None` ⇒ Stage 2 fails | same |
| Unavailable baseline after seed lookup ⇒ `avg_daily_volume = 0` | `universe.rs` scan |
| Unknown baseline in quiet watch ⇒ `rel_vol = INFINITY` ⇒ excluded | `select_quiet_watch` |
| Insufficient trade history ⇒ `is_flat_base = false` ⇒ **gate blocks** | `flat_base.rs` |
| Insufficient history ⇒ `trade_frequency_ratio = None` | `detect.rs` |
| Fewer than 25 quotes ⇒ `spread_ratio = None` | `detect.rs` |
| < 21 candles ⇒ `ma_slope = 0.0` | `scorer.rs` |
| No momentum data ⇒ `MomentumGateFailed` | `auto-trader/engine.rs` |
| New session date on a bar ⇒ **bail out and reconnect** to rebuild all state | `live.rs` Bar handler |

**Fails OPEN (missing data ⇒ proceed):**

| Case | Location | Note |
|---|---|---|
| Missing `name` ⇒ not a warrant ⇒ kept in universe | `universe.rs::is_warrant` | classification-only |
| Missing halt data ⇒ treated as **Calm** ⇒ entry allowed | `auto-trader/engine.rs` | explicit: "a data gap shouldn't silently block an otherwise-good entry" |
| Strategy missing from `enabled_strategies` ⇒ **enabled** | `auto-trader/engine.rs::try_enter` | explicit: "fails open the same direction every other data-gap case in this engine already does" |
| No metrics for a strategy ⇒ keep current enabled state | `decide_enabled_strategies` | |
| Unrecognised strategy key on disk ⇒ silently skipped | `StrategyConfigFile::decisions` | forward-compat |
| Catalyst lookup unreachable ⇒ warn, no tags, tracking proceeds | `spawn_catalyst_lookup` | |
| Unwritable research dir ⇒ capture disabled, service continues | `measurement.rs` | test-pinned |

### The one that deserves scrutiny

**Detection fails closed; the trading decision fails open.** A stock with no halt data is treated as
calm and traded. This is a coherent philosophy — do not let bookkeeping gaps veto a good entry — but
it means the halt veto (§12) has exactly the coverage of `halt_monitors`, and any symbol whose
`HaltWarning` never reached the auto-trader is traded as though it were verified calm. Since the
auto-trader is a *separate process over a WebSocket*, "no halt data" also includes "the halt event
was dropped in a `stream_lagged` gap" (§18 rule 15). A lagging auto-trader socket silently converts
halt-risk vetoes into permitted entries.

---

## 20. The Auto-Trader decision tree

Separate process. Paper only. Consumes the public wire.

### Entry triggers — exactly three

| `ScanEvent` | Strategy |
|---|---|
| `ConsolidationEvent { kind: EntryTriggered, strategy: Micropullback }` | `Micropullback` |
| `ConsolidationEvent { kind: EntryTriggered, strategy: ConsolidationBreakout }` | `ConsolidationBreakout` |
| `IgnitionEvent { kind: FollowThroughConfirmed }` | `IgnitionDetector` |

Deliberately **not** triggers: `FastFunnel` and `MomentumScorer` — both are continuous
qualifying-state streams rather than edge-triggered signals. `MomentumScorer` instead participates as
the engine's own **gate**.

### State absorbed but not acted on

`MomentumUpdate` → `momentum` map (and drives the deterioration exit) · `HaltWarning` → `halt_level`
· `CatalystUpdate` → `catalyst_tags` (recorded on the journal entry, **never gates anything**) ·
`BarUpdate {is_final, 60s}` → `last_price` + position management.

Explicitly ignored: `FunnelSignal`, the 30-second `BarUpdate` stream, `CandidateOpened`,
`FollowThroughRejected`, `SurgeDetected`, `ConsolidationConfirmed`.

### `try_enter` gates, in exact order

```
1. StrategyDisabled        — enabled_strategies lookup; MISSING ⇒ enabled (fail-open)
2. OutsideRegularHours     — classify_session(ts) == Regular AND weekday
3. HaltRiskTooHigh         — halt_level ∈ {Amber, Red}; MISSING ⇒ allowed (fail-open)
4. MomentumGateFailed      — no momentum data at all
5. MomentumGateFailed      — overall >= 0.6 AND volume_confirmation >= 0.6   ← BOTH
6. MaxConcurrentPositions  — open_positions.len() >= 4
7. AlreadyEnteredToday     — position still open for this symbol
8. AlreadyEnteredToday     — entries_today[symbol] == today
9. ZeroQuantity            — !price.is_finite() || price <= 0.0
10. ZeroQuantity           — floor(position_size_usd / price) == 0
```

Then: `thresholds = OutcomeThresholds::for_strategy(strategy)`;
`target = price × (1 + target_pct/100)`; `stop = price × (1 − stop_pct/100)`;
`max_hold_until = ts + lookforward_bars minutes`.

### Gate 5 is stricter than it looks

The engine requires `overall >= 0.6` **and** `volume_confirmation >= 0.6` **separately**.
`volume_confirmation` carries weight 0.4 in `overall`, so this is not a redundant check — it is a
second, independent hurdle on the same component, and it means a symbol can pass the 0.60 composite
while failing entry because its up/down volume split is 0.58.

Per §7.2, the code's own measured median `overall` on a **+41% gap day** was `0.46`. Requiring both
`overall >= 0.6` and `volume_confirmation >= 0.6` **at the exact moment a discrete entry event
fires** is therefore a materially tight conjunction, and I would expect `MomentumGateFailed` to be
the dominant `SkipReason` in the journal. (Stated as an expectation to be checked, not a finding —
see H3 in §27.)

Note also the **cross-strategy coupling this introduces**: the auto-trader's momentum gate makes a
Micropullback or Ignition entry conditional on the *MomentumScorer's* reading. Strategy Isolation
(§23) holds among the detectors and is deliberately abandoned at the trading layer.

### Exits

| `ExitReason` | Condition |
|---|---|
| `TargetHit` | bar close ≥ target |
| `StopHit` | bar close ≤ (trailed) stop |
| `Timeout` | `timestamp > max_hold_until` |
| `MomentumDeteriorated` | `MomentumUpdate.overall < 0.4` while a position is open **and** a `last_price` is known |

`on_bar` trails the stop **first** (so a bar that makes a new high and then reverses is judged
against the freshly trailed stop), using `highest_price_since_entry × (1 − stop_pct/100)` with the
**same bracket the position was opened under**. It can emit 0, 1, or 2 journal entries per bar.
`managed_through` guards against double-processing a bar.

Two real limitations, both documented in the code:

1. Exits are evaluated on **1-minute bar closes only**. Intrabar target/stop touches are invisible.
   A position that spikes +8% and closes +1% records a bar-close outcome, so **auto-trader P&L is a
   bar-close approximation, not an execution simulation** — it is not comparable to
   `max_favorable_pct` from the horizon machinery (§21, §25).
2. `MomentumDeteriorated` is **silently skipped when no `last_price` is known**, because
   `MomentumUpdate` carries no price. Documented as an accepted gap.

### Position sizing

```rust
DEFAULT_POSITION_SIZE_USD = 500.0;  MIN_POSITION_SIZE_USD = 100.0;
MAX_POSITION_SIZE_MULTIPLIER = 1.5; DEFAULT_MAX_CONCURRENT_POSITIONS = 4;
MIN_TRADES_BEFORE_ADAPTING_SIZE = 20;  ROLLING_WINDOW = 20;
WIN_RATE_SCALE_UP_THRESHOLD = 0.55;   // ×1.1
WIN_RATE_SCALE_DOWN_THRESHOLD = 0.45; // ×0.8
```

Adaptive sizing over a 20-trade rolling window, after ≥ 20 trades. Note the **asymmetry**: scale-up
is ×1.1, scale-down is ×0.8 — deliberately faster to de-risk than to press. Also note the window is
**pooled across strategies**, so a strategy's size is influenced by other strategies' results.

---

## 21. Outcome definitions — what "right" means, and it is not one thing

```rust
OutcomeThresholds::default()  // "swing"
    target_pct: 5.0, stop_pct: 3.0, lookforward_bars: 20
OutcomeThresholds::scalp()
    target_pct: 2.0, stop_pct: 2.0, lookforward_bars: 10
```

```rust
pub fn for_strategy(strategy: Strategy) -> Self {
    match strategy {
        IgnitionDetector | Micropullback                      => scalp(),
        FastFunnel | MomentumScorer | ConsolidationBreakout    => default(),
    }
}
```

This single function is doing an enormous amount of work, and three things about it are worth stating
plainly:

1. **Per-strategy brackets are the right idea.** The code's own history records that judging ignition
   against the swing bar gave 9.3%, and against the scalp bar 35.8% — the same signal, a 4× change in
   apparent quality, purely from the definition of success. Any cross-strategy hit-rate comparison
   that ignores this is meaningless.
2. **Micropullback and ConsolidationBreakout are near-nested signals judged against different
   brackets** (§11). This is defensible on the argument that micropullback is an "act within seconds"
   pattern — but it guarantees that the two strategies' recorded hit rates are not comparable, and
   the overlap in their underlying events means the difference between them is not an independent
   measurement either.
3. **`ConsolidationBreakout`'s bracket is explicitly an unverified guess.** The doc comment says so:
   *"an unverified starting choice … rather than a backtested one; revisit once it's been run through
   `tune_broad`."* So one of the three live auto-trader strategies is being judged, and enabled or
   disabled by evidence (§22), against a bracket nobody has validated.

### 21.1 The candid limitation the code itself flags

The scalp bracket is deliberately **not** retuned despite winners averaging +3.33% against a +2.0%
target, and the stated reason is exactly right:

> *"The log records `max_favorable_pct` but no max-adverse figure, so a different `stop_pct` cannot
> be evaluated post-hoc — changing this bracket honestly means re-running `backtest_broad` under it,
> not fitting a better-looking number to the sample already in hand."*

That is a real constraint on the historical data, and it is worth checking whether the new
measurement engine lifts it. It does, partially: `horizon.rs` records both MFE and MAE per horizon
with censoring, so the *episode* dataset can evaluate alternative stops post-hoc in a way the old
signal log could not. The auto-trader journal still cannot (bar-close only, §20).

---

## 22. Evidence-driven strategy selection (v4) — bounded self-modification

`backtest-metrics/src/strategy_config.rs`. This is the only component that changes the system's
behaviour from its own results.

```rust
MIN_SAMPLE_FOR_DECISION: usize = 100;
EXPECTANCY_MARGIN_PCT: f64 = 0.25;          // dead band, hysteresis
DEFAULT_ROUND_TRIP_COST_PCT: f64 = 0.5;     // env: ROUND_TRIP_COST_PCT
ACTIONABLE_STRATEGIES = [Micropullback, IgnitionDetector, ConsolidationBreakout];
ALL_STRATEGIES        = [.. all five ..];
default_enabled(s)    = matches!(s, Micropullback | IgnitionDetector | ConsolidationBreakout);
```

Decision, per strategy:

```
expectancy_pct = (real_expectancy_pct  OR  hit_rate*target − (1−hit_rate)*stop) − round_trip_cost
n < 100                              ⇒ InsufficientData, keep current
expectancy < −0.25 AND enabled       ⇒ NEVER auto-disable (hard rule, see below)
|expectancy| <= 0.25                 ⇒ NoChangeMarginal
```

`live_efficiency` writes `data/auto_trader_strategy_config.json`; the auto-trader re-reads it
periodically; `Engine::set_enabled_strategies` journals transitions with the `sample_size` and
`expectancy_pct` that justified them.

### 22.1 Two corrections in this file are genuinely good engineering

**The never-auto-disable rule.** A real near-miss (2026-09-05) was caught live: the system was about
to disable an already-enabled strategy. The rule now is absolute — evidence can *enable*, never
*disable*. Existing positions always keep their exit management. This is the correct asymmetry for a
system that can be wrong about its own evidence.

**The expectancy formula fix.** The crude `hit_rate × target − (1 − hit_rate) × stop` assumes every
non-hit cost exactly `stop_pct`. Most non-hits are **timeouts that resolve near flat**, so the
formula overstates losses *always in the same direction*. Measured across 4,736 backtested signals:
for `IgnitionDetector` the crude formula said **−0.91pp** while the real mean realised move was
**−0.008%**; for `MomentumScorer`, **−1.94pp** vs a real **+0.14%**. The file names this as the root
cause of the near-miss, not merely its symptom — the strategy it nearly disabled was roughly
breakeven, not decisively negative.

That is a two-order-of-magnitude bias, in a formula that gates a self-modifying system. Finding it is
the most valuable single thing in this file.

### 22.2 Where I would push back

- **`DEFAULT_ROUND_TRIP_COST_PCT = 0.5` is assumed, and the system measures the real thing and
  discards it.** Spread is computed in `detect.rs` on every ignition evaluation and never leaves the
  monitor (§17). For sub-$1 low-float names, a 0.5% round trip is likely optimistic by a wide margin,
  and it is a *constant* where the real quantity varies by orders of magnitude across the price range
  the funnel admits ($0.25–$20.00). Every enable/disable decision, and every expectancy figure
  reported to the operator, rests on it.
- **`MIN_SAMPLE_FOR_DECISION = 100` against a `±0.25pp` dead band.** For a signal with per-signal
  outcome dispersion of several percent, n=100 gives a standard error on mean expectancy on the order
  of tenths of a percentage point — i.e. **comparable to the dead band itself.** The gate is
  therefore not conservative in the way its size suggests; it will admit decisions whose sign is not
  statistically distinguishable. The never-auto-disable rule is what keeps this safe, and it is doing
  more load-bearing work than the sample gate is.
- **Pooling across strategies in adaptive sizing** (§20) means the sizing feedback loop and the
  enablement feedback loop operate on different partitions of the same trades.

---

## 23. Strategy Isolation — the principle, and what it costs

Stated in-repo and enforced by structure, not convention. `consolidation-breakout` does not read
ignition's alerts; it re-derives its own surge from the same bars. `movers.rs` does not read funnel
state. Each detector is a separate crate with its own thresholds and its own pure core.

**What it buys**, and it is not small:

- A strategy's measured performance is attributable to that strategy. No hidden coupling means a
  change to ignition cannot silently alter consolidation's numbers.
- Each detector is unit-testable without the others.
- Failure is contained: an ignition bug does not corrupt funnel output.
- It makes the whole Alpha measurement programme *possible* — you can only attribute outcomes to
  strategies if strategies are genuinely separable.

**What it costs**, precisely:

1. **No signal ever reinforces another.** An ignition confirmation and a consolidation entry firing
   on the same symbol within seconds is exactly the kind of agreement that a combined model would
   weight heavily. Here they are two independent events, and no component anywhere computes a joint
   or conditional score. There is **no confluence logic in this codebase.**
2. **Redundant, unreconciled definitions of the same concept.** Three different "unusual volume"
   definitions with three different denominators (§11). Two different session-volume accumulations
   (§15.2). Nothing reconciles them, and no single component could — reconciling them would violate
   the principle.
3. **The isolation is real at the module boundary and violated at the system boundary** — twice, in
   both cases deliberately and for good reasons:
   - `movers.rs`'s "informational" leaderboard **is** a detector-coverage gate (§14).
   - The auto-trader's momentum gate makes every strategy's entry conditional on
     `MomentumScorer` (§20).

   So the system does combine information — just only at the two points where the architecture said
   it wouldn't, and in both cases as a *veto* rather than as evidence.

---

## 24. Where information is combined — and where it conspicuously is not

**The complete list of places two or more signals are combined:**

| Combination | Form | Where |
|---|---|---|
| 4 momentum components → `overall` | fixed linear weights `.4/.3/.2/.1` | `scorer.rs` |
| 3 ignition signals → `triggered` | boolean **OR** | `detect.rs` |
| 4 funnel checks → `passed` | boolean **AND** | `filters.rs` |
| surge + consolidation + breakout | sequential state machine | `consolidation-breakout` |
| proximity + rel-vol + hysteresis → level | thresholds + escalation | `halt-detector` |
| momentum + halt + session + caps → entry | boolean **AND** of vetoes | `auto-trader::try_enter` |
| `|gap| × relvol` → float priority | product | `universe.rs` (budget only) |

**What is conspicuously absent:**

- **No learned model anywhere.** Every weight and threshold is hand-set; the only fitting that has
  ever occurred is manual threshold sweeps on backtests.
- **No cross-strategy confluence score.**
- **No per-symbol or per-regime adaptation of detection.** Thresholds are global constants. A $0.30
  low-float name and an $18 mid-cap face the identical `min_relative_volume: 5.0` and the identical
  ignition ratios.
- **No market-context conditioning.** `indices.rs` exists (56 lines) but no detector consumes an
  index or breadth reading. Whether SPY is up 2% or down 3% changes nothing about any threshold.
- **No time-of-day conditioning anywhere** (§6), despite every quantity involved being strongly
  time-of-day dependent.
- **No float-magnitude conditioning.** Float is a binary gate at 20M shares. A 2M-share float and a
  19.9M-share float are treated identically, though the whole premise of the strategy is that float
  scarcity drives the move.
- **No price-magnitude conditioning.** `gap_pct >= 10.0` is a single cut; a 12% gap and a 300% gap
  are the same boolean, and gap magnitude never reappears as a feature.
- **Catalyst tags gate nothing.** They are recorded on journal entries and never read by any decision.

The synthesis: **Stockspotter is a set of independent hand-tuned boolean gates with one linear
scorer, combined by AND at the trading layer and by OR inside the fastest detector.** Every
continuous quantity it computes — gap magnitude, float size, relative volume, surge size, spread,
score — is reduced to a boolean or discarded before any decision consumes it. This is the single most
important structural fact about how the system thinks, and it defines where predictive information is
most likely being thrown away.

---

## 25. Instrumentation — what the measurement engine actually captures

Added in Milestone A/B, deployed at this baseline, and deliberately additive (16 lines of re-exports
plus catalyst timestamp plumbing; zero deletions on strategy code).

```rust
// backtest-metrics/src/context.rs
SignalContext (schemaVersion 1); FeatureCache;
FEATURE_FRESHNESS_SECS = 120; PRICE_TRAIL_CAP = 512; PRICE_TRAIL_MAX_AGE_SECS = 400;

// backtest-metrics/src/episode.rs
OpportunityEpisode; EpisodeTracker; INACTIVITY_TIMEOUT_SECS = 300;
EpisodeCloseReason { Inactivity, Invalidated, SessionBoundary, CaptureEnded };
ResearchRank; TraderLinkage; link_trader_decisions(); EXIT_ATTRIBUTION_GRACE_SECS = 3600;

// backtest-metrics/src/horizon.rs
HORIZON_SECS = [30, 60, 180, 300, 600, 900, 1800];
TARGET_PCTS  = [2.0, 5.0, 10.0];
Observation<T> { Observed, Censored };
CensorReason { SessionEnded, CaptureEnded, InsufficientForwardData, DataGap };
MAX_GAP_SECS = 120;

// ws-server/src/measurement.rs
QUEUE_DEPTH = 64; RANKING_INTERVAL_SECS = 30; OUTCOME_WINDOW_SECS = 1800;
MAX_PENDING_OUTCOMES = 4096; MAX_PATH_POINTS = 2048;
```

`qualifying_strategy` (episode.rs:425) defines what opens an episode — and it is **broader than what
the auto-trader trades**:

```rust
FunnelSignal { passed: true }                       ⇒ FastFunnel
MomentumUpdate { qualifies: true }                  ⇒ MomentumScorer
IgnitionEvent { FollowThroughConfirmed }            ⇒ IgnitionDetector
ConsolidationEvent { EntryTriggered, strategy }     ⇒ ConsolidationBreakout | Micropullback
```

So the dataset covers all five strategies including the two the auto-trader will not act on. That is
the right choice — it means `FastFunnel` and `MomentumScorer` accumulate honest evidence without
being traded.

### What the design gets right, and it matters

- **Censoring is first-class.** `Observation<T>` forces every horizon to be `Observed` or `Censored`
  with a reason. A late-session signal whose 1800 s horizon extends past the close is *not* a
  failure, and the type system prevents aggregating it as one. This is the single most important
  correctness property in the whole measurement design — without it, every late-session signal would
  read as a loss and the analysis would conclude the system should stop trading in the afternoon.
- **MFE and MAE per horizon**, which lifts the post-hoc-stop limitation §21.1 records for the old
  signal log.
- **Ranking on a timer** (`RANKING_INTERVAL_SECS = 30`), not per event, so contemporaneous
  candidates receive stable comparable ranks — test-pinned.
- **Trader linkage is after-the-fact and time-guarded** (`EXIT_ATTRIBUTION_GRACE_SECS`), with a test
  asserting a decision predating an episode is never attributed to it.
- **Measurement provably does not alter production events** — test-pinned
  (`measurement_does_not_alter_the_events_production_sees`).
- **Bounded everywhere**, with `dropped` / `write_errors` counters exposed, so degradation is visible
  rather than silent.
- **Fail-soft**: an unwritable directory disables capture instead of taking the service down.

### Known operational properties of the capture

1. **No episode data exists for the first 30 minutes of any capture.** A closed episode enters the
   pending set and is only written after `OUTCOME_WINDOW_SECS = 1800`, so its forward returns can
   accumulate. This was diagnosed live on 2026-09-09 (research file empty for ~30 min while 73
   episode-opens and 837 episode-closes per minute were flowing). Working as designed — but it also
   means **an unclean process kill loses everything still pending.**
2. `episodes-YYYY-MM-DD.ndjson` rotates per UTC day, which is what makes scoping an export to one
   session clean.
3. Feature-group coverage is genuinely partial and varies by group. On the 53-episode startup
   verification: `openingContext` and `outcome` 53/53; `ignition` 46, `halt` 31, `catalyst` 27,
   `funnel` 27, `market` 27, `momentum` 23. Non-uniform presence is the honest shape for optional
   capture — but it means **feature availability is itself correlated with tier and timing**, and
   must be treated as a covariate, not as missing-at-random.

---

## 26. Performance claims the code itself makes — to be tested, not trusted

Collected in one place deliberately, so BASELINE SESSION 001 can be read against them rather than
through them. **Every number here is a claim I read in a doc comment. None is an audit finding, and
none has been verified in this task.**

| Strategy / parameter | Claimed evidence | Source |
|---|---|---|
| **IgnitionDetector** | 869 signals, 45 sessions, 9 symbols → **38.6%** hit vs 2%/2%; avg winner **+3.33%**; **−0.46pp** expectancy before costs | `monitor.rs`, `outcome.rs` |
| IgnitionDetector (live, later) | **5,879** live-evaluated signals → **35.6%** | `engine.rs` |
| `confirmation_trade_count` | 10 → 493 signals @ 9.3%; **20 → 316 @ 35.8%**; 40 too thin | `monitor.rs` |
| `alert_cooldown_secs` | 300 s optimal; 450/600/1200 s all worse; **< 300 s untestable on that sample** (§9.2) | `monitor.rs` |
| **MomentumScorer** | 213 signals → **10.8%** vs 5%/3%; needs > 37.5% to break even | `engine.rs` |
| Momentum threshold sweep | 0.55 → 14.9% (248); 0.60 → 20.0% (175); 0.65 → 22.7% (110) | `scorer.rs` |
| Momentum distribution | max 0.80, **median 0.46** on a +41% gap day | `scorer.rs` |
| **Micropullback** | **2** live signals ever; backtest 37 → 27 → 15 | `engine.rs`, `live.rs` |
| **ConsolidationBreakout** | **2** live signals ever; backtest 36 → 20 → 11 | `engine.rs`, `live.rs` |
| Ignition scalp vs swing | 9.3% → 35.8% purely from the outcome bracket | `outcome.rs` |
| Crude vs real expectancy | Ignition −0.91pp vs **−0.008%**; Momentum −1.94pp vs **+0.14%** (n=4,736) | `strategy_config.rs` |

### What this table says as a whole

Two of the three strategies the auto-trader is allowed to trade have **two live signals each**. The
one with real evidence has a **negative unmanaged expectancy** at its own shipped bracket. The
strategy with the largest sample after ignition (`MomentumScorer`, 213) is explicitly not actionable.

So the honest characterisation of this platform at the frozen baseline is: **one detector with a
large sample and slightly-negative unmanaged expectancy, one scorer with a large sample and clearly
insufficient edge, and three strategies with essentially no live evidence at all** — which is exactly
why the measurement engine was built, and exactly why BASELINE SESSION 001 matters.

The argument that the *managed* auto-trader trade beats the *raw* signal (trailing stop plus
momentum-deterioration exit cutting real losses below the bracket's flat −2%) is plausible and is
documented as the reason for enabling despite negative raw expectancy. It has not been demonstrated
on live data, and per §20 the journal's bar-close-only exits are not directly comparable to the
horizon machinery's MFE/MAE — so establishing it requires care, not just a P&L sum.

---

## 27. Ten falsifiable pre-outcome hypotheses

Stated **before** any outcome data has been seen. Each names the prediction, the mechanism in the
code, and the observation that would falsify it. This is the only forward-looking section.

---

**H1 — Detection density rises monotonically through the session, for reasons internal to the
detector.**
*Mechanism:* §6 — `relative_volume = session_volume / avg_daily_volume`, no time-of-day
normalisation, threshold 5.0.
*Predicts:* `FastFunnel`-opened episodes per unit time increase from open to close; the median
`relative_volume` at first qualification falls through the day; premarket funnel qualification is
rare-to-absent.
*Falsified if:* funnel episode density is flat or front-loaded across the session.

---

**H2 — Symbols with unstable tier membership systematically fail the momentum gate.**
*Mechanism:* §4.2 + §7.1 — tier exit destroys the momentum window; `ma_slope = 0.0` below 21
candles; backfill only fires when `window.len() == 0`.
*Predicts:* episodes on symbols with repeated funnel entry/exit show lower `momentum.overall` and a
higher share of `MomentumGateFailed` skips than stably-tracked symbols with comparable price action.
*Falsified if:* momentum scores are independent of tier-churn count.

---

**H3 — `MomentumGateFailed` is the dominant auto-trader skip reason, by a wide margin.**
*Mechanism:* §20 gate 5 — `overall >= 0.6` **and** `volume_confirmation >= 0.6`, evaluated at the
instant a discrete entry event fires, against a documented median `overall` of 0.46 on a strong day.
*Predicts:* `MomentumGateFailed` > all other `SkipReason`s combined.
*Falsified if:* another reason dominates, or the gate passes more often than it fails.

---

**H4 — Micropullback and ConsolidationBreakout episodes overlap heavily on the same symbol-times.**
*Mechanism:* §11 — the configs differ only in `min_consolidation_candles` (1 vs 2); the patterns are
nested.
*Predicts:* a large fraction of `ConsolidationBreakout` `EntryTriggered` events have a
`Micropullback` `EntryTriggered` on the same symbol within a few minutes; their outcome paths
correlate strongly.
*Falsified if:* the two fire on largely disjoint symbol-times.

---

**H5 — The flat-base gate suppresses essentially nothing in the deployed configuration.**
*Mechanism:* §10 — gate applies at `price <= 0.25`; funnel and quiet tiers admit at `price >= 0.25`;
universe mode is opt-in.
*Predicts:* almost no episode has an opening price at or below $0.25; the low-float flat-base pattern
the architecture doc headlines is absent from the dataset.
*Falsified if:* a meaningful population of sub-$0.25 episodes exists (which would mean either
universe mode is on, or symbols routinely tick below their admission price).

---

**H6 — Ignition confirmation latency is highly dispersed and materially long in the tail.**
*Mechanism:* §9.1 — confirmation requires 20 further prints, a trade count and not a duration.
*Predicts:* the gap between `CandidateOpened` and `FollowThroughConfirmed`/`Rejected` for the same
symbol spans milliseconds to minutes, with a right tail beyond 30 s; longer confirmation latency is
associated with worse forward outcomes (the move is over by the time the alert lands).
*Falsified if:* confirmation latency is tightly clustered, or is uncorrelated with forward return.

---

**H7 — `HaltRiskTooHigh` almost never fires, and `halt` feature coverage is well below universal.**
*Mechanism:* §19 — missing halt data fails **open** (treated as Calm); `halt_monitors` exist only for
tiers 1–2; the auto-trader reads halt state over a lossy WebSocket.
*Predicts:* `HaltRiskTooHigh` is a small share of skips, and `ctx.halt` is present on well under half
of episodes (the startup sample showed 31/53).
*Falsified if:* halt vetoes are common, or halt context is near-universal.

---

**H8 — Catalyst presence is not missing-at-random; it tracks funnel provenance.**
*Mechanism:* §13.2 — `spawn_catalyst_lookup` fires only on funnel promotion, once, never refreshed.
*Predicts:* `ctx.catalyst` is present almost exclusively on episodes for symbols that also produced a
`FunnelSignal`; `IgnitionDetector` episodes on movers-only or quiet-only symbols have no catalyst
data. Any naive "catalyst → better outcome" correlation will partly be "funnel-tracked → better
outcome."
*Falsified if:* catalyst coverage is roughly uniform across `openedBy` strategies.

---

**H9 — Signals cluster on the same symbols far more than on distinct symbols, and the 300 s cooldown
is visibly binding.**
*Mechanism:* §9 — cooldown is per-symbol and per-monitor, and starts only on a *confirmed* alert;
`CandidateOpened` and `FollowThroughRejected` are uncooled.
*Predicts:* the distribution of episodes per symbol is heavy-tailed; consecutive **confirmed**
ignition events on one symbol are almost never < 300 s apart, while candidate-opens are; symbols
whose monitors were recently recreated show confirmed pairs closer than 300 s (§9.3).
*Falsified if:* confirmed events on a single symbol routinely appear inside 300 s without an
intervening tier change.

---

**H10 — Research rank will show little relationship to forward outcome, because no ranked quantity
survives to the ranking.**
*Mechanism:* §24 — every continuous quantity (gap magnitude, float size, surge size, spread, relative
volume) is reduced to a boolean or discarded before any decision or record consumes it. Ranking
therefore operates on a feature set stripped of magnitude.
*Predicts:* `researchRank` correlates weakly with MFE at every horizon; conversely, quantities that
*were* discarded — if they can be reconstructed from `BarUpdate` (gap magnitude, realised volume,
bar range) — will predict better than the rank does.
*Falsified if:* `researchRank` shows a strong monotone relationship with forward MFE.

---

## 28. Questions BASELINE SESSION 001 structurally cannot answer

Not "would be hard to answer" — **cannot**, given how the data is generated.

### 28.1 Recall. The largest gap.

The dataset records what the system detected. It records **nothing** about what it missed. There is
no independent "what actually ran today" reference, so:

- **The system's hit rate is measurable. Its miss rate is not.** A day's biggest runner that never
  cleared Stage 2 leaves no trace in the episode stream at all.
- Every suppression in §18 except rules 6, 7, 15, 18–20 is unrecorded. **Signals removed by the
  300 s cooldown, by the flat-base gate, by the quiet caps, or by tier eviction do not exist in the
  data.**
- Consequence: *"Where could predictive information exist that Stockspotter currently ignores?"* —
  the framing question of this audit — is **not answerable from this dataset.** It requires an
  external reference set of the session's real movers, built independently of the funnel. The
  `data/discovery-audit` capture is the closest thing available (it records `selection_inputs` for
  every scan, all snapshots in $0.25–$3.00), and it is a genuine partial answer for that price band
  only.

### 28.2 Confounds that cannot be removed by analysis

| Confound | Why it cannot be removed |
|---|---|
| **Time-of-day ↔ detector sensitivity** (§6) | Sensitivity is a monotone function of elapsed session time for a fixed level of unusualness. "Signals are better later" and "the detector is looser later" are not separable within one session's data. |
| **Tier provenance is not recorded** (§17) | Tiers differ in available signals, catalyst coverage, and price band — the most important covariate is absent from the episode records. Partially reconstructible by joining the discovery audit. |
| **Float budget state** (§5.2) | On a busy morning the funnel silently runs a top-60-by-`|gap|×relvol` cut on top of its thresholds. `starvedCandidates` says *how many* were starved, never *which*. |
| **Micropullback ⊂ ConsolidationBreakout** (§11) | Their samples overlap by construction, so their measured difference is not an independent comparison. |
| **Different brackets per strategy** (§21) | Cross-strategy hit rates are not comparable and no rescaling makes them so, because the underlying signals differ in horizon as well as target. |
| **Feature coverage ↔ tier and timing** (§25) | Missing features are not missing at random; conditioning on them selects a non-random subpopulation. |
| **One session, one regime** | Every threshold is a global constant with no market-context input (§24). A single day's data cannot distinguish "this threshold is right" from "this threshold suited today." |

### 28.3 Specific unanswerable questions

1. **Would a lower ignition cooldown be better?** No. §9.2 — the historical sample was generated
   under the 300 s gate, and so is this one. Only re-running under a different cooldown can answer it.
2. **What is the real round-trip cost?** No. Spread is computed and discarded (§17). The 0.5%
   assumption that gates every enable/disable decision cannot be validated against this dataset.
3. **Does float size matter beyond the 20M gate?** No. The float value never reaches the wire (§17) —
   only `floatOk: bool`.
4. **Does gap magnitude predict anything?** Not from the funnel events — `gapPct` *is* on
   `FunnelSignal`, so partially yes for funnel-opened episodes only; for ignition/consolidation
   episodes it must be reconstructed from `BarUpdate`.
5. **Which ignition signal fired?** No. `IgnitionSignals` is not on the wire (§17). The three signals
   cannot be evaluated separately, so "is ask-absorption better than frequency-spike?" is
   unanswerable from this data at all.
6. **Are two agreeing strategies better than one?** Measurable as a correlation, but **not causally** —
   nothing in the system acts on confluence, so there is no treatment/control contrast, only
   observational overlap.
7. **Would a short/reversal strategy work?** No. `gap_ok` is signed (§5.1); down-gapping stocks are
   never tracked, so no data on them is generated.
8. **Does the auto-trader's management genuinely beat the raw signal?** Only weakly. §20 — the journal
   is bar-close-only and not comparable to the horizon MFE/MAE. The two would need to be recomputed
   on a common footing.
9. **What did the system look like at 09:30:00?** Nothing before the collector's start, and nothing
   in the first 30 minutes of any *capture* (§25) — though for this session the collector started
   2026-09-09 22:08 UTC, well before the open, so the session itself is covered from its first tick.
10. **Intrabar path.** Horizons sample `MAX_PATH_POINTS = 2048` observed prices from the event stream,
    which is trade-derived and therefore fine-grained; but the auto-trader's own outcomes are bar-close
    only. Any claim about slippage or intrabar stop-through is out of reach on the trader side.

### 28.4 What the dataset genuinely *can* answer well

Worth stating, since the section above is unrelievedly negative:

- **Precision of every emitted signal**, per strategy, at seven horizons and three targets, with
  censoring handled correctly.
- **Conditional MFE/MAE distributions** — including post-hoc evaluation of alternative stop levels,
  which the old signal log could not support (§21.1).
- **Whether `researchRank` orders contemporaneous candidates usefully** (H10).
- **Auto-trader skip-reason composition** (H3), completely — this is fully journalled.
- **Feature-availability structure** — which is a real finding in its own right, not merely a
  nuisance (H7, H8).
- **Latency distributions** for candidate→confirmation (H6) and detection→trader-decision.
- **Episode close-reason composition**, which measures how episodes end rather than how they start.

---

## 29. Dependency and data-flow map

Only the edges that carry decisions.

### 29.1 Crate dependency direction

```
fast-funnel ──┐
momentum-scorer ──┤
ignition-detector ──┤
consolidation-breakout ──┼──► market-data ──► ws-server ──► (WebSocket) ──► auto-trader
halt-detector ──┘                 ▲                                              │
                                  │                                              ▼
                          backtest-metrics ◄──────────────────────────── Alpaca paper API
                          (episode/horizon/context/outcome/strategy_config)
```

The five detector crates have **no dependency on each other** — this is Strategy Isolation as a
compile-time property, not a convention. `market-data` depends on all five and is the only place they
meet. `backtest-metrics` depends on `market-data` (for `ScanEvent`) and is consumed by both
`ws-server` (live capture) and `auto-trader` (brackets, strategy config).

### 29.2 The decision path, end to end

```
[1] Alpaca REST /v2/assets ─────► universe.rs::fetch_universe
                                       │ tradable && active && !warrant       (~12,635)
                                       ▼
[2] Alpaca REST /snapshots (chunks of 200) ─► scan_shortlist          every 15 s
        │
        ├─ fetch_daily_seeds(20d) for price_ok && gap_ok symbols
        │     └─► avg_daily_volume, prior_close  →  recompute gap_pct
        │         (missing seed ⇒ avg_daily_volume = 0 ⇒ FAILS CLOSED)
        │
        ├─ Stage 2: rel_vol >= 5.0 && gap_pct >= 10.0        ← §6 un-normalised, §5.1 signed
        │
        ├─ FloatCache: known(free) | failures(600 s cooldown) | needs_fetch
        │     └─ cap: min(60, budget_remaining of 240/day)
        │        overflow sorted by |gap_pct| × relvol, truncated  ← §5.2 the hidden ranking
        │        starved ⇒ float unknown ⇒ Stage 1 FAILS CLOSED
        │
        ├─ FMP /shares-float per allowed symbol → float_ok = f <= 20_000_000
        │
        ├─ select_quiet_watch(snapshots)   ← INVERSE profile, ranked by avg_daily_volume
        │
        └─► ScanOutcome { qualified, float_status, quiet_watch, daily_seeds, session_bars }
                 │
                 ▼
[3] live.rs rescan arm
        ├─ FunnelHealth broadcast (always, healthy or not)
        ├─ quiet tier diff  (+300 s grace, caps 150/300)   → ignition monitors only
        ├─ funnel diff (diff_watchlist, 2-miss tolerance)
        │     └─ track_symbol(): SessionTracker + momentum + ignition + halt
        │                        + consolidation + micropullback
        │        └─ if window.len() == 0: BACKFILL from session_bars   ← §15.1
        ├─ not_covered_by_other_source() → WS subscribe delta
        └─ spawn_catalyst_lookup(actually_added)   ← FUNNEL ONLY, ONCE  §13.2
                 │
                 ▼  Python qualify service
           Alpaca /v1beta1/news?limit=10 → tag_catalysts(substring) → CatalystUpdate

[4] movers.rs (independent task, own scan)
        update_rolling_best(24 h, per session) → ranked_top_n(25) ×2
                 │
                 ▼
[5] live.rs halt_watch_ticker (60 s; reset_immediately on a confirmed ignition)
        mover_tracked diff → track_symbol_for_movers()
        = momentum + ignition + halt + consolidation + micropullback
          (NO SessionTracker, NO FunnelSignal, NO catalyst)      ← §14 isolation violated here

[6] Alpaca WS stream ──► live.rs tokio::select! loop
        │
        ├─ Bar(b)   ── new ET date? ⇒ BAIL, reconnect, rebuild all state
        │   ├─ trackers[sym]      → SessionTracker::on_bar → explain() → FunnelSignal (ts + 1 min)
        │   ├─ momentum_windows[] → BarUpdate {60 s, final} (ts as-is)   ← §16 offset differs
        │   ├─ momentum_windows[] → score() → MomentumUpdate (ts + 1 min)
        │   ├─ consolidation_monitors[]  → ConsolidationEvent {ConsolidationBreakout}
        │   └─ micropullback_monitors[]  → ConsolidationEvent {Micropullback}
        │
        ├─ Trade(t)
        │   ├─ discovery_audit::receipts
        │   ├─ trackers[]     → live_bars 60 s + sub_minute_bars 30 s, 500 ms throttle
        │   ├─ halt_monitors[] → HaltWarning (level change OR >= 500 ms)
        │   └─ ignition tier lookup:
        │         ignition_monitors[]  (tiers 1–3, trades + QUOTES)
        │         universe_monitors[]  (tier 4, trades only, max_quotes = 0)  ← §4.3
        │           on_trade → [pending? accumulate 20 → confirm()]
        │                    → [resume_awaiting_first_trade? halt-lift, BYPASSES cooldown]
        │                    → detect(A || B || C) → cooldown(300 s) → flat_base gate(<=$0.25)
        │                    → CandidateOpened | FollowThroughResolved
        │
        ├─ Quote(q) → ignition_monitors[].on_quote()      ← DEAD END, never reaches the wire §17
        ├─ Status(s) → halt-lift flag (tiered, same lookup)
        └─ Luld(l)  → real bands (else estimated_bands = true)

[7] broadcast::Sender<ScanEvent>  cap 16_384
        │
        ├──► ws-server::server   → EventFrame{event_id} → EventSnapshot(2000 alerts / 5000 latest)
        │                        → every client identically; lag ⇒ stream_lagged + snapshot resend
        │
        ├──► ws-server::measurement (QUEUE_DEPTH 64)
        │       EpisodeTracker::observe(event, now)
        │         qualifying_strategy() → open/extend episode
        │         FeatureCache (120 s freshness) → SignalContext
        │         rank every 30 s → ResearchRank
        │         close: Inactivity(300 s) | Invalidated | SessionBoundary | CaptureEnded
        │         → pending 1800 s, accumulate forward path (2048 pts)
        │         → horizons [30,60,180,300,600,900,1800] × targets [2,5,10] with censoring
        │         → data/research/episodes-YYYY-MM-DD.ndjson
        │
        ├──► ws-server::push → Expo, 15-min per-symbol cooldown
        └──► discovery_audit → data/discovery-audit/*.jsonl

[8] auto-trader (SEPARATE PROCESS, public WebSocket client)
        on_event:
          MomentumUpdate  → momentum[]  + deterioration exit (< 0.4)
          HaltWarning     → halt_level[]
          CatalystUpdate  → catalyst_tags[]  (RECORDED, GATES NOTHING)
          BarUpdate{final,60} → last_price[] + on_bar() position management
          Consolidation{EntryTriggered, Micropullback|ConsolidationBreakout} → try_enter
          Ignition{FollowThroughConfirmed}                                    → try_enter
        try_enter: 10 gates in order (§20) → OutcomeThresholds::for_strategy → bracket
        on_bar:    trail stop FIRST, then Target | Stop | Timeout
        journal:   Entered | Skipped{SkipReason} | StopAdjusted | Exited{ExitReason}
                   → data/auto_trader_journal.jsonl

[9] backtest-metrics::bin::live_efficiency (offline)
        AggregateMetrics per strategy → decide_enabled_strategies(n>=100, ±0.25pp, −0.5% cost)
        → data/auto_trader_strategy_config.json → auto-trader re-reads → set_enabled_strategies
        HARD RULE: can ENABLE, can NEVER auto-disable an enabled strategy  ← §22.1
```

### 29.3 The four edges that matter most

1. **`broadcast::Sender<ScanEvent>` is the total interface between detection and everything else.**
   Nine variants. Anything not in them is unrecoverable downstream — permanently, for clients, for the
   trader, and for research. (§17)
2. **`Quote → ignition_monitors → ∅`** is the only dead-end data path in the system. Real
   microstructure information enters, is computed on, and never leaves. (§3, §17, §22.2)
3. **`movers.rs → halt_watch_ticker → track_symbol_for_movers`** is where an "informational" ranking
   becomes a detection-coverage gate. (§14)
4. **`MomentumUpdate → auto-trader gate`** is where Strategy Isolation is deliberately abandoned:
   every strategy's entry is conditional on a different strategy's score. (§20, §23)

---

## Standing confirmations

- **READ ONLY, honoured.** No source, config, threshold, strategy, instrumentation, or data file was
  modified. No research code or scripts were written. No service was restarted, no deployment
  performed, nothing committed or pushed. No VPS or production access occurred in this task.
- **Baseline verified at start and at end of the audit:**
  `ba722698af4fa2b387339eb017a2d58734f969c7` — MATCH, working tree carrying only the pre-existing
  untracked `.claude/` and `AUDIT-2026-09-09.md`.
- **No BASELINE SESSION 001 outcome data was inspected**, and none was consulted while writing this.
  No hit rates, precision, MFE distributions, ranking performance, catalyst effectiveness,
  auto-trader profitability, or missed-runner characteristics were computed or read.
- **No recommendations are made.** §27 contains hypotheses, not proposals. Nothing in this document
  asks for a threshold to change.
- Every performance number in §26 is labelled as a claim made by a code comment, not as an audit
  finding.
