# Order-flow delivery: strategy, critique, and what this slice does

Baseline: `master` 7d8ebe5 (pure `order-flow` crate + default-off
`market_data::order_flow_store` ingestion hooks). Archive:
`recovered/orderflow` f294a11, which holds the remaining feature —
delivery (HTTP + WS), backfill, recorder, bar cache, halt log, offline
validator, and the web UI.

## 1. Critique of the obvious strategy

The obvious strategy is "merge `recovered/orderflow`" or "copy its files
over". Both are wrong here, and the diff says why.

`git diff HEAD recovered/orderflow` is 181 files, **+7851/-12853**. The
archive is not ahead of master; it forked before most of the current
production work and therefore *deletes* it. A merge or a bulk file copy
would regress, among others:

- `live::run_live_scan_with_cache` and the server-owned discovery
  `FloatCache` (archive reverts to a per-rescan-task cache, losing
  accepted baselines and retry budgets across reconnect);
- `daily_baseline.rs`, `rest::fetch_live_baselines`, the discovery
  freshness fixtures and the page-budget/`redirect::Policy::none()`
  hardening in `rest::daily_history_bounded`;
- `momentum-strategies` (IAC/Extreme) entirely — crate, replay binary,
  tests, client panels and CSS;
- `experimental_strategies` and its separate research socket with the
  64-primary/32-research admission split in `server.rs`;
- `offline_eval` + `offline_eval_tests` (2709 lines behind the
  `offline-eval` feature);
- signed desktop identity (`desktop-release.yml`, `DesktopReleaseControl`,
  `desktopUpdateSession`, `verifyAccess`, `AccessGate`), master platform
  builds, EAS verification, the JS advisory gate, `observation_archive` /
  `observation_offbox` and their Python tests;
- `universe_d7_tests`, `movers.rs` provenance, `moverProvenance`,
  `moverEmptyState`, `momentumPrices`, `chartChange`, `rossFivePillarsRule`.

The archive's own `order_flow_store.rs` is also **behind** the baseline's:
it has no `MAX_ENGINES`, no idle sweep, no late-day discard, no
correction/cancel counters, and `fail_backfill`/`finish_backfill` lock the
global store with the feature off.

**So the strategy is a granular, additive port.** The archive is read as a
design reference via `git show`; every change lands on modern source by
hand, and nothing modern is deleted. Where the archive and the baseline
disagree about the same function, the baseline wins and the archive's
*feature* is added on top.

## 2. What the archive got wrong, and how this slice fixes it

Each item below is a real defect in the archived implementation, not a
stylistic preference. They are the blockers this slice has to clear.

