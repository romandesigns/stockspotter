# Trading Scanner App — Specification (Updated)

## Overview
A stock scanning and charting platform based on Ross Cameron / Warrior Trading day trading strategies. Goal: surface high-accuracy trade candidates instead of requiring manual screening, fixing the scan-speed and false-positive/clutter problems of prior versions.

## Architecture
- **Rust**: fast scanning engine, handles the full ticker universe, tick-level data, and all detection logic (funnel filtering, ignition detection, halt early-warning detection, backtest/replay engine).
- **Python**: qualitative scoring layer only. Consumes shortlists/signals that Rust has already produced (news catalyst confirmation, trend confirmation, momentum scoring).
- **Data provider**: Alpaca for live/streaming and historical (tick + bar) market data. FMP (Financial Modeling Prep) considered as a supplementary source for fundamentals/news/catalyst data to feed the Python scoring layer — not a replacement for Alpaca, which stays the backbone for price/tick data and any future live trade execution.

## Design Principle: Strategy Isolation
Every detection system below is **independent** — each runs its own logic against raw market data and fires into its own panel. None of them read from, gate, or modify another's output. A ticker can legitimately appear in multiple panels at once (e.g. a low-float stock approaching its halt threshold while also matching the flat-base ignition pattern) — that's expected and useful, not a conflict. The only shared surface is upstream raw data (price/tick/volume feeds); no strategy's detection logic depends on another strategy's state or output.

## Core Scanning Pipeline
1. **Fast funnel (Rust)**: filters the full ticker universe on static criteria (price range, float), then checks dynamic criteria (relative volume, premarket gap) on the reduced list.
2. **Shortlist hand-off to Python**: qualitative scoring — news catalyst, trend confirmation.
3. **Bullish momentum score** combines, in order of weight:
   - Volume confirmation (up-candle volume vs. down-candle volume) — strongest signal
   - Higher-highs / higher-lows structure — second strongest
   - Moving average slope (9 and 20 period)
   - Absence of rejection wicks at resistance

## Ignition Detection (independent system)
- Runs independently in Rust, watching **tick-level trade data across the entire eligible universe** — not just pre-filtered candidates — since explosive moves can happen on stocks with no prior setup.
- On detecting a sudden price surge, does **not** alert immediately. Waits through a brief **follow-through confirmation window**: price must hold (not round-trip back down), and dips must actually get bought.
- Once confirmed, fires an alert into its own dedicated live feed panel (timestamp + key stats), kept separate from other panels.
- Ignition panel starts as a simple live feed of new alerts (not a ranked table); iterate based on real usage.

### Refinement: low-float flat-base ignition (additive layer, does not alter base ignition logic)
- Repeated observed pattern: low-priced penny stocks (as low as ~$0.15–$0.25) that trade flat and quiet for a period, then suddenly ignite and run sharply.
- Add this as an **additional qualifying condition**, checked before firing an ignition alert for stocks in this price range: confirm the stock has been in a tight, low-volatility range for some minimum lookback period immediately before the surge.
- **Isolation note**: this check only applies to, and only tightens, alerts for stocks matching the low-price profile. It must not change ignition detection behavior for any other stock. It runs as an extra gate inside the ignition system, not a separate system.

## Panels
Signals must not blend together — separate panels by strategy type:
1. Ross Cameron gap-and-go setup panel
2. Confirmed bullish momentum panel
3. Ignition / explosive-move alert panel (includes flat-base low-float pattern as a sub-condition)
4. Halt early-warning panel (new — see below)

## Post-Ignition Entry Strategy: Consolidation Breakout
Rather than chasing the peak of an ignition move or waiting for a full pullback (which may never come on low-float names), watch for a **brief consolidation** after the initial surge and enter on breakout from that range.

Detect consolidation via three conditions checked after an ignition alert fires:
1. **Volume contraction**: each pullback/consolidation candle shows declining volume vs. the surge candles and vs. the prior consolidation candle.
2. **Range tightening**: candle high-low range shrinks compared to the ignition move; several candles cluster in a narrow band rather than making new lows.
3. **Price holding above a defined support level**: e.g., the 9-period moving average, or the low of the ignition candle itself.

