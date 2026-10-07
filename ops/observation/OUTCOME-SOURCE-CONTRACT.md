# Outcome source contract — `alpaca-v2-stocks-trades-v1`

**Status:** contract specified and implemented as an adapter. **No network
transport exists in this build.** Adding one is a separate reviewed change,
made only after the campaign reaches `OUTCOME_FETCH_AUTHORIZED` (capture set
closed on measurement evidence **and** an explicit authorization event).

The code is `crates/ws-server/src/observation_outcome.rs`. The preregistration
names this contract in `outcome.fetchContract`, and the freeze manifest binds
it.

## Provider and endpoint

| Item | Contract |
|---|---|
| Provider | Alpaca Market Data v2 |
| Endpoint family | `GET https://data.alpaca.markets/v2/stocks/{symbol}/trades` (historical trades, one symbol per request) |
| Feed | `feed=sip` (consolidated tape A/B/C). Any other feed is refused before I/O |
| Order | `sort=asc` |
| Page size | `limit=10000` |
| Pagination | `page_token` from the previous page's `next_page_token`, until it is `null` |
| Time format | `start` / `end` in RFC 3339, UTC, nanoseconds, percent-encoded; the URL is built deterministically, so an archive proves which request produced each body |

## When a request is allowed

- **Post-session only.** A request is refused while the session's regular
  close (NYSE calendar, including early closes) is still in the future.
- **Bounded interval.** `[start, end)` must lie inside one market session
  and span at most 16 h. The primary horizon needs `(T0, T0 + 300 s]`.
- **Firewall.** Every fetch requires an `OutcomeAccess` covering the
  session. Only a campaign in `OUTCOME_FETCH_AUTHORIZED` or `ANALYSIS_READY`
  issues one, and only for its closed set of qualifying sessions.

## Completeness semantics

A fetch is **complete** only when every page returned HTTP 200, parsed fully,
and the last page carried `next_page_token: null`. **Every** other ending is
incomplete, with its reason recorded:

- transport error;
- HTTP error;
- rate limit (429) still failing after the bounded, logged backoff;
- unparseable or partial page;
- repeated page token;
- page cap reached;
- wrong symbol;
- a trade outside the requested interval;
- trades out of time order.

An incomplete fetch produces incomplete `TradeEvidence`, which the evaluator
censors (`IncompleteResponse`). A complete, empty result is valid evidence: a
horizon with no qualifying trade is a **failure**, not censored.

## Normalisation

| Field | Rule |
|---|---|
| Price | Integer micro-dollars (`round(p × 10⁶)`); the target comparison is exact in i128 |
| Time | Exchange timestamp `t`, RFC 3339 with nanoseconds |
| Tape and conditions | `z` and `c` are preserved exactly; they are interpreted only by the SHA-bound per-tape condition policy |
| Other fields | size `s`, exchange `x` and trade id `i` are preserved in the normalised record, so its hash covers them |

## Archive requirements (`outcome-evidence-v1`)

- **Location.** One directory per `(session, symbol, interval, attempt)`,
  created without overwrite.
- **Raw bodies.** Every HTTP exchange body is stored as received: pages,
  429s, errors, and an empty body for transport failures.
- **Manifest.** Binds:
  - session, symbol, requested interval, feed, fetch contract, endpoint and
    page limit;
  - per exchange: URL, token in and out, role, HTTP status, retrieval time,
    and body SHA-256 and size;
  - the normalised SHA-256 and count;
  - `binding{conditionTableSha256, statusPolicySha256, implementationSha, preregistrationSha256}`.
- **Incomplete fetches** carry an `INCOMPLETE` marker file.
- **Loading re-derives** normalised trades from the raw pages. It also
  re-checks every body hash, the page chain and each request URL, and the
  derived completeness. A stored normalised file that does not re-derive is
  refused.
- **The loader** requires `OutcomeAccess` and the exact preregistered binding.

## What is deliberately absent

- **No HTTP client.** The adapter's `PageSource` trait has only test
  implementations.
- **No retries beyond** the bounded rate-limit backoff.
- **No fallback** to another feed or provider. A different source is a
  different contract identity and a new preregistration decision.