| Archived behaviour | Problem | This slice |
| --- | --- | --- |
| `Store::roll_to` advances the day from any frame's timestamp | A single garbage or far-future feed timestamp rolls the whole store and wipes every engine; a stale receipt can retro-date it | `sane_event_time` gates ingestion on a receipt-vs-feed window before day roll or sweep; rejects are counted, never authoritative |
| `note_venues` parses every raw frame under the global mutex | Enabled full-tape parse + lock per frame, unvalidated under load | Kept (correction evidence must not be missed) but documented as an explicit activation cost with a measured-throughput gate, and the summary path no longer contends with it |
| `summary()` recomputes the full summary under the global mutex per request/push | Per-connection fan-out × 4 symbols × 2 s, each rebuilding delta buckets, profile bins and large prints while the live loop waits for the same lock | `lite_summary()` serves a cached lite snapshot (TTL-bounded, built once per symbol per tick) and never serializes the chart arrays |
| Lite responses simply omit `deltaBuckets` / `largePrints` / `profile.bins`; the client re-attaches the arrays from the last full fetch | Fresh header numbers silently carry minutes-old arrays, indistinguishable from current ones | Arrays carry their own `chartArraysAsOfSecs`; `withChartArrays` stamps the carried-forward arrays with the age they actually have, and the UI labels them |
| No tape-provenance surface | `ObservationHealth` exists on the baseline but nothing reads it; a client cannot tell "uncorrected tape" from "reconciled" | `/orderflow` and the push frame carry `tape: { provenance, correctionsSeen, cancelsSeen }`; the strip shows an explicit "uncorrected tape" chip with the counts |
| Eviction/recreation is silent | A recreated engine looks like a full session | `coverage` is derived from backfill state *and* whether the entry was ever evicted/capacity-rejected; `partial` is never upgraded |
| Backfill: unbounded pages, no generation guard, no event cap | `finish_backfill` can apply history to an engine that was evicted and recreated under the same key; a busy symbol can pull an unbounded trade tape | Per-entry `generation` counter checked on completion; page cap, event cap and a per-process provider budget; `ORDER_FLOW_BACKFILL` opt-in on top of `ORDER_FLOW_ENABLED` |
| Backfill: the day budget is charged before eligibility is checked | Every full fetch of an already-backfilled symbol spends one. One chart open for half an hour drains all 64 without a single provider request, and then the symbol that needs history cannot get it | `decide` runs both switches, then the slot, then an atomic reservation, then eligibility -- and refunds the reservation when nothing starts, so the counter holds requests made. A refused budget never marks a symbol `Running` |
| Bounded historical fetch: `reqwest::Client::new()` | Follows up to ten redirects, re-sending `APCA-API-KEY-ID` and `APCA-API-SECRET-KEY` to wherever it is sent; no timeout; 300,000 rows parsed and allocated before any cap applies | A bounded budget builds its own client: `redirect::Policy::none()` (and a redirection status refused outright), a 30 s timeout, a per-page body cap enforced while the body arrives, and a row cap checked before each page's rows are kept. A repeated pagination token is refused on both paths. `Unbounded` keeps the research path's existing client and semantics |
| Recorder: unbounded file growth | This box has had a disk emergency from research output before | Per-day byte ceiling applied *inside* a sample (whole lines up to exactly what is left, never overshot by a whole universe's worth of rows), a second ceiling across everything retained (`MAX_TOTAL_BYTES`, because 512 MB x 14 days of retention is 7 GB), plus the archive's name-based retention sweep |
| Recorder: the protected-path refusal names `OBSERVATION_DIR`, `OBSERVATION_ARCHIVE_DIR`, `RESEARCH_DIR`, `AUTO_TRADER_JOURNAL` | **None of those variables exists in this codebase**, so the refusal protected nothing | The names this code actually reads, taken from their definitions: `observation::ENV_ROOT`, `observation::ENV_PREREG_PATH`, `AUTO_TRADER_JOURNAL_PATH`, `AUTO_TRADER_PAPER_LEDGER_PATH`, `DISCOVERY_AUDIT_DIR`, with a test that checks them against those definitions |
| Recorder: the overlap check compares the configured strings | `captures/../captures` does not start with `captures`, and a symlink does not start with anything, so either walks straight into a preregistered capture directory | Comparison on resolved paths: `..` refused outright, every existing ancestor required to be a real directory (not a link), both sides canonicalised; and the output file itself refused if it is a symlink |
| Recorder: any directory is written into and swept by name | A mistyped variable points the sweep at someone else's directory | The directory must be empty, already marked, or hold nothing but this recorder's own recordings; it is then marked, and the mark is re-checked at every day roll |
| Recorder: "one write per sample so a crash cannot leave a half-written line" | Not true -- `write_all` can be interrupted part-way and an append of this size is not atomic | Claim removed; readers are told to tolerate a final partial line |
| Recorder: a line is `recordedAt` + `symbol` + the summary, and nothing else. `active_summaries` returns `Vec<(String, OrderFlowSummary)>`, discarding the coverage, backfill state, engine identity, tape provenance and health that `Read` already carries to the HTTP and WS paths | A summary on its own is not reproducible evidence. The archived 2026-10-06/07 recordings (12,305 rows/199 symbols and 205,716 rows/388 symbols) are arithmetically consistent and still cannot say how much of the session a row covers, whether it came from the same engine as the previous sample, whether the tape was uncorrected, or which build wrote it — so the 265 profile-volume decreases and 320 `profileSince` changes in the Oct 6 file are uninterpretable: an evicted-and-recreated engine and a real data fault look identical | `order_flow_store::active_snapshot` returns one `ObservationSnapshot` (rows + market day + health + provenance) from a single lock acquisition, and every line carries `coverage`, `backfill`, `engineGeneration`, `enginePartial`, `tape`, `capture.health`, `capture.marketDay` and `capture.sourceCommit` beside the unchanged `recordedAt`/`symbol`/`summary`. `active_summaries` is kept, now derived from the same snapshot |
| Recorder: the file is named from this process's clock and so is everything in it | The market day rolls at 04:00 ET and the store rolls on the first frame of the new day. A sample taken in between holds the *previous* session's engines, written under today's date | A row carries `capture.marketDay`, the day the engines are actually on (`null` before any event), never restamped to the clock's day; a mismatch is logged once per day |
| Recorder: nothing identifies the build | A recording that cannot say which code produced it cannot be validated against anything | `capture.sourceCommit` from `provenance::build_commit()` — the compile-time `STOCKSPOTTER_COMMIT`, `null` when absent or malformed, never a placeholder |
| `bar_cache` is **default-on** (`BAR_CACHE_DISABLED` to turn it off) | Changes `/bars/:symbol` behaviour for the running app with no flag set, and a failed warm-up retries on every live bar | `BAR_CACHE_ENABLED` opt-in, default **off**; failed fetches enter a per-symbol retry cooldown; warm-up fetches draw on a bounded per-minute provider budget |
| `bar_cache`: a live bar renews a copy however stale it was, and a `Lagged` broadcast is skipped | Live bars arrive once a minute, so a copy nothing refreshed for 90 s is missing one. Applying the next bar marked it current again -- and the next, and the next: an incomplete history served as fresh for the rest of the session | A gap marks the entry as needing the provider (`holed`); `lookup` misses and `on_final_bar` reports a miss until a fetch replaces it. `Lagged` drops every copy, because the count never says which symbols were lost |
| `bar_cache`: `seed` replaces the copy with the provider's snapshot | A live bar that arrived during the 150-180 ms fetch is lost, and an empty provider answer wipes the copy | `seed` keeps live bars newer than the newest fetched bar, and an empty answer keeps them all |
| `bar_cache`: warm-ups are spawned and then wait for a concurrency slot | 20 per minute, each a task queued behind four slots: a bounded rate turned into an unbounded backlog of provider requests | The permit is taken with `try_acquire_owned` *before* spawning; a refused warm-up gives back its mark and the minute's allowance instead of queueing |
| Client: the push frame is checked for three fields and the HTTP body is cast | `OrderFlowStrip` dereferences `exhaustion.level`, `book.ask`, `sweeps.state`, `large`, `absorption`, `deltaBuckets` and uses `interpretation.largeTrades` as a map key. A body from another version threw inside a React render; a body for another symbol was shown against the open chart | One `decodeOrderFlowBody` for both paths: every field a consumer reads is checked (finite numbers, enum values, nested array elements), the symbol must match the one asked for, and a malformed body becomes `unavailable` |
| Client: whichever answer arrives last wins | The push is live and a full fetch takes as long as it takes, so a body in flight across two pushes rolled the header backwards and then forwards again | A summary older than the one on screen does not replace it; a late *full* body still attaches its arrays, with their own age, to what is shown |
| Client: "full" means `deltaBuckets.length > 0` | A quiet symbol's genuinely full answer reads as lite, so the client carries older arrays forward over it indefinitely | `chartArraysAsOfSecs`, which the server stamps on a full body and only a full body, with the array-contents test kept as the fallback for a server that predates it |
| Client: carried-forward arrays are carried forward for ever | A chart left open overnight draws yesterday's delta buckets and profile bins under today's numbers | Arrays further than `MAX_CARRY_FORWARD_SECS` (15 minutes) from the summary beside them are dropped, not labelled |
| Push frames are sent for subscribed symbols even with the feature off | A permanent 2-second stream of `{enabled:false, summary:null}` per symbol per connection | A subscription is answered once; the recurring tick does not run unless the store is enabled and has something to send |
| Halt log: day rolls from any status timestamp, in either direction | One stale or garbage frame wipes the day's marks and retro-dates the record; the next real status wipes them again | The receipt clock owns the calendar and only moves forward (`roll_to`); a status whose own timestamp is implausible against its frame's read time, or whose market day is not the one being recorded, is refused and counted (`rejectedTimestamps`) |
| Halt log: marks appended in feed-timestamp order, unchecked | A resume stamped before its halt is appended anyway, so "oldest first" is false and a chart draws the pair backwards | A transition that would land before a mark already held for the symbol is refused and counted (`outOfOrder`) |
| Halt log: `history()` reports whatever day the log is on | A server that saw halts yesterday and no status yet today answers with yesterday's marks, which reads as "halted today" | The day is rolled at read as well as at write, and yesterday's record is released |
| Halt log: the 64-mark and 4096-symbol bounds are silent | Past the bound `marks` no longer starts where `sinceSecs` says coverage does, and some symbols have no record at all | `droppedMarks` and `droppedSymbols` on the wire |
| Halt log: `marks()` returns a bare array | Reads as "these are today's halts", when it is only what this process saw since start-up | `marks()` returns `{ sinceSecs, marks, complete: false }`, derived from the existing 04:00 ET `market_day` calendar. No SIP sequence numbers are invented, no halt is inferred from ordinary trades or from silence — only exchange status codes via `classify_status` |
| `/orderflow` full shape on the ordinary 120/min budget | A full fetch serializes a session of buckets *and* triggers provider requests | Full shape moves to `/orderflow/:symbol/full` and is charged to `EXPENSIVE_REQUESTS_PER_MINUTE`; `/bars-batch` likewise |

## 3. Scope boundaries this slice respects

- **Untouched:** `apps/mobile`, `packages/shared-types`, root
  `package.json`/`bun.lock`, `ops/ci/*advisory_gate*`. Android stays
  independently blocked; shared/native integration needs that gate, so
  **mobile parity for the order-flow protocol is deferred**.
- The new web-only protocol's decoder and types live in
  `apps/client/src/lib/orderFlow.ts` and
  `apps/client/src/lib/orderFlowWatch.ts`. They define *only* the new
  `order_flow` frame and `/orderflow` body. No existing shared protocol
  type is altered or shadowed; `RealtimeMessage` handling in
  `useRealtimeFeed` is unchanged except that an `order_flow` frame is
  routed out before the existing switch can treat it as a detection event.
- `ScanEvent` stays frozen. Order flow and halts are process-side stores
  read on request, exactly as the baseline's `order_flow_store` comment
  argues, so `ws-server`'s preregistered observation layer is untouched.
- Every added consumer is opt-in and default-off:
  `ORDER_FLOW_ENABLED`, `ORDER_FLOW_BACKFILL`, `ORDER_FLOW_RECORD_DIR`,
  `BAR_CACHE_ENABLED`, and on the client `VITE_ORDER_FLOW`. With none of
  them set the running app behaves exactly as it does today: `/bars`
  fetches from the provider, no new background task is spawned, no push
  tick runs, and the strip does not render.
- Analytics stay descriptive and untuned. Nothing claims L2 depth from
  NBBO (the touch is labelled as the touch), nothing enters ranking, the
  momentum score, any alert, or the auto-trader — `subscribe_order_flow`
  is refused for `ClientKind::AutoTrader`.

## 4. Deliberately deferred (and why)

These are archived work this slice does **not** port:

- **Chart overlays and the 1D timeframe**: `ChartOverlays`, `LevelsRuler`,
  `ProfileOverlay`, print bubbles at price, session shading, `keyLevels`,
  `chartSessions`, `glance`, `usePrefetchBars`, `useChartContext`,
  `useChartLayers`, and the archive's 421-line `SuperChart` /175-line
  `superChartEngine` rewrite. That rewrite replaces the fit/refit and
  gap-repair path (`chartChange.ts` is deleted in the archive, `chartFit`
  replaces it) and would put chart authority and gap repair at risk for a
  feature that is not order flow. The data layer those overlays need is
  delivered here (`/halts/:symbol`, absorption levels and profile in the
  full summary); the rendering is a separate chart slice. Two of the
  endpoints therefore have **no client consumer in this slice**:
  `/halts/:symbol` (for chart halt markers) and `/bars-batch` (for chart
  prefetch). Both are cheap or switched off when nothing calls them —
  `/halts` is an in-memory read that is empty unless `HALT_LOG_ENABLED`,
  and `/bars-batch` answers 404 unless the bar cache is on.
- **The strip's four at-a-glance reads** (Breakout, Structure, Order flow,
  Risk). Three of the four are chart-derived (`keyLevels`, bar ranges), so
  they belong with the chart slice above; the strip is shipped without
  that header row rather than with a half-wired version of it.
