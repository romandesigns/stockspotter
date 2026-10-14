# Order flow / microstructure: data capability audit

**Date:** 2026-10-06
**Branch:** `feature/order-flow-20261006` (based on `release/begun-window-20261004-r1` @ `1a57989`)
**Scope:** Section 3 of the order-flow specification. Nothing was implemented, subscribed to, or purchased.

## How this audit was done

Every row marked **code** was read from this repository at the commit above.
Rows marked **docs** are Alpaca's documented behaviour as I know it and were
**not** re-checked against the live API today: no request was sent to Alpaca,
because live fetches in this project need explicit authorisation each time.
Confirm the **docs** rows before relying on them.

**Update, same day.** Several **docs** rows were afterwards confirmed
against the copy of Alpaca's documentation already snapshotted in this
repository on 2026-09-28
(`ops/observation/metadata/alpaca-doc-real-time-stock-pricing-data-20260928.md`
and the `alpaca-conditions-trade-tape*-20260928.json` tables): the trade
fields `i`, `x`, `z`; the quote fields `ax`, `bx`, `c`, `z`; nanosecond
RFC 3339 timestamps; automatic delivery of corrections and cancel/errors
with a trades subscription; and that quote sizes are in **round lots**,
not shares. Rate limits, the absence of equity depth, and OTC entitlement
are still unverified here.

## Summary

- There is one market-data provider: **Alpaca, SIP feed**, over a single
  WebSocket connection plus REST.
- **Trades and top-of-book (NBBO) quotes are available** and already
  streaming. That is enough for trade classification, delta, cumulative
  delta, velocity, large-trade intensity, price impact and trade-based
  absorption.
- **There is no Level II / depth data**, live or historical. The liquidity
  heatmap, multi-level liquidity consumption and multi-level replenishment
  cannot be built from this feed. Per the specification, those stop here.
- **Aggressor side is not supplied.** It must be inferred from the quote
  prevailing at each trade.
- **Quotes only stream for funnel-tracked symbols**, not the whole market.
  A universe-only symbol has trades but no quotes, so its trades can only be
  classified by the weaker tick rule.
- Several fields that order flow needs are **received and then dropped** by
  the current parser (trade ID, exchange, tape; quote exchanges and
  conditions). Capturing them is a parser change, not a new subscription.

## Capability matrix

| Capability | Available | Provider | Resolution | Historical | Source | Notes |
|---|---|---|---|---|---|---|
| Trades (price, size, time) | Yes | Alpaca SIP | Per print | Yes, REST | code | `market_data::Trade`. Full market via `trades: ["*"]`. |
| Trade conditions | Yes | Alpaca SIP | Per print | Yes | code | `Trade.conditions`. Codes differ by tape (CTA vs UTP). |
| Bid / ask price (NBBO) | Yes, tracked symbols only | Alpaca SIP | Per quote update | Yes, REST | code | `market_data::Quote`. Not subscribed for universe-only symbols. |
| Bid / ask size (NBBO) | Yes, tracked symbols only | Alpaca SIP | Per quote update | Yes, REST | code | Top of book only. |
| NBBO | Yes | Alpaca SIP | Per quote update | Yes | docs | The SIP quote stream is the consolidated best bid/offer. |
| Level I | Yes | Alpaca SIP | Per update | Yes | code | Trades + NBBO quotes. |
| Level II / depth | **No** | — | — | **No** | docs | Alpaca offers no equity depth-of-book. |
| Exchange / venue of trade | Sent, **dropped** | Alpaca SIP | Per print | Yes | docs | Field `x`. Not in `market_data::Trade`. |
| Exchange of bid / ask | Sent, **dropped** | Alpaca SIP | Per quote | Yes | docs | Fields `bx`, `ax`. Not in `market_data::Quote`. |
| Quote conditions | Sent, **dropped** | Alpaca SIP | Per quote | Yes | docs | Field `c` on quotes. |
| Tape | Trades/quotes: **dropped**. Statuses: kept | Alpaca SIP | Per message | Yes | code | `Status.tape` exists; `Trade` and `Quote` have none. Needed to read condition codes correctly. |
| Trade ID | Sent, **dropped** | Alpaca SIP | Per print | Yes | docs | Field `i`. Unique per exchange and day, not a global sequence. |
| Sequence numbers | **No** | — | — | — | docs | No consolidated sequence number. Ordering is by timestamp plus arrival order. |
| Trade timestamps | Yes | Alpaca SIP | Nanosecond (RFC 3339) | Yes | code + docs | Parsed as `chrono::DateTime<Utc>`. The ignition detector reduces them to seconds (`to_secs`); order flow must not. |
| Aggressor side supplied | **No** | — | — | — | docs | Must be inferred (quote rule, then tick rule). |
| Corrections / cancels | Sent, **dropped** | Alpaca SIP | Per event | Flagged in REST | code + docs | Fall into `AlpacaMessage::Other`. Docs say they arrive automatically with a trades subscription. |
| Trading status (halts) | Yes | Alpaca SIP | Per event | **No** | code | Live only. Replay infers resumptions from condition `5`. |
| LULD bands | Yes, tracked symbols | Alpaca SIP | Per event | No | code | `AlpacaMessage::Luld`. |
| 1-minute bars | Yes | Alpaca SIP | 1 min (+ updates) | Yes | code | 30-second bars are built locally from trades and have no history. |
| Historical trades | Yes | Alpaca REST | Per print | Yes | code | `replay_engine::fetch_historical_trades`, 10,000 per page. |
| Historical quotes | Yes | Alpaca REST | Per quote | Yes | code | `replay_engine::fetch_historical_quotes`. |
| Historical depth | **No** | — | — | — | docs | Does not exist. |
| Premarket / after-hours | Yes | Alpaca SIP | Same as regular | Yes | code + docs | Extended hours 04:00–20:00 ET. Code already handles premarket volume. |
| OTC | Not configured | Alpaca (separate feed) | — | — | docs | Needs its own feed and entitlement. Out of scope. |
| Delay | None (realtime) | Alpaca SIP | — | — | docs | SIP is realtime on a paid plan. |

