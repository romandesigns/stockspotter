# Today's Shortlist

The shortlist is the default web view since 2026-10-15. It is one ranked list
of 5–10 candidates, and each one has a decision card. It replaces the need to
combine ten panels by eye. The Classic workspace is one click away in the rail.

It answers **where to look**, not **when to enter**. No detector in this
repository has shown an edge after costs (consolidation index R1, R2). The
ranking is the one result that held up (R3).

## Server: `GET /shortlist`

Source: `crates/ws-server/src/shortlist.rs`.

**The pool.** Funnel symbols with a funnel reading under 10 minutes old,
priced $1–$20 and up on the day.

**The ranking.** `gapPct × relativeVolume`. `gapPct` is the last price against
the prior close. `relativeVolume` is the volume since 04:00 ET against the
20-day average. The top 10 are returned. Symbols are excluded for being out
of price range, not up on the day, or having no relative volume. They are
counted under `excluded`, not hidden.

**What each candidate carries:**
- `levels` comes from the day's final 1-minute bars:
  - premarket high and low;
  - day high;
  - VWAP, anchored at 04:00 ET before the open and at 09:30 ET after it;
  - approximate dollar volume.

  A symbol promoted mid-session has its earlier bars backfilled once from the
  provider. `barsComplete` stays false until they arrive.
- `quote` is the latest two-sided bid/ask and the spread, from
  `market_data::quote_book`, which is fed by the live quote stream.
- `halt` gives the LULD band edges and the distance to the upper band.
- `catalyst` gives the tags and the newest headline.
- `trackRecord` gives the base rates below. It is absent when no rates file
  is loaded.

**Frozen events untouched.** The shortlist is built only from events that
are already broadcast. The frozen `ScanEvent` wire contract does not change.

## Track record: historical base rates

The engine is `crates/backtest-metrics/src/base_rates.rs`. The CLI is the
`base_rates` binary.

**Decision point.** 09:35 ET. Only sessions that meet all of these are used:
- traded between 09:30 and 09:35;
- priced $1–$20;
- up on the day;
- had a two-sided quote by 09:35.

**Profile.** Day-change bucket (0–10, 10–25, 25–50, 50%+) × relative-volume
bucket (<1, 1–3, 3–10, 10x+).

**Entry and exit.** Entry is at the ask. The close is valued at the bid, so
the spread is paid on the way in and on the way out.

**Outcome.** Whether +10% was reached first, or −5% was reached first. A
minute that touches both counts as the stop.

**Minimum sample.** A profile with fewer than 30 cases shows its count and no
rates.

To rebuild the file from cached sessions (nothing is fetched):

```
cargo run --release -p backtest-metrics --bin base_rates -- \
  --input-dir <dir of *-sip-v1.json replay files> --out base_rates.json [--note "how sessions were chosen"]
```

To deploy it, put the file where the server reads it. That is
`data/base_rates.json` relative to the ws-server working directory, which on
the VPS is the bind-mounted `data/`. You can point elsewhere with
`SHORTLIST_BASE_RATES_PATH`. The server reads the file once, at first use.

### First measurement (2026-10-15)

The 2026 cache gave 1,080 sessions and 323 observations across 122 symbols,
from 2026-03-02 to 2026-09-03. Only three profiles reached 30 cases:

| Profile at 09:35 | n | +10% first | −5% first | Median close, net |
|---|---|---|---|---|
| 50%+, 10x+ RVOL | 47 | 32% | 68% | −16.5% |
| 25–50%, 10x+ RVOL | 36 | 50% | 47% | −5.7% |
| 0–10%, <1x RVOL | 182 | 6% | 69% | −6.5% |

Even the strongest gappers lost money by the close for a 09:35 buyer after
the spread. And these rates are optimistic: most small caps in the cache were
saved because they moved.