- **`fetch_daily_candles` + `/bars-daily`**: part of the 1D chart
  feature, not order flow, and it spends provider budget with no consumer
  in this slice.
- **Mobile**: `OrderFlowCard`, `LevelsLadder`, `GlanceTiles`,
  `useOrderFlow`, `useHaltMarks`, `barsCache`, `chartHtml` halt marks.
- **Recorded-session validation runs.** `order_flow_validate` is ported,
  but running it fetches real sessions and looks at real outcomes; that
  stays an explicit per-run research decision, as the archive's own
  module comment says.

### The recordings that already exist cannot be used as evidence

Two files from the archived recorder survive outside this repository (on
the Pi, under `ignored-unique/ss-orderflow/target/orderflow-run/data/orderflow`):
`orderflow-2026-10-06.ndjson` (sha256 `309e6e90…f3e55677`, 12,305 rows,
199 symbols) and `orderflow-2026-10-07.ndjson` (sha256 `b02f1a8d…00e3e28da`,
205,716 rows, 388 symbols). Their internal arithmetic holds — sampled
window volume, delta, buy/sell ratio and share velocity are consistent
with each other — and that is all they establish.

Every row in them predates the metadata above, so for those two files
coverage, tape provenance, engine identity, process health, market day
and build commit are **unavailable, not zero**. They must not be
back-filled with defaults to make them loadable: a row stamped
`coverage: session` or `correctionsSeen: 0` because the field was missing
would be a fabricated claim about a session nobody observed. The Oct 6
file alone contains 265 decreases in a cumulative profile volume and 320
changes in `profileSince`, which with no generation or eviction context
cannot be attributed — an engine evicted and recreated under the same
symbol produces exactly that pattern, and so does a genuine data fault.
No causal or predictive conclusion is drawn from either file here, and
neither is offered as load evidence: they say nothing about throughput,
lock contention or provider cost under full-universe traffic.