## Limits that shape the design

- **One WebSocket connection.** `ws.rs` documents a "connection limit
  exceeded" failure when a second connection is attempted. The order-flow
  engine must consume from the existing `market-data` live loop; it cannot
  open its own stream.
- **Full-market quotes are deliberately off.** `subscribe_all_trades` takes
  trades and statuses only, because the quote tape is roughly ten times the
  trade tape. Order-flow features that need quotes therefore cover
  funnel-tracked symbols only.
- **Trade and quote messages are not ordered against each other.** They
  arrive in separate batches on one socket. Classification has to match each
  trade to the quote prevailing at its timestamp and lower its confidence
  when the two are close together in time.
- **NBBO sizes are round-lot quotes.** Odd-lot trades often print inside
  the spread, so "inside spread" will be common on low-priced names and
  needs the midpoint/tick fallback with reduced confidence.
- **REST rate limits and symbol limits** depend on the account plan and are
  not recorded anywhere in this repository. (docs: unverified.)
- **Nothing persists quotes today.** Trades reach the observation receipts
  writer; quotes are held only in each ignition monitor's in-memory window.

## What each specified feature can use

| Feature (spec section) | Feasible now | Depends on |
|---|---|---|
| Trade classification (5) | Yes, tracked symbols. Tick rule only elsewhere | Trades + NBBO quotes |
| Delta, delta ratio, cumulative delta (6) | Yes | Classification |
| Trade / share / dollar velocity (7) | Yes, any symbol | Trades only |
| Large trade intensity (8) | Yes | Trades (+ classification for buy/sell split) |
| Absorption (9) | Yes, trade-based | Classification + price displacement |
| Exhaustion (10) | Yes, research only | Velocity + delta |
| Sweep detection (11) | Partial | Needs trade exchange (`x`) and the intermarket-sweep condition; both require the parser change |
| Liquidity consumption (12) | Top of book only | NBBO size. No levels beyond the best bid/ask |
| Liquidity replenishment (13) | Top of book only | NBBO size refill at a held price. Weak evidence; label as "possible" |
| Price impact / efficiency (14) | Yes | Classification + price displacement |
| Bubble overlay (21) | Yes | Classified trades |
| Liquidity heatmap (22) | **No** | Requires depth history, which does not exist |
| Volume profile (23) | Yes | Trades |

## Data already ingested but discarded

These arrive on the existing subscription and are thrown away by the parser:

1. Trade `i` (ID), `x` (exchange), `z` (tape).
2. Quote `bx`, `ax` (exchanges), `c` (conditions), `z` (tape).
3. Trade corrections and cancel/error messages.
4. Sub-second timestamp precision, wherever a consumer converts to seconds.

Keeping items 1–3 costs no new subscription and no new provider.

## Stopped here, as the specification requires

- **Liquidity heatmap (22):** not buildable. No depth data.
- **Multi-level consumption and replenishment (12, 13):** only a
  top-of-book version is possible, and it should be named as such.

Everything else can proceed as observational research features on
funnel-tracked symbols.