**Entry trigger**: the first candle that breaks back above the high of the consolidation range, once all three conditions above have been holding.

This needs backtesting to establish real hit rate and average move size — the pattern is mechanically well-defined to detect, but its predictive reliability varies by stock quality (see Price Floor below).

## Price Floor Decision
- Stocks priced above roughly **$1.50** tend to give more reliable signals for the consolidation/breakout entry strategy — tighter spreads, less manipulation, more textbook technical behavior.
- Below that, especially down toward **$0.15–$0.25**, spreads widen and price action is more prone to false breakouts/fakeouts even when volume/range signals look technically correct.
- **Decision**: not willing to fully exclude the low end. Practical price floor for scanning = **$0.25** (not $1.50), acknowledging the sub-$1.50 range is higher risk/less reliable but still in scope, especially for the ignition/flat-base pattern above.

## Halt Early-Warning Detection (new, independent system)
**Goal**: identify stocks a few seconds *before* they potentially get halted — an early warning based on proximity to the exchange's actual halt threshold, not a reaction after the halt occurs.

### Mechanism — LULD (Limit Up-Limit Down) thresholds
The relevant real-world rule: an exchange halt is triggered when price moves outside a band around a continuously updating reference price (the average trade price over the trailing 5 minutes, which only updates once the new average is at least 1% away from the current one).

Band width depends on price tier, for Tier 2 securities (the vast majority of tickers, i.e. non S&P 500 / Russell 1000 names):
- **Above $3.00**: 10% band
- **$0.75–$3.00**: 20% band
- **Below $0.75**: lesser of $0.15 or 75% of the reference price (in practice, for stocks in the $0.15–$0.25 range this collapses to essentially the flat $0.15 move)

Bands **double** during the last 25 minutes of the trading day (3:35–4:00 PM ET) for Tier 1 stocks and Tier 2 stocks at or below $3.00.

### What to build
1. A continuously updating 5-minute rolling reference price per stock (same methodology the exchange uses).
2. A lookup table mapping price → applicable band percentage (including the sub-$0.75 flat-dollar-amount special case).
3. A time-of-day check to double the band during the 3:35–4:00 PM ET window where applicable.
4. A live proximity calculation: current move (from reference price) as a percentage of the applicable threshold.

### UI concept
Dedicated panel, one card per candidate stock, showing:
- Ticker and current price
- Live percent-move (ticking in real time)
- A proximity gauge showing how close that move is to the stock's actual halt threshold
- Relative volume, shown alongside price move
- Color escalation (calm → amber → red) based on **both** proximity to threshold and volume strength together — a fast move on weak volume should be flagged differently than the same move backed by heavy volume, since volume-backed moves are more trustworthy.

### Isolation note
This is a fully separate detection system from ignition and momentum — it only reads raw price/volume ticks and the LULD threshold table. It does not gate or get gated by any other panel's logic. A stock can appear here and in the ignition panel simultaneously; that overlap is informative, not a bug.

## Backtesting / Replay
- Replay historical Alpaca tick and bar data through the **same detection code** used live (ignition watcher, momentum scorer, halt early-warning detector) — replay and live must never drift into separate code paths.
- Replay engine runs in Rust; Python's role stays limited to consuming qualified signals.
- Log actual outcomes per signal (hit rate, move size, timing) to measure real accuracy before trading live.
- Core goal of backtesting: prove real accuracy of each detection strategy against historical data before risking capital.

## Charting
- Reusable core charting module shared across all panels, feature-comparable to Robinhood/TradingView/Alpaca charts.
- Toggleable indicators: 20 EMA, MACD, volume profile.
- Drawing tools, multiple timeframes.
- **Chart replay feature**: select a stock + date/time range, replay historical price action on the same core chart component as if watching live, with indicators and detection signals rendering as they would have fired in real time.
- Adjustable playback speed in replay mode — skip quickly through flat/uneventful periods, slow down around interesting moves.

## Open Items
- Halt-and-reversal *post-halt* behavior (what happens after the halt lifts and trading resumes) is still undesigned — the spec above only covers the pre-halt early-warning detection, not a post-reopen strategy.