## 5. Remaining activation blockers

Porting the code does not clear activation. After this slice, enabling
`ORDER_FLOW_ENABLED` in production still requires:

1. **Enabled-path load validation.** `note_venues` parses every raw
   market-data frame and takes the global store mutex while the live scan
   loop takes the same mutex per trade. The correctness tests here do not
   measure that under full-universe traffic.
2. **Cached-summary staleness budget.** The lite cache removes the
   fan-out cost but introduces a TTL; the right TTL for a real session is
   a measurement, not a default.
3. **Backfill coverage.** Quote history is fetched for a recent window
   only, so older trades are tick-rule classified. The summary's own
   unknown-volume and `coverage` fields disclose this; nothing resolves it.
4. **Provider budget interaction.** Backfill and bar-cache warm-up both
   spend the shared Alpaca allowance alongside discovery and movers. The
   budgets added here are caps, not a measured allocation.
5. **Classification efficacy.** Every read is an untuned default. No
   recorded session has been validated against outcomes, so nothing here
   is cleared to inform ranking, alerts or the auto-trader.
6. **Mobile/native parity** behind the Android advisory gate.
7. **A recording run that is actually reproducible.** The metadata is now
   written, but a build that can say which commit produced it needs
   `STOCKSPOTTER_COMMIT` set at compile time (`ops/vps/deploy.sh` does
   this; an ordinary `cargo build` does not). A recording made by an
   unstamped build is honest about being unprovable and is still
   unprovable. The two archived files from the previous recorder carry
   none of this metadata at all — see the end of section 4.

