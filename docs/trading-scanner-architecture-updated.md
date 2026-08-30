# Trading Scanner & Charting Platform — System Architecture

## 1. Purpose

An application that scans the market in real time for stocks matching Ross Cameron's (Warrior Trading) day trading setups, presenting only high-confidence candidates instead of requiring manual screening across thousands of tickers. The system must be fast, low-noise, and provable — every detection strategy must be backtestable against historical data before being trusted with real capital.

Core failure modes from prior versions of this app that this architecture is explicitly designed to avoid:
- **Scan speed problems** at thousands-of-tickers scale
- **Signal clutter / false positives** from filters bolted on over time without a clear separation of concerns

## 2. High-Level Design Principle

**Rust does the fast, continuous numeric work. Python does the qualitative, low-frequency work.** Rust never hands Python more than a small shortlist of already-qualified candidates. Python never touches raw tick-by-tick data at scale.

The **same Rust detection logic** is used for both live trading and historical replay/backtesting — there is only one code path, never a separate "backtest version" of the logic. This guarantees that what gets backtested is exactly what runs live.

## 3. Data Provider

- **Alpaca** — used for both live/streaming market data and historical bar + tick data for backtesting.

## 4. Core Components

### 4.1 Rust — Fast Funnel (Static + Dynamic Filtering)

Responsible for reducing the full tradable universe down to a small, high-quality candidate pool.

**Stage 1 — Static filtering** (cheap, applied first, no live data needed per-tick):
- Price between $1–$20
- Float under 20M shares

**Stage 2 — Dynamic filtering** (applied only to the reduced pool from Stage 1):
- Relative volume ≥ 5x average
- Premarket/intraday gap ≥ threshold (e.g. 10%+)

Output: a shortlist (tens of tickers, not thousands) passed downstream.

### 4.2 Rust — Bullish Momentum Scorer

Runs continuously against a rolling candle buffer per ticker. Combines four weighted factors into a single bullish confidence score:

1. **Volume confirmation** (strongest weight) — up-candle volume vs. down-candle volume; confirms real buying pressure vs. thin drift.
2. **Higher highs / higher lows structure** (second strongest) — confirms trend structure is intact.
3. **Moving average slope** — 9-period and 20-period MAs both sloping upward, price above both.
4. **Absence of rejection wicks** — no repeated rejection at resistance levels.

Only tickers crossing a high confidence threshold (e.g. 90%+) are passed downstream.

### 4.3 Rust — Ignition Detector (Explosive Move Detection)

Runs **independently** of the fast funnel and momentum scorer, and watches the **entire eligible universe continuously** — not just pre-filtered candidates — because explosive moves can occur on any stock at any point in its price action, not only on stocks that already show a prior setup.

**Detection signals (tick-level):**
- Sudden spike in trade frequency (trades/second)
- Bid-ask spread tightening aggressively
- Ask-side size being rapidly eaten through
- Halt-lift resumption moves

**Follow-through confirmation window** (to filter fake spikes / liquidity grabs before alerting):
- Price must hold above the breakout level rather than fully round-tripping
- Dips after the spike must be getting bought (absorbed), not just air-pocketing back down
- This costs a small delay (hundreds of ms to ~1s) but filters a large share of false ignitions

### 4.4 Python — Qualitative Scoring Layer

Receives only the small shortlist of already-qualified candidates from Rust. Responsible for:
- News catalyst detection/confirmation (earnings, FDA, etc.)
- Any higher-level qualitative trend/sentiment confirmation better suited to Python's libraries

Python never scans the full market and never processes raw tick data at scale — its job is strictly the final qualitative pass on a short list.

## 5. UI: Panel Structure

Signals from different strategies are **never blended into one feed**. Each is its own panel so Roman can immediately tell what kind of setup/trade he's looking at:

1. **Ross Cameron Panel** — gap-and-go setups (low float, high rel volume, price range, gap %)
2. **Bullish Momentum Panel** — confirmed momentum stocks (ranked table, four-factor score)
3. **Ignition Panel** — live alert feed of explosive moves that passed follow-through confirmation

**Ignition Panel interaction model:** starts as a simple live feed that pops new alerts as they fire, each showing a timestamp and key stats (not a ranked table initially) — this is the fastest to build and lets real usage during live sessions reveal whether a ranked/comparison view is actually needed later.

## 6. Core Charting Module (Shared/Reusable)

A single reusable charting component, built once, imported by every panel. Not a separate chart implementation per panel.

**Feature parity target:** Robinhood, TradingView, and Alpaca's own charts.

**Required features:**
- Toggleable indicators: 20 EMA, MACD, Volume Profile
- Drawing tools
- Multiple timeframes
- Designed as a self-contained module so any future chart improvement automatically benefits every panel

## 7. Replay / Backtest Engine

**Engine language: Rust.** Reuses the exact same detection logic (fast funnel, momentum scorer, ignition detector) as the live system — replay is not a separate implementation, it's the same code fed historical data instead of a live stream.

**User flow:**
1. Select a specific ticker
2. Select a date/time range
3. Replay plays back on the same core chart module, tick-by-tick or candle-by-candle, exactly as if watching a live session
4. Indicators and any detection signals (ignition alerts, momentum panel qualifications, etc.) render on the chart at the exact moments they would have fired live

**Playback controls:**
- Adjustable speed — fast-forward through flat/uneventful periods, slow down around signal events
- Not fixed to real-world pace

**Architecture note:** The replay engine feeds historical data into the same live-data pipeline interface the chart already expects, so the chart itself doesn't need to know whether it's receiving live or replayed data.

## 8. Backtesting Methodology

Purpose: **prove real accuracy** of each detection strategy against historical data before trading it live with real capital.

Process:
1. Replay historical data (bar + tick level from Alpaca) through the live detection code path
2. For every signal fired (Ross Cameron setup, momentum confirmation, ignition alert), log the actual subsequent price action
3. Compute per-strategy metrics:
   - Hit rate (% of signals that led to a continued favorable move vs. round-tripped/failed)
   - Average move size on winners
   - Timing accuracy (how early into the move the signal fired)

This is the core validation loop — the system isn't trusted with live capital until its logged historical hit rate and move-size stats are known.

## 9. Data Granularity Notes

- **Tick data** (every individual trade, with price/size/timestamp) is required specifically for the **ignition detector**, since it depends on seeing trade-by-trade frequency and behavior — minute-level candles would completely mask a fast spike-and-fade within a single bar.
- **Candle/bar data** is sufficient for the Ross Cameron panel and the bullish momentum panel.

## 10. Build Order (Suggested)

1. Rust fast funnel (static + dynamic filtering) against Alpaca live data
2. Rust bullish momentum scorer
3. Rust ignition detector + follow-through confirmation
4. Python qualitative scoring layer (catalyst/news)
5. Core reusable charting module with toggleable indicators
6. Panel UIs (Ross Cameron / Momentum / Ignition) built on the charting module
7. Rust replay engine (reusing live detection code) + chart replay integration
8. Backtest logging & metrics reporting