Enabling any flag is not authorized by this work, and no live efficacy is
claimed.

## 6. What was actually built

### Backend (Rust)

| File | Change |
| --- | --- |
| `market-data/src/order_flow_store.rs` | `sane_event_time` receipt-vs-feed window; `timestamp_rejected` and `backfill_discarded` counters; `Coverage`; `Read`; `lite_read` (TTL-cached, stripped) and `full_read`; per-entry `generation` and `partial`; `Store::lost`; `MAX_HISTORY_EVENTS`; `active_summaries` strips at source; `ObservationSnapshot`/`RecordedRow` and `active_snapshot`, which reads every active row **and** the shared market day, health and provenance under one lock so a recorder's context cannot describe a different moment than its numbers (`active_summaries` is now a projection of it, unchanged in shape) |
| `market-data/src/halt_log.rs` | New, and `HALT_LOG_ENABLED` opt-in. Status-only halt/resume marks on the 04:00 ET `market_day` calendar, keyed to the receipt clock and rolled forward only, with a receipt-window check on every status and a per-symbol monotonic check on every transition. Discloses `sinceSecs`, `complete: false`, `repeatedStates`, `unmatchedResumes`, `rejectedTimestamps`, `outOfOrder`, `droppedMarks`, `droppedSymbols`; bounded per symbol and in total |
| `market-data/src/live.rs` | One `Utc::now()` per socket batch passed to the order-flow hooks; `halt_log::on_status` in the status arm |
| `market-data/src/ws.rs` | `note_venues` now receives the frame's read time |
| `replay-engine/src/historical.rs` | `PageBudget`, `fetch_historical_{trades,quotes}_bounded`. A bounded budget also chooses the client: no redirects, a request timeout, a per-page body cap enforced as the body arrives, and a row cap checked before rows are kept. Repeated pagination tokens refused. The existing unbounded functions are unchanged wrappers and keep their own client and semantics; `fetch_historical_bars` is untouched |
| `replay-engine/src/bin/order_flow_validate.rs` | New. The offline validator. Nothing runs it |
| `ws-server/src/order_flow_wire.rs` | New. `body()` for lite/full, bucket trimming, `chartArraysAsOfSecs` stamping, coverage and tape on the wire |
| `ws-server/src/order_flow_backfill.rs` | New. `ORDER_FLOW_BACKFILL` opt-in on top of `ORDER_FLOW_ENABLED`, both checked before a slot or a reservation is touched; one slot, `MAX_PER_DAY` reserved-and-refunded so it counts requests made, page budgets, timeout, generation handed through. `decide` makes the admission order testable without a provider |
| `ws-server/src/order_flow_recorder.rs` | New. `ORDER_FLOW_RECORD_DIR` opt-in, per-day and total byte ceilings (applied inside a sample, whole lines only) that survive a restart, resolved-path refusal of protected and symlinked paths, directory ownership marker, name-based retention. A line is a typed `Record`: the existing `recordedAt`/`symbol`/`summary` plus `coverage`, `backfill`, `engineGeneration`, `enginePartial`, `tape` and `capture` (market day, build commit, process health), all from one `active_snapshot`, serialized with the store lock released. `append` is the single write path and takes finished lines |
| `ws-server/src/bar_cache.rs` | New. `BAR_CACHE_ENABLED` opt-in (default **off**), per-symbol retry cooldown, per-minute warm-up budget with the concurrency permit taken before spawning, gap-aware freshness (`holed`), every copy dropped on broadcast lag, and a seed that keeps live bars newer than the fetch |
| `ws-server/src/http.rs` | `/orderflow/:symbol`, `/orderflow/:symbol/full`, `/halts/:symbol`, `/bars-batch` (404 unless the bar cache is on); `/bars/:symbol` consults the cache only when it is on, and `?fresh` always bypasses it. A router-construction test guards against route overlap |
| `ws-server/src/access.rs` | `is_expensive` now covers `/bars-batch` and `/orderflow/:symbol/full` |
| `ws-server/src/protocol.rs` | `subscribe_order_flow` |
| `ws-server/src/server.rs` | Per-connection push: authenticated-only, max 4 symbols, never to the auto-trader, no recurring tick while disabled or unsubscribed; post-hello loop extracted as `serve_client` so this is testable |
| `ws-server/src/main.rs` | Recorder and warmer started (both no-ops by default) |

Nothing was deleted. The discovery cache, `run_live_scan_with_cache`,
`daily_baseline`, IAC/Extreme, `experimental_strategies` and its
admission split, `offline_eval`, desktop identity and the frozen
`ScanEvent` are all as they were.

### Web client (opt-in, default off)

`lib/orderFlow.ts` (types, `decodeOrderFlowFrame`, `withChartArrays`,
`chartArrayLagSecs`, formatters), `lib/orderFlowWatch.ts` (push + poll
rules, no React, no shared-types), `lib/useOrderFlow.ts` (hook +
`orderFlowEnabled()`), `components/OrderFlowStrip.tsx`, the `of-*` rules
in `index.css`, one `onTimeframeChange` notification out of `SuperChart`,
and the strip rendered in `ChartSlot`. `useRealtimeFeed` routes an
`order_flow` frame out before its existing switch and attaches/detaches
the subscription sender on welcome/close. `apps/mobile`,
`packages/shared-types`, the root JS manifests and the advisory gate are
untouched.

Tests: `orderFlow.test.ts` (decoding, including a case per field the
strip dereferences and per wrong-shaped value; carried-forward array ages
and their 15-minute limit; the full-shape marker; window choice;
formatting), `orderFlowWatch.test.ts` (push/poll interaction, the four
non-ready states, throttling, a delayed response racing a fresher push in
both directions, malformed and mismatched bodies, a new day invalidating
yesterday's arrays, subscription bookkeeping across reconnects and two
charts on one symbol), and `orderFlowBoundary.test.ts`, which fails if the
web-only protocol ever imports from `@stockspotter/shared-types` — the
regression that would otherwise compile and pass everything else. That
file no longer asserts the injected-fetch type annotation as source text:
`tsc -b` runs here now and checks it properly, so the string match was a
guard against reformatting rather than against regression. The
architectural assertions — no `@stockspotter/*` import in the three
web-only files, the hook as the one seam that reaches the shared fetch,
and the watcher reaching the network only through what it is given — stay.

### Switches, all default-off

| Variable | Default | What it turns on |
| --- | --- | --- |
| `ORDER_FLOW_ENABLED` | off | The ingestion store. Everything else below is inert without it |
| `ORDER_FLOW_BACKFILL` | off | Provider fetch of a symbol's earlier session |
| `ORDER_FLOW_RECORD_DIR` | unset | The NDJSON recorder |
| `ORDER_FLOW_RECORD_RETENTION_DAYS` | 14 | Recorder retention (`0` keeps everything) |
| `ORDER_FLOW_RECORD_MAX_MB` | 512 | Recorder byte ceiling per market day. Cannot be switched off, and is not the only one: a 2 GiB ceiling across every retained day is a constant in the module, with no variable to raise it |
| `HALT_LOG_ENABLED` | off | Recording halt/resume times from the status stream |
| `BAR_CACHE_ENABLED` | off | In-memory 1-minute bar copy, the warmer, and `/bars-batch` |
| `VITE_ORDER_FLOW` | off | The web strip. Without it nothing subscribes or polls |

## 7. Validation run

- `cargo test --workspace`: 51 test binaries, 0 failed. `market-data` lib
  223 passed, `ws-server` bin 494 passed (8 ignored, pre-existing),
  `order-flow` lib 62 passed, `replay-engine` lib 15 passed.
- `cargo check -p ws-server --features offline-eval --all-targets`: clean;
  the only warnings are the two pre-existing dead-code ones on master.
- `bun test` in `apps/client`: 212 passed, 0 failed.
- `bun run build` in `apps/client` (`tsc -b && vite build`): passes. This
  is the real typecheck, and it covers the tests too (they are inside the
  app's tsconfig `include`).
- `oxlint`: 10 warnings, all in files this work did not add and none in
  the order-flow files. Two of them sit in `useRealtimeFeed.ts`
  (`react(refs)` on `urlRef.current` in the return value, and
  `exhaustive-deps` on `noteGap`); both predate this change and only
  their line numbers moved.

### What those tests do and do not prove

- **Real I/O, not mocks, for the paths where the bug was in the I/O.**
  The bounded historical fetch is exercised by real `reqwest` calls
  against a localhost `TcpListener` fixture: the redirect refusal is
  proven by the fixture seeing exactly one request (the redirect target is
  never asked for), and the body cap is proven both from a declared
  `Content-Length` and from a response that declares none. The recorder's
  path rules are proven against real files, real directories and real
  symlinks, in a temp directory; the symlink cases create actual links and
  print a skip line if the platform refuses, and on this box (Windows 11,
  developer mode) they did create them and did run.
- **Process-wide state is tested in-process under a lock.** The backfill
  slot and day budget are global by design, so those tests serialise on a
  module mutex and reset the budget. They are not a concurrency proof:
  what they prove is the order of the admission checks and the arithmetic.
- **Synthetic input only.** No provider request was made to Alpaca, no
  recorded session was fetched or replayed, no flag was set anywhere, and
  nothing was deployed. The clock-dependent rules (receipt windows, market
  day rolls, staleness) are driven by injected times, not by waiting.
- **The recorder's metadata is tested as serialization, not as a capture.**
  The record tests build real `OrderFlowEngine` summaries and real
  `RecordedRow`/`ObservationSnapshot` values and assert what reaches the
  string: the coverage, backfill, generation and partial flags of the row;
  the tape counters and process health; a `marketDay` that stays the
  engines' day when the recorded-at clock has already rolled past it; a
  `null` market day when the store has none; and a `null`
  `sourceCommit` on an unstamped build, checked against
  `option_env!("STOCKSPOTTER_COMMIT")` rather than a constant. No capture
  run was performed and none is implied — nothing has written a file with
  these fields in it outside a temp directory under test.
- **Nothing here is an efficacy claim.** These tests say these code paths
  behave as described on the inputs given. They say nothing about
  throughput under full-universe traffic, about the right TTL or budget
  for a real session, or about whether any feature in this slice predicts
  anything at all. See section 5, which is unchanged by this work.

## Additional splice regression (Codex)

A delayed quote arriving after backfill begins can have an event timestamp older
than an already-observed trade. Cutting history at that quote silently erased
the trade. A regression reproduced 1,000 shares instead of the 1,100 shares
supplied by history.

The candidate correction refuses a splice when live timestamps cross the
captured begin checkpoint, go backwards, duplicate indistinguishable trade
shapes, or cannot account for the supplied historical overlap. It preserves the
live engine, reports failed/partial backfill and increments `backfillDiscarded`.
A replay with initially empty buffering also refuses an overlapping arrival
before committing. This is a conservative consistency check, not provider trade
identity or official-tape reconciliation. Normal ordered overlap retains the
existing 1,400-share conservation fixture. Equal timestamps and insufficient
history are refused rather than guessed.

The correction has local regression coverage and remains held for renewed
Claude review. No production flag or runtime was changed.

## Additional UI telemetry regression (Codex)

Review found that a missing optional tape object displayed the same tooltip as
known zero corrections. It now explicitly displays tape telemetry unavailable;
known zero and process-wide nonzero counts stay distinguishable. Empty-window
side percentages also display unavailable rather than zero. Three actual React
render regressions and the client build pass. This addition needs renewed Claude
review along with the conservative splice correction; no live flag changed.
