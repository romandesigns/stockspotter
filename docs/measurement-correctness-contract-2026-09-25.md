# Measurement correctness contract — D3–D7 (2026-09-25)

Written BEFORE any implementation, as the P2 assignment requires, so that
fixing the code cannot silently change what the research data means. Each
defect was traced read-only against `119b9a4` (= production backend
`7e36586` for every OI/measurement file) and confirmed against the
preserved 09-21..09-24 evidence.

## Decisions (read these first)

| Defect | Decision this assignment | Reason |
|---|---|---|
| D3 stale pre-detection baseline | **IMPLEMENT** | Contained in the research path (`backtest-metrics` feature cache); root cause exact; testable. |
| D7 premarket session volume | **SPLIT.** D7a (research rows: `features.funnel`/`features.market` carried across market days with no freshness) **IMPLEMENT** with D3. D7b (live universe snapshot uses a stale `dailyBar` premarket, which decides FastFunnel/quiet-watch/movers/halt coverage) **DESIGN ONLY — decision returned to GPT/user.** | D7b changes which symbols live detectors track premarket. That is production detector coverage, not measurement; it needs its own preregistered note, an API-budget bound and a premarket soak. |
| D4 disposition never set | **IMPLEMENT** | Closes already exist in the engine and are thrown away; plumbing + outcome-v2. |
| D5 opportunity ≈ symbol-day | **DESIGN ONLY (stop condition §6.5)** | Correct unit is "one causal move/setup", but the evidence-rule table and `T_move` must be preregistered, V2's preregistered inputs change meaning, and `model_freeze` byte-identity breaks by design. Next session is NOT qualified for opportunity-lifecycle research. |
| D6 rank cohort cap 4,096 | **IMPLEMENT** | Cap was never derived; binding it to open capacity (16,375) makes truncation structurally impossible at 0–1 ms/window at today's peak. |

### Market day (shared by D3 and D7)

`market_day(t) = (t in America/New_York − 4h).date()` — the market day
starts at **04:00 ET**, DST-aware via `chrono-tz` (already a dependency).
20:00–04:00 ET belongs to the previous market day. Resets fire on
market-day *inequality*, so weekends and holidays need no special case.
UTC `sessionDate` is KEPT for file partitioning, `OpportunityId`,
`EpisodeId` and the retention registry (changing it would break identity
with every artifact and the retention contract); `marketDay` is added as a
separate field. In EST the final after-hours hour of a market day lands in
the next UTC file.

### Not changed here (recorded, separately versioned later)

- The UTC-date lifecycle close of opportunities/episodes (splits after-hours at 19:00 ET in winter) — belongs with D5.
- Hard-coded 20:00Z session close for outcomes (`opportunity_outcomes.rs:65`), 13:30–20:00Z label windows (`alpha/labels.rs`), `oi_outcome_replay.rs:99` — wrong in EST and on early closes; changes outcome labels, so it gets its own version.
- The first-minute detector-state leak at 04:00 ET (`live.rs:650-652`, old run processes new-day trades until the first new-date bar) — detector boundary, not measurement.
- Idle-deadline re-arm defect (`live.rs:623`) — ingestion behaviour.

### Historical data is immutable

09-21..09-24 artifacts (schema 2, `opportunity-outcome-v1`,
`oi-cfg-b4f21c8b311a1b99`) remain evidence of the OLD contract. Nothing here
rewrites them; readers must distinguish versions. Corrected quantities
reconstructed offline from discovery tapes are DIFFERENT quantities and must
carry their own names.

### Governance consequence

Any of D3/D4/D6 moves the OI config fingerprint and/or pinned
`expected*` values, which changes the qualification spec SHA pinned in
`ops/qualify/session.sh`. The frozen `alpha-qualification-v3` contract must
be deliberately re-bound before the next designated session.

## D3 — market-day-scoped pre-detection baseline

**Current behaviour.**
- Per-symbol `first_observed`, `session_low`, `price_trail`, the ignition and consolidation counters, and the funnel and market groups live for the whole process lifetime.
- Only a process restart resets them.
- `firstObservedAt` falls back to `detectedAt` when unknown.
- `last_price` and `price_Nm_before` can return a prior day's price.

**Why incorrect.**
- `moveBeforeDetectionPct` is meant to answer "how much of *this session's* move preceded us" (`context.rs:334-347` doc). It instead measures against an arbitrary historical day, whichever day the process first saw the symbol.
- That corrupts:
  - regime assignment;
  - EarlyQuality (0.15 weight) and Continuation (0.35 weight) V1;
  - V2 A1 and A2.
- Its value depends on process uptime, so two identical market days produce different features. Replay from a day file cannot reproduce live.

**Desired behaviour.**
- All day-scoped per-symbol state is keyed by `market_day(event_time)`. The first event for a symbol whose market day is later than the stored one resets that state before it is folded in.
- Day-scoped state:
  - `first_observed`
  - `session_low`
  - `price_trail`
  - ignition and consolidation counters
  - `funnel`
  - `market`
  - `halt` (it has its own freshness filter anyway)
- `momentum` and `ignition` are already gated by 120 s freshness.
- `catalyst`: flag for D11. Recommend day-scoping too, or at minimum recording `marketDay`.
- An event whose market day is *earlier* than the stored one (for example a late UpdatedBar correction) must not reseed or lower the baseline. It is ignored for day-scoped fields.
- `snapshot(symbol, …, detected_at)` returns day-scoped fields only if `state.market_day == market_day(detected_at)`. Otherwise they are absent. This covers a price-less opening event on a new day.
- `last_price` obeys the same rule. `firstObservedAt` becomes `Option`: absent is "unknown", never `detectedAt`.

**Causal timestamp semantics.**
- The reset is decided by the **event's data timestamp**, never `received_at`, so live and replay agree.
- `firstObservedAt` means "timestamp of the first price-bearing event for this symbol in this market day **as observed by this process**".
  - It inherits the event's convention: BarUpdate = bar start; FunnelSignal = bar end.
  - The contract must state this. Optionally, normalise BarUpdate to bar end (`+interval_secs`) for baseline purposes; that is a separate, versioned choice.
- `sessionLowObserved` means "minimum observed price for this symbol since the first observation this market day".
- Nothing observed after `detected_at` may appear. This is unchanged and structurally guaranteed by fold order.

**Identity and lifecycle.**
- `OpportunityId`/`EpisodeId` (UTC `sessionDate` + sequence) are unchanged.
- A new per-row `marketDay` is added.
- The UTC-midnight lifecycle close is unchanged (see §3.4).
- A symbol re-entering coverage the same market day keeps its baseline. This is intended: "since monitoring began this market day".
- A symbol re-entering on a later market day gets a fresh one.

**Schema implications.**
- `PreDetectionContext` gains:
  - `marketDay` (YYYY-MM-DD, NY, 04:00 boundary);
  - `observationStartedAt` (the first event this cache instance ever saw);
  - `baselineTruncated: bool` = `observationStartedAt > marketDayOpen(marketDay)` (04:00 ET of that day, DST-aware). This is true on a restart or deploy day.
- `firstObservedAt` becomes optional.
- Existing field names keep their names, with a **changed meaning**. So bump:
  - `SIGNAL_CONTEXT_SCHEMA_VERSION` 1→2 (`context.rs:43`);
  - `OI_FEATURE_SCHEMA_VERSION` 2→3 (`opportunity.rs:89`);
  - `OPPORTUNITY_SCHEMA_VERSION` 2→3 (`:75`), because `moveBeforeDetectionPct` changes meaning;
  - `EPISODE_SCHEMA_VERSION` 2→3 (`episode.rs:53`);
  - and add `baselinePolicy: "market-day-0400-ny-v1"` to `OiVersions` (`opportunity.rs:366-378`) so every row self-declares.
- `FunnelFeatures` and `MarketFeatures` gain `observedAt` and `marketDay`. Populate `MarketFeatures.session` with `classify_session`.
- The OiConfig fingerprint does not change unless a config field is added. Recommend not adding one, and keeping the policy in the versions.

**Backward compatibility.**
- Readers must accept both.
- New fields are `#[serde(default)]`/`Option`. Old rows (schema 2/1) deserialize with `marketDay` absent. Readers must treat absent `marketDay` as the **old contract**, not as "market day = sessionDate".
- `oi_v2_replay.rs:277-279` must stop defaulting `firstObservedAt` to `at`.
- The frozen test mirror `backtest-metrics/tests/frozen/mod.rs` (`:99`, `:1117-1293`) copies the schema constants and the cache usage. Update it deliberately, or keep it as the frozen old contract and add a new frozen fixture.

**Historical replay.**
- Schema-2 artifacts from 09-21 to 09-25 are **old-contract evidence**. Their `firstObservedAt`, `firstObservedPrice`, `sessionLowObserved`, `moveBeforeDetectionPct`, regime, and V1 prior-move components measure "since 09-21 first sight". They must not be reinterpreted or overwritten.
- A corrected baseline can be reconstructed offline from the discovery tape (`latestTrade`, 15 s cadence). That is a **different quantity** (whole-market, not "as observed by this process"). It must carry its own name and version, such as `reconstructedFirstPriceMarketDay`.
- A replay of a UTC day file through the new code yields `baselineTruncated=false` only if replay begins at or before that market day's 04:00 ET. In EST that requires the prior UTC file's tail.

**Deployment.**
- Deploying is a restart, so the cache is empty.
- On deploy day, every baseline starts at the first event after restart, and `baselineTruncated=true` for every symbol that day. That day must not be designated as clean V2.1 evidence.
- From the next 04:00 ET, baselines are complete (subject to "monitoring began" semantics).
- A mid-day restart later has the same effect and is self-reporting.
- Hydrating baselines from REST at startup (`fetch_session_bars` from 04:00 ET) is possible but puts network I/O in a research consumer. Recommend deferring it and relying on the flag.

**Tests** (D3-T1…T15; map to the brief's 15 by content — the brief's own list was not available to this trace):
1. `market_day` in EDT: 07:59:59Z → previous day, 08:00:00Z → same day.
2. `market_day` in EST: 08:59:59Z → previous day, 09:00:00Z → same day.
3. `market_day` on DST-transition Sundays (2026-03-08, 2026-11-01): 04:00 local is unambiguous, with no panic in `from_local_datetime`.
4. The first event of a new market day resets `first_observed`, `session_low` and `price_trail`. The same-day re-event does not.
5. Friday → Monday (weekend) resets exactly once. The day after a holiday resets once.
6. `session_low` on day 2 is never lower than day-2 prices (the JAGX 2.5 case).
7. `moveBeforeDetectionPct` on day 2 uses day-2 first price. Regression test with the JAGX fixture: 2.77 on day 1, 8.29 on day 3, which must not yield 201%.
8. A late prior-day event after reset does not reseed or lower the baseline.
9. A price-less opening event (MomentumUpdate) on a new day with no same-day price gives no `last_price`, so no opportunity opens at yesterday's price, and `price_Nm_before` is absent.
10. `snapshot` at `detected_at` in a later market day than the state omits all day-scoped groups.
11. Ignition and consolidation counters reset per market day.
12. Symbol leaves and re-enters the same market day, and keeps its baseline. Re-entry the next day gets a fresh one.
13. The `baselineTruncated` flag is true for a cache that starts after 04:00 ET, and false for one that started before.
14. Replay parity: the same event sequence through `OpportunityIntelligence` live-style and replay-style gives identical `preDetection`. Replay from an empty cache equals live once the day has reset.
15. Schema: an old schema-2 row deserializes with `marketDay` absent, and the new row round-trips exactly. `firstObservedAt` absent is not rendered as `detectedAt`.
- Also cover `EpisodeTracker`, which has its own cache: at least T4 and T7 against `episode.rs`.


## D7 — session volume, gap and baseline freshness

> Scope note: items under "Desired behaviour" 1–2 (snapshot staleness, premarket volume source) are **D7b, design only**. Item 3 (OI/episode `funnel`/`market` freshness + `observedAt`/`marketDay`) is **D7a, implemented** with D3.

**Current behaviour.**
- The universe snapshot uses `dailyBar`/`prevDailyBar` unconditionally. Premarket (04:00 to ≈09:31 ET) these are yesterday and the day before.
  - `session_volume` = yesterday's full-day volume.
  - The raw gap is against the close two sessions back.
  - The raw average is the volume two sessions back.
- The seed step fixes gap and average only for price+gap survivors selected on the wrong gap.
- Movers never correct anything.
- In OI and episodes, the `funnel` and `market` groups persist indefinitely (no timestamp, no freshness).

**Why incorrect.**
- Stage-2 relative volume premarket measures *yesterday*:
  - yesterday's runners qualify;
  - today's premarket runners cannot (LHSW, DCOY).
- The qualified set jumps discontinuously at 09:31 (verified 13:31:02Z on 09-22).
- Premarket FastFunnel coverage, quiet-watch coverage, movers leaderboards, and halt coverage are all chosen on the wrong day.
- OI `features.funnel/market.sessionVolume` can be from a previous day, even several days back.

**Desired behaviour.**
1. **Snapshot freshness.** Compute `today = market_day(now)` (or `market_day(latestTrade.t)`). The snapshot `dailyBar` is *current* iff NY-date(`dailyBar.t`) == today. If it is stale:
   - the reference close is `dailyBar.c`;
   - the average proxy is `dailyBar.v`;
   - today's `session_volume` is **unknown** from the snapshot.
2. **Premarket volume.** For price+gap survivors (a small set, already seeded), obtain today's cumulative volume since 04:00 ET from bars. Two options:
   - batched multi-symbol `/v2/stocks/bars` 1Min (or coarser) since 04:00 ET;
   - reuse the per-symbol `fetch_session_bars` that `spawn_periodic_rescan` already runs after qualification (`live.rs:1454-1461`), moved to before the rel-vol gate for a bounded, prioritised set.

   Everything else fails closed on rel-vol, as unknown float does today. The movers "Highly Trading" list excludes stale-volume rows premarket, or labels them with the day they belong to.
3. **OI and episodes.** `FunnelFeatures`/`MarketFeatures` get `observedAt` (the FunnelSignal timestamp) and `marketDay`. `snapshot` omits them unless they are from the same market day and not later than `detected_at`.
   - Recommend also a staleness cap. There is currently none; 120 s would match other groups, but funnel signals arrive once a minute, so use ≥120 s or the same-market-day rule only. This is a documented choice.

**Causal timestamp semantics.**
- `sessionVolume` = cumulative volume for the market day from 04:00 ET through the end of the last completed minute bar ≤ observation time. It includes premarket.
- The snapshot path must record which source produced it: `sessionVolumeSource ∈ {stream_bars, rest_bars, snapshot_daily_bar, unknown}`, plus `sessionVolumeAsOf`.
- Relative volume premarket is cumulative-since-04:00 over a full-day average. It is **not time-of-day normalised**, so premarket and regular values are not comparable. State this; do not silently "fix" it here.

**Identity and lifecycle.**
- IDs are unaffected.
- Funnel qualification membership changes premarket. This is intended, and it changes which FastFunnel opportunities exist. It is a detector-coverage change, not only a measurement change, so it must be declared as such and versioned.
- Quiet-watch and mover coverage change for the same reason.

**Schema implications.**
- `TickerSnapshot.session_volume: u64` (`fast-funnel/src/types.rs:23`) cannot express "unknown". Two options:
  - make it `Option<u64>`, which touches `fast-funnel`, `universe`, `movers`, `session` and tests;
  - add `session_volume_known: bool` / `source`.
- Discovery `scan_completed.selection_inputs` gains `sessionVolumeSource`/`dailyBarDate` so the tape self-describes.
- OI: `features.funnel.observedAt`, `features.market.observedAt`, `marketDay`, and `market.session` populated. This bumps the OI feature schema, and can share the D3 bump.
- `ScanEvent::FunnelSignal` is unchanged (its values were already correct).
- Optional, for a later feature: `regularSessionVolume`/`premarketVolume` split. Not needed for correctness.

**Backward compatibility.**
- Old OI rows have no `observedAt` on funnel/market. Readers must treat them as old-contract (possibly stale-day).
- Old discovery `selection_inputs` must be interpreted with the rule "`session_volume` is yesterday's when recorded before the roll". The raw `snapshot_batch` records carry `dailyBar.t`, so historical staleness is **fully reconstructable** from the tape.
- Client `FunnelPanel` is unaffected, because it reads FunnelSignal.

**Historical replay.**
- Schema-2 OI artifacts: `features.funnel` and `features.market` are old-contract. The correct same-day value is recoverable from the FunnelSignal stream only if it was captured, since OI stores the last-seen group, not the signal.
- Discovery tapes allow an exact per-scan recomputation of the corrected snapshot inputs (`dailyBar.t` is recorded) for 09-21 to 09-24.
- Historical premarket funnel membership cannot be "replayed as if fixed", because the symbols that were not tracked have no stream data. Treat premarket FastFunnel coverage before the fix as a known coverage bias.

**Deployment.**
- The snapshot change takes effect on the first scan after start.
- Restarting mid-premarket is safe: seeds and bars are refetched, and SessionTracker is backfilled from 04:00 ET.
- Watch the API budget: extra bars requests per 15 s scan for price+gap survivors. Bound and cache them per minute.
- Deploying intraday changes qualification behaviour mid-session, so do not designate that session.

**Tests** (D7-T1…T14):
1. Staleness detector: `dailyBar.t` = previous NY date means stale, same NY date means current. Cover EDT (`04:00Z`) and EST (`05:00Z`) encodings.
2. Monday premarket with `dailyBar` = Friday means stale. Post-holiday premarket means stale.
3. Stale snapshot: the gap reference is `dailyBar.c`, not `prevDailyBar.c`. BTTC fixture 09-22 10:54Z: gap ≈ −25%, not +63%.
4. Stale snapshot: `session_volume` is unknown, not `dailyBar.v`, and Stage-2 rel-vol fails closed without bars.
5. Premarket survivor with bar-derived volume passes rel-vol. LHSW-style fixture: +184%, today's volume > 5× average.
6. Yesterday's runner with no volume today does not pass premarket. ZEO-style fixture.
7. Seed preselection uses the corrected gap: a stock up today after a down yesterday is seeded.
8. After the roll (13:31Z EDT / 14:31Z EST), snapshot `dailyBar.v` is used as-is. Include the TOPS equality fixture: 53,418,201 from the snapshot equals the SessionTracker value.
9. There is no discontinuity at 09:31: for a fixture spanning 13:29–13:33Z, qualified membership does not change purely because of the roll.
10. Movers: premarket Highly Trading excludes or labels stale-volume rows. Top Gainers `change_pct` is against `dailyBar.c` when stale.
11. Quiet watch premarket uses corrected gap and known or unknown volume, with no selection on yesterday's volume.
12. OI `snapshot` omits `funnel`/`market` from a previous market day. BTTC fixture: the 09-21 funnel must not appear on 09-22 or 09-24.
13. OI `funnel.observedAt` is ≤ `detected_at` and within the chosen staleness. `market.session` is populated.
14. Early-close day and DST-transition week: staleness and `market_day` behave correctly, and `classify_session` stays consistent with the calendar.

**D7b implementation notes (P3, branch `p3/d7b-premarket-volume`).** Authorized in P3 as a correctness fix, not detector tuning: no threshold changed (RVOL ≥ 5, gap ≥ 10%, $0.25–$20, float < 20M). It is still a declared **detector-coverage change** premarket (see "Identity and lifecycle"); do not designate a session it is deployed into.

- **Source of truth.** `session_volume` = cumulative *extended-session* volume since 04:00 ET. `TickerSnapshot.session_volume: Option<u64>` plus `session_volume_source` ∈ {`snapshot_daily_bar_current`, `minute_bars_since_open`, `unknown`}; `None` ⇔ `unknown`, and unknown fails relative volume closed. No regular-session figure was needed, so none was added.
- **Snapshot rule** (`universe::snapshot_from_raw`). Current iff NY date of `dailyBar.t` = `market_day(now)`. Current: reference `prevDailyBar.c`, average proxy `prevDailyBar.v`, volume `dailyBar.v`. Stale: `dailyBar.c`, `dailyBar.v`, volume unknown. Ahead/missing/undated: `prevDailyBar.c`, `prevDailyBar.v`, unknown.
- **Premarket volume** (`premarket_volume.rs`). Price + corrected-gap survivors with a seed and a trade since 04:00 ET get minute-bar volume from one batched multi-symbol `/v2/stocks/bars` 1Min request per 50 symbols, summed over complete bars (`t ≥ 04:00 ET`, `t + 1 min ≤ now`) and stamped `as_of` the last completed minute. Budget: ≤ 100 symbols per scan, each refreshed at most once per minute, ≤ 12 requests started per minute (hard ceiling 15 with pagination). Never-fetched symbols first, then |gap|. A symbol with no trade since the open is not fetched and stays unknown. A failed fetch leaves the symbol unknown, or at an earlier causal value.
- **No double count.** The sources are exclusive: bars only while the snapshot is stale, `dailyBar.v` alone after the roll. The 09-22 TOPS equality (53,418,201) is a test fixture; membership across 13:29–13:33Z is continuous.
- **Restart/reconnect.** The cache lives in the rescan task and is rebuilt with it; the next scan re-sums from 04:00 ET.
- **Movers.** Highly Trading skips unknown-volume rows; Top Gainers rows carry `volumeSource` (`unknown` premarket, with `volume` 0 kept numeric for existing clients). Halt coverage inherits both.
- **Quiet watch.** An unknown volume is not read as quiet or busy; the gap test (now against the right close) carries the premarket decision. A known volume must still be ≤ 1.0x.
- **Discovery.** `scan_completed` gains `selection_inputs_schema: 2`; each row gains `sessionVolumeSource`, `dailyBarDate`, `dailyBarFreshness`, `sessionVolumeAsOf`; `session_volume` may be `null`. A `premarket_volume` object records candidates, requests and resolved volumes. The record envelope `schema` stays 2.
- **Health.** `/research/completeness` gains `premarketVolume` {`marketDay`, `initializedAt`, `lastScanAt`, `lastSuccessfulFetchAt`, `bySource`{`snapshotDailyBarCurrent`, `minuteBarsSinceOpen`, `unknown`}, `survivorsNeedingVolume`, `survivorsResolved`, `survivorsDeferred`, `survivorsNoTradeSinceOpen`, `fetchFailures`, `initFailures`, `lastFetchError`, `requestsThisMinute`, `requestsThisMarketDay`, `cachedSymbols`, budget constants}. `fetchFailures`/`initFailures` are market-day cumulative.
- **Not changed.** `ScanEvent::FunnelSignal` and `SessionTracker` values (already correct); `SIGNAL_CONTEXT_SCHEMA_VERSION` (no signal-context value changes). The halt monitor's own from-creation volume counter (trace §2.2 C) is out of scope.
- **Tests.** T1, T2, T14: `premarket_volume_tests.rs`. T3–T9, T11: `universe_d7_tests.rs`. T10: `movers.rs`. T12, T13 are D7a (`backtest-metrics/src/market_day_baseline_tests.rs`, `d7a_*`).


## D4 — Opportunity disposition on outcome rows

- **Current behaviour.**
  - Every `OpportunityOutcomeRow.opportunityDisposition` is `still_open`.
  - Engine closes are discarded (`opportunity_shadow.rs:263,302`), and `note_disposition` has no production caller.
  - Closes are not persisted in any artifact.
- **Why incorrect.**
  - The field asserts a lifecycle fact that is never measured.
  - A reader cannot tell "open" from "not tracked", and the enum cannot express the reason.
  - Historical dispositions are unrecoverable.
- **Desired behaviour.**
  - Every row carries the opportunity's disposition *as of its settlement*: the engine's close reason if the close was observed at or before `anchor_at + 1320 s`, otherwise `still_open`.
  - It also carries `opportunityClosedAt` and `opportunityCloseObservedAt` when closed.
  - The disposition is provenance only and never censors.
- **Causal timestamp semantics.**
  - The comparison uses `closeObservedAt`, the receipt instant of the event whose processing produced the close, against the fixed deadline `anchor_at + 1320`.
  - `closedAt` is reported as the engine's lifecycle instant. It may be backdated, and for a session boundary found by expiry it may precede `anchor_at`.
  - Closures are applied before settlement within an iteration.
- **Identity and lifecycle implications.**
  - The two lifecycles stay separate. An anchor binds by `(opportunityId, openedAt)`.
  - Application is exactly-once and first-terminal-wins. A duplicate or conflicting notice is a no-op.
  - Restart: no cross-process notices. A graceful shutdown gives `capture_ended`.
- **Schema implications.**
  - `OpportunityDisposition = {still_open, inactivity, session_boundary, capacity_reached, capture_ended}`, extended in lockstep with `OpportunityCloseReason`.
  - Optional `opportunityClosedAt` and `opportunityCloseObservedAt`.
  - `OPPORTUNITY_OUTCOME_VERSION` becomes `opportunity-outcome-v2`, because the meaning of `still_open` changes.
  - A new `opportunity_closed` record in the OI artifact.
- **Backward compatibility.**
  - v1 rows keep parsing; readers must treat v1 `still_open` as **unknown**.
  - `closed` and `capacity_evicted` were never produced in production. Remove them, or keep them as deserialize-only aliases.
- **Historical replay implications.**
  - Pre-fix sessions cannot be given dispositions. Do not infer them from the last snapshot plus 300 s and present that as fact.
  - Post-fix sessions replay dispositions from the `opportunity_closed` records.
- **Tests required.** D4.6, items 1–15.
- **Deployment implications.**
  - Research shadow only, and off unless `OPPORTUNITY_INTELLIGENCE_SHADOW=1`. No client, detector or trader path.
  - Cost is O(anchors per symbol) per close.
  - The OI artifact grows slightly: one record per close, about 0.85/s.
  - The preflight or contract pin must move to outcome-v2 in the same release.
- **Implementation notes (branch `p2/d4-d6-observability`).**
  - D4-5 is fixed in the engine rather than only documented: `close()` floors `closed_at` at the last ranking instant the opportunity took part in, so `closedAt` is never earlier than any anchor issued while it was open. The comparison still uses `closeObservedAt`.
  - Closes are persisted as `opportunity_closed` **markers** (the data file is parsed as snapshots, where any other line is blocking-malformed). Marker loss is not counted as data loss, so a replay must reconcile the marker count against `capture_finished.opportunitiesClosed` / `closedByReason`.
  - `oi_outcome_replay --closures=<markers>` reproduces dispositions (requires rows carrying the exact `openedAt`). Without closes it stamps `opportunity-outcome-v1`, because the dispositions are unknown.
  - `closed` was removed; `capacity_evicted` survives as a deserialize-only alias of `capacity_reached`.

## D5 — Opportunity lifecycle unit

- **Current behaviour.**
  - One open opportunity per symbol. Any event carrying the symbol (bars, halt warnings, catalysts, level funnel or momentum) refreshes `last_seen_at`.
  - Invalidations are absorbed.
  - Closes happen only on 300 s of total silence, a UTC date change, capacity, or capture end.
  - Tracked symbols therefore live a symbol-day. YMAT lived 46,240 s with 40,300 events and 197 absorbed invalidations; the mean lifetime is 3,752 s.
  - Detection-time features are frozen at the day's first detection.
- **Why incorrect.**
  - The documented unit is one causal move/setup (§D5.1 quotes).
  - The 300 s "silence" rule was inherited from episodes without noticing that market-data events never fall silent.
  - The results: earliness, age, invalidation burden, the move extremes and first-entry evaluation all describe the wrong span, and V2's age prior is misapplied.
- **Desired behaviour.**
  - Lifecycle `move-v1`. It opens on a detector *edge*; it lives while detector evidence continues; market data updates state only.
  - It closes on `T_move` without relevant evidence (`setup_inactivity`, or `invalidated` when the last evidence was a rejection), or on a session boundary, capacity, or capture end.
  - Confluent detectors merge. A new setup after a close gets a new id.
- **Causal timestamp semantics.**
  - Every rule reads only the event stream up to `now`.
  - `closed_at = last_relevant_at + T_move` for evidence-silence closes.
  - A session-boundary `closed_at` must be ≥ `opened_at` and must not precede the anchors issued while open; record `closeObservedAt`.
  - Bars keep the R2 `+interval` correction.
- **Identity and lifecycle implications.**
  - `SYM:UTC-date:ms` stays unique. Many ids per symbol-day are now expected.
  - `opportunityId` denotes a move, so it cannot be compared with schema ≤2 ids.
  - Episode membership ambiguity will rise and must be reported.
  - UTC versus NY trading date is an open, separate decision.
- **Schema implications.**
  - `OPPORTUNITY_SCHEMA_VERSION = 3`; `OiVersions.lifecycle`; `OiConfig.lifecycle` and `move_inactivity_secs` in the fingerprint.
  - New close reasons; new fields `lastRelevantAt` and `openedPhase`; the D4 enum extended.
- **Backward compatibility.**
  - Schema ≤2 artifacts keep parsing, with lifecycle implied `symbol-activity-v1`.
  - Keep `SymbolActivityV1` selectable so historical replays and `model_freeze` keep their meaning.
- **Historical replay implications.**
  - Re-segmenting a past session needs its raw `ScanEvent` stream; snapshot artifacts are not enough. Verify which sessions have a raw corpus.
  - V2 development results (09-17/18) were produced under symbol-activity-v1 and do not transfer.
  - V2 must either be re-preregistered against schema 3 or confined to schema ≤2.
- **Tests required.** The 16-case matrix above, plus a new explicitly reviewed `model_freeze` regime (lifecycle change: ids and ranks differ by design; identical under `SymbolActivityV1`).
- **Deployment implications.**
  - Research shadow only; no production path.
  - Ship behind the lifecycle selector, with the default unchanged until preregistered. Preferably run both lifecycles in shadow for at least one prospective session.
  - Re-measure `supported_lifetime_secs`. The structural bound of one per symbol still holds, so capacity is unaffected.
  - Stop condition: do not flip the default without a written preregistration of the relevant-activity table and `T_move`, and a governance decision on V2.


## D6 — ranking cohort

**Current behaviour.** `OpportunityIntelligence::rank` scores every open opportunity, then ranks each surface (EarlyQuality, Continuation) independently via `rank_cohort`. That function fully sorts, then keeps the top `max_rank_cohort = 4,096` (`opportunity.rs:1182-1218,1805-1810`). Rows past the cap are still emitted, with score present, `*Rank = None` and `*CohortSize = 4,096`. `cohortTruncations` counts windows where either surface was cut. In production it counted 69, with a peak open of about 4,678.

**Why incorrect.** The cap was never derived (79c21e1). It was left below the measured population when open capacity was raised to 16,375 (af986b8: "max_rank_cohort stays 4,096", against a measured max of 4,808). The consequences:

1. Scored opportunities are reported unranked, indistinguishable at the rank field from unscorable ones.
2. Cohort size is misreported, so every normalized quantity is wrong in those windows: persisted `shadowState`, and alpha TopPercent thresholds.
3. The session becomes INVALID under the completeness contract.
4. The cap buys nothing: sort and emission are already O(open), and the saving is under 17 ms and under 2% memory even at capacity (§2).
5. V1 and V2 cohorts differ, contrary to the V2 preregistration §19-21.

**Desired behaviour.** Every opportunity with a score on a surface receives a rank on that surface, in every window. `*CohortSize` equals the number scored on that surface. The rank bound is `max_rank_cohort ≥ max_open_opportunities()`, compile-asserted for the default and checked at runtime by `capacity_invariant()`, so truncation is unreachable. Each snapshot additionally carries `*RankFraction = (rank − 1)/cohortSize`.

**Causal semantics.** Unchanged. The rank at window W uses only the scores computed at W from state at or before W. The cohort is the set of opportunities open at W. Removing the cap adds no information and alters no score. The fraction's denominator is contemporaneous (same window, same surface).

**Identity / lifecycle.** Unchanged. No `OpportunityId`, open, close or eviction rule moves. Ranks remain per-window ordinals; one opportunity holds at most one slot per surface.

**Schema.** Additive optional `earlyQualityRankFraction`, `continuationRankFraction` (f64, omitted when the rank is absent). No field removed or renamed. `OPPORTUNITY_SCHEMA_VERSION` is unchanged for D6 alone. The config fingerprint changes automatically (D6 alone: `oi-cfg-15861d6d0b263f12`). `RANKING_VERSION` is unchanged. Health gains the per-surface truncation counters, cohort last/peak, `rankCohortCapacity` and rank latency (§7). A new capture marker `ranking_cohort_truncated` is written only if the invariant is violated.

**Backward compatibility.** Old readers parse new rows, since the fields are optional. Pre-fix rows remain parseable. A pre-fix row whose score is present but whose rank is absent **is** a truncated row. Readers must key the "no truncation" assumption on the fingerprint (`b4f21c8b…` means the cap was possible) together with `cohortTruncations`, not on the schema version. The alpha dataset should use per-surface cohort sizes or the fraction, not `max(early, cont)`.

**Historical replay.** No raw events exist, so `oi_replay` cannot re-run production days. The persisted snapshots suffice, though. Per `windowId`, re-rank all rows with a score present by `(value desc, symbol, sequence)` to recover exact uncapped ranks and the true N, then recompute `shadowState` and the fraction. This is valid only for windows with no OI writer loss, which must be checked first. Tag repaired outputs as derived (for example `repair: "d6-rerank"`), and never overwrite the capture.

**Tests required.**

- *Cohort levels* (each asserting zero truncation, `*CohortSize == scored`, and every scored row ranked 1..N, contiguous and unique):
  - 4,096 (the old boundary)
  - 4,678 (the production peak)
  - 6,000
  - 16,375/16,384 (open capacity; requires `supported_symbol_universe` lifted in the fixture)
  - a stress case above capacity (for example 32,768 opens against 16,375: open is capped by eviction, so the ranked N stays ≤ capacity and truncation remains 0)
- *Top-rank invariance:* for n > 4,096, ranks 1..4,096 are identical to the pre-fix engine. Compare against `tests/frozen` or a copy of the old `rank_cohort`.
- *Surfaces:* EQ and CC truncation are counted separately. A fixture where only CC exceeds the (test-lowered) cap increments only `continuationCohortTruncations`.
- *Invariant:* a const assert that `DEFAULT_MAX_RANK_COHORT >= DEFAULT_MAX_OPEN_OPPORTUNITIES`. `capacity_invariant()` returns `Err` for `max_rank_cohort < max_open_opportunities()`. A config deliberately violating it still counts truncations and writes the marker (the counter must stay reachable, per the `start_inner` lesson in `opportunity_shadow.rs:88-96`).
- *Fraction:* N = 1 gives 0.0. Best gives 0.0. Worst gives (N−1)/N. For p ∈ {0.05, 0.10, 0.25} and N ∈ {1, 7, 10, 4,678}, `fraction < p ⇔ rank ≤ ceil(N·p)`. The fraction is absent iff the rank is absent.
- *Determinism:* byte-identical JSON across two builds and across reversed insertion order, at 4,678 and 16,384 (as the scratch harness does). Add a NaN-score fixture once the comparator decision is made (`total_cmp` or unranked).
- *Latency:* `rank()` at 16,384 below a loose bound (for example 4× the 4,678 time; growth must be about linear, never quadratic). Report the absolute ms on the VPS once.
- *Memory:* the counting-allocator transient of `rank()` at 16,384 is within 1.1× of the capped engine (measured 89.2 vs 87.1 MB).
- *Contract/pins:* update `model_freeze.rs::out_of_scope_configuration_is_untouched` deliberately, with a comment citing D6. Update the fingerprint pins (`alpha/spec.rs:70`, `session.sh:44`, the runbook, `completeness_tests.rs`, `runbook_contract_tests.rs:89`). Update the spec SHA pin (`session.sh:45`, runbook) after the re-bind.
- *Completeness:* `any_known_loss()` is true when `cohortTruncations > 0`. `check()` stays INVALID.

**Deployment implications.**

- Code-only change in the research path: no detector, client or auto-trader surface.
- CPU: +0–1 ms per 30 s window at the observed peak, and at most about 17 ms at capacity.
- Memory: unchanged.
- The fingerprint change breaks preflight until `session.sh` `EXPECTED_OI_CONFIG` and the spec binding are updated in the same commit; the build test already enforces the `session.sh` half. The qualification contract must be re-bound (new SHA, or `alpha-qualification-v4`) before the next prospective session. Sessions under the old fingerprint remain evaluable only under the old binding and are INVALID wherever `cohortTruncations > 0`.
- The V2 preregistration's "identical cohort" claim should get an erratum.
- Deploying means pushing stockspotter `master`, which reaches the live VPS within about 2 minutes. Do it outside market hours, and only as a deliberate release.

**Implementation notes (branch `p2/d4-d6-observability`).** Fingerprint verified `oi-cfg-15861d6d0b263f12`. Scores compare with `total_cmp` (NaN placed by sign bit; deterministic). The alpha dataset's `max(early, continuation)` denominator was a genuine bug and is fixed: percentile thresholds now use per-surface cohort maps (`windowEarlyCohortSizes`, `windowContinuationCohortSizes`). The qualification contract is re-bound as `alpha-qualification-v4` (v3's criteria, unchanged); its SHA moves again when the D3/D7a schema bumps land.

---



## Versioning matrix (D3–D7)

Recommendation: ship these together and bump once per identifier. Where two changes hit the same identifier, a single bump covers both.

| CHANGE | SEMANTICS CHANGED? | PERSISTED FIELD CHANGED? | VERSION BUMP? | WHY / recommendation |
|---|---|---|---|---|
| **D3** baseline reset semantics (feature baselines reset at a market-day boundary rather than the current rule) | **Yes**: the same field name yields different values after the boundary | Values of existing `SignalContext` fields (and therefore OI `features`/`detectionFeatures` and episode contexts); possibly a new reset-status field | **`SIGNAL_CONTEXT_SCHEMA_VERSION` 1→2** (its own rule: changed meaning). `OI_FEATURE_SCHEMA_VERSION` 2→3 because the OI feature surface's meaning follows. Model versions **no** (weights unchanged; inputs are versioned via the feature schema). Fingerprint moves automatically only if the reset boundary becomes an `OiConfig` field. | A reader must be able to tell pre- and post-reset baselines apart. If this changes only feature-cache internals and emitted values are provably identical on replay, then no bump, with a replay-parity test as proof. |
| **D4** `opportunityDisposition` populated | **De facto yes.** Today `note_disposition` has **no production caller** (only `opportunity_outcome_tests.rs:93,119`), so every production row says `still_open`. After the fix, `still_open` means "actually still open". | Values of the existing `opportunityDisposition` | **`OPPORTUNITY_OUTCOME_VERSION` → `opportunity-outcome-v2`**. That forces a spec `expectedOutcomeMeasurementVersion` update and changes the spec SHA. | v1 rows' disposition must be read as *unknown*, and only a version can say so. (A `dispositionTracked: bool` field also works, but it adds a second way to express the same fact.) Disposition stays provenance, never a censor. |
| **D5** opportunity identity / segmentation (e.g. session_date = market day, segment by session) | **Yes** if `sessionDate`/`sequence`/`opportunityId` or the open/close rule changes | `opportunityId` components; possibly the episode id and uid | **`OPPORTUNITY_SCHEMA_VERSION` 2→3** (precedent: the `sequence` change was exactly this). If episodes change too: `EPISODE_SCHEMA_VERSION` 2→3, plus `EPISODE_UID_VERSION` `epu2` if the uid tuple changes, plus `MEMBERSHIP_RULES_VERSION` if join rules move. Fingerprint moves automatically if a segmentation boundary becomes config. | Identity drives every join. A silent change reuses the same keys for different units. |
| **D6** rank cap = open capacity, plus `*RankFraction` | Rank *algorithm* unchanged. Ranks 1..4,096 are byte-identical. Cohort size and bottom ranks change **only in windows where more than 4,096 were scored**, where the old values were the defect. | `*CohortSize` values (true N) and `*Rank` values (no longer `None` for scored rows), both only in formerly-truncated windows; `shadowState` in those windows; plus two new optional fields | **Fingerprint: yes, automatic** (`oi-cfg-15861d6d0b263f12` for D6 alone). **`RANKING_VERSION`: no.** The ranking rule is identical, the cap is config, and repo precedent (af986b8) treats capacity as config made visible via the fingerprint (`model_freeze.rs:365-368`). **`OPPORTUNITY_SCHEMA_VERSION`: no, for D6 alone** (additive optional). If D5 bumps it anyway, document the new fields under v3. | Where no truncation occurred, the new artifacts are identical in rank. The fingerprint is the attribution handle for "cap removed". |
| **D7** `session_volume` semantics (e.g. reset boundary or regular-only vs cumulative) | **Yes** | `SignalContext.sessionVolume`, `FunnelFeatures.sessionVolume`; **also `ScanEvent.session_volume` on the wire** (`market-data/src/events.rs:34`), which is production- and client-visible, not research-only | **`SIGNAL_CONTEXT_SCHEMA_VERSION`**, shared with D3 (one bump to 2 covers both if they ship together). `OI_FEATURE_SCHEMA_VERSION` shared with D3. Also check whether the discovery capture persists it, and bump that artifact's version if so. The wire change has no version today; it needs a client-compatibility note. | Feeds funnel gates (`rel_vol_ok` etc.): a detector-visible change, outside the shadow boundary. Confirm the scope with the D7 owner. |

**Governance consequence:** any of D3/D4/D5/D6 moves the fingerprint and/or a pinned `expected*` value. That changes the spec SHA pinned in `session.sh:45`, and the frozen contract (`FROZEN_AT 2026-09-17`) needs a deliberate re-bind, meaning a new SHA or `alpha-qualification-v4`, *before* the next prospective session. The build enforces part of this: `runbook_contract_tests.rs:189-192` fails if `session.sh`'s pinned fingerprint differs from `OiConfig::default()`.

---


## Observability: what `/research/completeness` exposes today, and bounded additions

**Today:** `{report:{generatedAt, commit, oiConfigFingerprint, opportunityIntelligence(WriterCapture), measurement(WriterCapture), discovery, opportunityEngine:{open, peak, capacity, capacityEvictions, evictionMarkersDropped, opportunitiesOpened, opportunitiesClosed, cohortTruncations, scoresEmitted}, opportunityOutcomes(WriterCapture), opportunityOutcomeEngine:{outstanding, peakOutstanding, capacity, anchorsCreated, anchorsSettled, capacityEvictions, symbolsTracked}}, measurementPending:{pending, pendingPeak, pendingCapacity, capacityEvictions, openEpisodes}, retention, discoveryRetention, anyKnownLoss}`.

Sources: `research_health.rs:117-175`, `completeness.rs:115-151`, `http.rs:549-591`, `opportunity_shadow.rs:163-203`.

**Proposed additions.** All are fixed-size scalars or small fixed enums. No per-symbol or per-window arrays. All are additive (serde default) so old readers keep working.

| Addition | Where it belongs | Notes |
|---|---|---|
| `report.reportSchemaVersion: u32` | `CompletenessReport` | The endpoint has no version today. Start at 1. |
| `report.oiVersions` (the full `OiVersions`: opportunitySchema, featureSchema, regime/price/model/ranking/policy, configFingerprint) | set once like the fingerprint (`OnceLock`) | Lets preflight verify every pinned `expected*` value, not only the fingerprint. |
| `report.outcomeMeasurementVersion`, `report.episodeSchema`, `report.signalContextSchema` | same | Required for D4/D5/D3/D7 verification. |
| `opportunityEngine.rankCohortCapacity` | `EngineHealth` | The bound for the cohort, reported beside it like `capacity` is for `peak`. |
| `opportunityEngine.{earlyCohortLast, continuationCohortLast, earlyCohortPeak, continuationCohortPeak}` | `OiHealth` → atomics | The true scored N, per surface. |
| `opportunityEngine.{earlyCohortTruncations, continuationCohortTruncations}` (keep `cohortTruncations` = OR) | same | Says which surface was hit. |
| `opportunityEngine.{rankingWindows, lastRankMicros, peakRankMicros}` | same | Latency evidence for the D6 cost claim in production. |
| `opportunityEngine.openedByStrategy{…}` / `closedByReason{inactivity, sessionBoundary, captureEnded, capacityReached}` | same | A fixed enum, so it is bounded. `capacityReached` duplicates `capacityEvictions` as a cross-check. |
| `opportunityEngine.marketDayId` (the session_date currently being assigned) | same | For D5: shows which day the engine thinks it is in. |
| `opportunityEngine.sessionVolumeResetStatus {lastResetAt, resetsToday}` | the feature cache | D7. |
| `opportunityEngine.baselineResets {count, lastAt}` | the feature cache | D3. |
| `opportunityOutcomeEngine.dispositionCounts {stillOpen, closed, capacityEvicted}` over settled rows | `OutcomeHealth` | D4. Once D4 lands, `stillOpen == anchorsSettled` is the regression signature. |
| `anyKnownLoss` to also include `opportunityEngine.cohortTruncations > 0` | `completeness.rs:156` | Makes it consistent with `check()`. |

`ops/qualify/session.sh` and `runbook_contract_tests.rs` walk these paths, so new blocking keys must be added to both.

---


## Appendix — D5 design detail (intended unit, move-v1 segmentation, test matrix)

### D5.1 What unit was intended? **C: one causal move/setup.** Not A.

The origin documents describe the unit as a *move*. The inactivity rule is a borrowed proxy for where a move ends, not a definition of the unit.

- Module doc, `opportunity.rs:14-18`: *"129,655 regular-session detector episodes collapse to ~67,032 distinct **symbol/move** opportunities. Ranking detector episodes therefore ranks the **same move** repeatedly."* Also `:10-12`: *"a single developing move that goes confirm → reject → confirm is recorded as several episodes."*
- `opportunity.rs:20-24`: *"An `Opportunity`… differs from an episode in **exactly one rule**: invalidation does not end an opportunity. Only inactivity (… 300s) and a session boundary do. Everything else… deliberately mirrors `EpisodeTracker`."* The intent is to merge fragments of one move, not to widen the unit to a day.
- V1 report (`research-reports/ALPHA-OPPORTUNITY-INTELLIGENCE-V1-2026-09-15.md:17`): *"collapses repeated detector events into evolving opportunities"*. Also `:140-146`, which is the same single-divergence statement.
- Commit `68628d2`/`79c21e1` body: *"An Opportunity differs from an OpportunityEpisode in exactly one rule: invalidation does not end it… fragments **one continuing move** into several episodes."*
- Field doc, `opportunity.rs:777-779`: `episode_fragments` is *"greater than 1 exactly when invalidation fragmented **the move**."* Also `:1620`: *"A confirm after a rejection is **the same move resuming**."*
- Test `a3_a_subsequent_move_creates_sequence_plus_one` (`opportunity_tests.rs:75-89`): *"a **new move** after closure must not reuse the first opportunity's id"*. Several moves per symbol-day are expected.
- Stage-B frozen contract (`stockspotter-research/reports/OI-V1-STAGE-B-FROZEN-CONTRACT-2026-09-17.md:245-271`) plans for *"the same symbol producing many opportunities"* and *"40 opportunities from one symbol"*. That clustering defence only makes sense if a symbol-day holds many opportunities.
- V2 preregistration (`OI-V2-PREREGISTRATION-2026-09-19.md:71,109`): D1 `maturity.opportunityAge`, knots `(0,.35)(60,.85)(300,1.0)(900,.9)(1800,.6)(3600,.3)(7200,.1)`, *"the 60–900 s band is prime, and stale decays."* An age prior peaking at 5–15 minutes assumes a move-length unit. Under the symbol-day unit, almost every row of a long-lived symbol lands on the floor.
- V2 prereg `:265`: persistence is *"reset only by opportunity lifecycle"*. Also `:337`: *"~99% of opportunities are single-detector"*. That observation also fits short units. YMAT's all-5-detector lifetime contradicts it.
- Episode design (`episode.rs:18-37`): its boundary rules, which OI inherits, were *"chosen from contemporaneous signals only"*. Rule 2 is *"**any** further observation of the same symbol… extends it."* The "any observation" rule was inherited without accounting for the fact that tracked symbols emit a bar, funnel and momentum event every minute (`live.rs:683,710,747`) and a halt warning on every trade (`events.rs` HaltWarning doc: *"sent on every trade"*).

**Where the drift was ratified, not designed.**
- The 09-16 capacity repair (`af986b8`; `opportunity.rs:146-151,290-306`) measured a mean lifetime of **3,752 s = 12.5× inactivity** and wrote *"an opportunity survives as long as its symbol keeps trading."*
- It then sized capacity to the **symbol universe** (`:153-159`, *"at most one opportunity per symbol"*).
- That repair's charter forbade changing which opportunities exist (`OI-V1-CAPTURE-CAPACITY-ROOT-CAUSE-2026-09-16.md:119-122`). It documented the behaviour it found; it did not re-decide the unit.

**Verdict.** The intended unit is **C, one causal move/setup**, with invalidation absorbed. The implemented unit is **"a symbol's continuous event activity"**. Because tracked symbols never go quiet for 300 s during the session, that is operationally **A, symbol-day**.

- The YMAT example fits: open 46,240 s, 40,300 events, 197 invalidations absorbed, all 5 detectors seen.
- Its `move_before_detection_pct` and `detection_context` are frozen at the first detection of the UTC day (`opportunity.rs:1520-1522,803-812`), often premarket. Earliness for a regular-session setup hours later therefore describes a different move.
- B (a detector-active episode) and D (a detector-specific setup) have no support in the documents.
- Episodes (E) are not per-detector either (D5-3).

### D5.2 Consumer assumptions

| Consumer | What it assumes | Effect of symbol-day |
|---|---|---|
| V1/V2 ranking (`rank`, `opportunity_v2.rs:542-552`) | Age is a maturity signal on a move | Age is time since the day's first detection |
| V2 C1 / `risk.instability` (`invalidationsAbsorbed`) | Burden within one move | Burden accumulated across the day; YMAT's 197 saturates the bins `[0,1,3,8]` |
| Stage-B evaluation ("collapse to opportunity, first-entry") | Many units per symbol | One unit per symbol-day, so first-entry means the first window of the day |
| `membership.rs` (temporal containment) | Episodes nest inside opportunities | Under symbol-day that holds trivially. Under D5 episodes (kept alive by bars) will more often outlive a move-opportunity, so `EpisodeOutlivesOpportunity` ambiguity rises |
| Outcome anchors | One anchor per window per unit | Unchanged in mechanism. Cohort composition changes under D5 |
| Capacity math (`supported_lifetime_secs`, `opportunity.rs:146-151,183`) | Lifetime is measured | Must be re-measured; the structural bound (one per symbol) still holds |

### D5.3 Causal segmentation design ("move-v1"). It uses existing detector and lifecycle state only.

**Principle.** An opportunity is alive while *detector evidence* for a setup keeps arriving. Market data (bars, trades, halt proximity, catalysts) updates its **state** (price, extremes, features) but never its **life**.

**Per-symbol edge state** (bounded by the universe, like `LiveSignalTracker` `live_signals.rs:60-62`): `funnel_passing: bool`, `momentum_qualifying: bool`, `ignition_phase ∈ {idle, candidate, confirmed, rejected}`, `consolidation_phase[strategy] ∈ {idle, surge, consolidating, entered}`. This lives in the engine, outside the opportunity, so it persists across closes.

**Relevant activity** refreshes `last_relevant_at`:

| Event | Relevant? | Why |
|---|---|---|
| `FunnelSignal` flip false→true | yes (open and keep) | Edge, as in `signals.rs`. The level `passed:true` on every bar is **not** relevant: the funnel is a gap/universe filter that holds all day |
| `MomentumUpdate{qualifies:true}` | yes (keep); the flip opens | The momentum detector still says yes, re-read every bar. Treat a level-true reading as evidence the setup is continuing |
| `MomentumUpdate{qualifies:false}` | no | |
| `IgnitionEvent CandidateOpened` / `FollowThroughConfirmed` | yes | Setup in progress or confirmed |
| `IgnitionEvent FollowThroughRejected` | **no refresh**; it is absorbed and counted | Invalidation is not a close (keeps V1's rule), but it does not extend life either |
| `ConsolidationEvent SurgeDetected / ConsolidationConfirmed / EntryTriggered` | yes | Setup phases |
| `BarUpdate`, `HaltWarning`, `CatalystUpdate`, `FunnelHealth` | **no** | State only: price, extremes, features. This is the core fix |

**Close rules** are evaluated in `expire_inactive`, keyed by `last_relevant_at` rather than `last_seen_at`:
1. `SetupInactivity`: `now − last_relevant_at >= T_move`. `T_move` defaults to the existing 300 s constant, so no new tuned number is introduced; it must be preregistered. `closed_at = last_relevant_at + T_move`.
2. `Invalidated`: the same condition, when the last evidence-bearing event was a `FollowThroughRejected` and no positive evidence came after it. It is a distinct *label* on the same causal clock. Confirm→reject→confirm within `T_move` stays one opportunity, which is exactly what V1 was built to preserve.
3. `SessionBoundary`: keep the UTC-date rule for now. Keep the event-path close at `at`. In the expiry path, set `closed_at = max(last_relevant_at, …)` and also record `closeObservedAt`.
   - Open question: 00:00 UTC is 20:00 EDT, which is the end of after-hours, but it is 19:00 EST, which is *inside* after-hours from November to March.
   - Switching to the America/New_York trading date matches `live.rs`'s own session reset (*"new session: reconnecting…"* on the NY date change). It would change `session_date` semantics and must be a separate, explicit decision.
4. `CapacityReached` and `CaptureEnded`: unchanged.

**Second independent setup.** After a close, the next *opening edge* (a funnel flip, a momentum flip, ignition confirmed, or a consolidation or micropullback entry) opens a new opportunity with a new id. While one is open, every detector's evidence merges into it. That is confluence (`detectors_seen`, `detector_transitions`), so overlapping detectors cannot create duplicates: there is still one open opportunity per symbol.

**Session phases.**
- Premarket→regular and regular→after-hours continue the same opportunity if relevant evidence keeps arriving across the bell.
- If the setup went quiet for `T_move`, the next evidence opens a new one.
- Record the phase at open (a new field `openedPhase`) so the analysis can filter on it without having to split at the bell.

**Price and earliness.**
- `move_before_detection_pct` and `detection_context` are now frozen at *this move's* open, which is what earliness should mean.
- `observed_high/low` and `max/min_move_pct` are per move.

**ID uniqueness.** `SYM:UTC-date:ms` stays unique.
- `SetupInactivity` and `Invalidated` both require `T_move` of evidence silence, so the next open is at least `T_move` later.
- The event-path `SessionBoundary` changes the date.
- Capacity evicts a symbol other than the one opening.
- The only residual is an out-of-order event exactly equal to a previous `opened_at`. Add a debug assertion and a test. Adding `openedBy` to a durable uid (like `epu1`) is optional hardening.

**Bindings.**
- Outcome anchors: unchanged, one per window per open opportunity. D4 notices now carry `setup_inactivity` and `invalidated` too.
- Episode membership: unchanged algorithm; expect more ambiguity, and report it.
- Ranking rows: fewer, shorter-lived opportunities per window, so cohort sizes change and ranks move.

### D5.4 Does D5 require Opportunity schema 3? **Yes.**

- The repo's own rule is to *bump when the meaning of an existing field changes* (`opportunity.rs:59-73`). D5 changes what one `opportunityId` denotes. It also changes the meaning of `opportunityAgeSecs`, `episodeFragments`, `invalidationsAbsorbed`, `moveBeforeDetectionPct`, `detectionFeatures`, `maxMovePct`/`minMovePct`, `rawEventCount` and `detectorsSeen`.
- Required changes:
  - `OPPORTUNITY_SCHEMA_VERSION = 3`.
  - New `OiVersions.lifecycle = "opportunity-lifecycle-move-v1"`, with the old one named `symbol-activity-v1`. It goes into the config fingerprint, as `T_move` and the rule set.
  - New `OpportunityCloseReason::{SetupInactivity, Invalidated}`. Keep `Inactivity` deserializable for schema ≤2.
- The key format can stay `SYM:date:ms`; readers must branch on `opportunitySchema`.
- `opportunity-outcome` also bumps to v2 because of D4: the meaning of `still_open` changes from "not tracked" to "as of settlement".

### D5.5 Implement now, or design now and implement later? **Design now; implement later, behind a version, after preregistration.**

Honest risk assessment:
- **Implementability: moderate.** The engine change is localized, mostly `observe`, `record`, `expire_inactive`, `qualifying_strategy` and a new edge-state map. The 16-case matrix below can be proven on synthetic fixtures in about a day.
- **Correctness proof beyond unit tests: not possible today.**
  1. The rule set, meaning the relevant-activity table and `T_move`, is a research choice that changes every artifact. Under this project's discipline it must be preregistered before any outcome is examined. Choosing it now, after seeing YMAT and V2 results, invites outcome-informed tuning.
  2. Validating that the unit matches "one move" needs a raw `ScanEvent` corpus replay. The OI artifacts hold only 30 s ranking snapshots. I did not verify whether full raw event recordings exist for 09-17/18/24.
  3. It invalidates comparability with every schema-2 artifact, including the V2 development sessions. V2 is preregistered ("any divergence… is a stop condition", `opportunity_v2.rs:3-6`), and its inputs change meaning. That needs an explicit governance decision: re-preregister, or evaluate V2 only on schema-3 sessions.
  4. `tests/model_freeze.rs` asserts byte identity against the deployed engine below the capacity bound. That assertion **must fail** by design, so it needs a new, explicitly reviewed freeze regime.
  5. Membership ambiguity and capacity lifetime need re-measurement.
- **Stop condition met:** the unit decision (C) is supported by the documents, but the *segmentation parameters* and schema 3 are governance decisions. **Recommendation:** implement D4 now (small, provable, no unit change). Adopt the D5 contract below now. Implement D5 as a versioned lifecycle, ideally able to run in **dual-lifecycle shadow** (both lifecycles from one stream, two artifacts) for at least one prospective session before switching the default.

**Minimal implementation plan** (when authorized):
1. `opportunity.rs:746-756`: add `SetupInactivity` and `Invalidated`; keep `Inactivity` for legacy.
2. `opportunity.rs:75`: set schema to 3. `:383-396` and `:366`: add `lifecycle` to `OiVersions`. `OiConfig` (`:125-170`): add `lifecycle: Lifecycle { SymbolActivityV1, MoveV1 }` and `move_inactivity_secs`, both included in the fingerprint.
3. `opportunity.rs:1380-1405`: add `edge: HashMap<String, EdgeState>` (bounded by the universe). Evict entries on a date change.
4. `opportunity.rs:1943-1962`: split into `opening_edge(event, &mut EdgeState) -> Option<Strategy>` (flip semantics, mirroring `live_signals.rs:81-130`) and `relevant_evidence(event) -> Evidence {Positive, Invalidation, None}`.
5. `Opportunity` (`:758-821`): add `last_relevant_at`, `last_evidence_kind`, `opened_phase`. Re-key `by_last_seen` (`:1394`) to `(last_relevant_at, symbol)` under `MoveV1`.
6. `observe` `:1467-1509` and `record` `:1579-1666`: market events update price and features only; evidence refreshes `last_relevant_at`; opening requires an edge.
7. `expire_inactive` `:1668-1703`: use the `last_relevant_at` clock; choose the label from `last_evidence_kind`; make the SessionBoundary `closed_at` non-inverting.
8. D4 plumbing (independent of D5): `opportunity_shadow.rs:251-299,301-302` returns the closes; `opportunity_outcomes.rs:279-321` gets `apply_closures` before `settle_due`; `opportunity_outcome.rs:239-243,321,499,619-625` gets the enum, first-terminal-wins, the symbol-indexed lookup and the new row fields; `main.rs:421-441` sets the call order; the OI artifact gets an `opportunity_closed` record.
9. Tests: new `opportunity_lifecycle_tests.rs`, plus updates to `opportunity_tests.rs` a1–a7, `opportunity_identity_tests.rs`, `model_freeze.rs` (new regime) and `membership_tests.rs`.

**D5 test matrix (16).** I did not have the brief's own list of 16 cases, so this is my proposed set; map it onto the brief's list.
1. Bars, halt warnings and catalysts alone never keep an opportunity alive past `T_move`.
2. Level-true funnel on every bar does not keep it alive; a false→true flip opens it.
3. Momentum qualifies:true readings keep it alive; qualifies:false does not.
4. Confirm→reject→confirm within `T_move` stays one opportunity, with absorbed count 1.
5. A reject with no positive evidence for `T_move` closes as `Invalidated` at `last_relevant + T_move`.
6. Silence of relevant evidence gives `SetupInactivity`, even while bars continue.
7. A second setup after a close gets a new id; ids are distinct, and the second `move_before_detection` and `detection_context` are frozen at the second open.
8. Overlapping detectors (ignition plus consolidation plus momentum at the same instant) give one opportunity, confluence 3, and no duplicate.
9. Premarket→regular with continuous evidence stays one opportunity with `openedPhase = premarket`.
10. Premarket evidence, then quiet for `T_move`, then regular evidence gives two opportunities.
11. Regular→after-hours continuation stays one opportunity.
12. A UTC date change closes via `SessionBoundary` with `closedAt >= openedAt`, and the next-day event opens a new id.
13. Capacity eviction under `MoveV1` stays explicit and counted.
14. `finish` gives `CaptureEnded`.
15. Causality: replaying any prefix is identical regardless of later events, and no rule reads future prices (extends b13).
16. Determinism and uniqueness: replaying twice is byte-identical; a restart split gives no id reuse; out-of-order equal-ms reopen is caught.

---

## Appendix — D4 terminal paths and tests

### D4.1 Every terminal path in the OI engine (`crates/backtest-metrics/src/opportunity.rs`)

All paths go through `close()` (`:1739-1755`). It removes the opportunity from `open` and `by_last_seen`, clamps `closed_at = at.max(opened_at)`, sets `close_reason`, and returns `Some(op)` **exactly once**, because `open.remove` makes a second close impossible.

| Path | Code | Trigger | `closed_at` written | Instant the close becomes known (`closeObservedAt`) |
|---|---|---|---|---|
| Inactivity | `expire_inactive` `:1668-1703`. Runs at the top of **every** `observe` (`:1475`). | `last_seen_at <= received_at − 300` (the index range `:1680-1684`). Note `last_seen_at` is event time and the cutoff is receipt time. | `last_seen_at + 300 s` (backdated) | `received_at` of the first event (of **any** symbol) processed at or after the deadline |
| Session boundary, via expiry | `expire_inactive` `:1692-1693` | Same trigger, plus `opened_at.date() != now.date()` (receipt clock) | `last_seen_at` (backdated; **inversion risk D4-5**) | Same as above |
| Session boundary, via event | `observe` `:1481-1490` | A same-symbol event with `at.date() != opened_at.date()` (event clock) | event `at` | `received_at` of that event |
| Capacity | `enforce_capacity` `:1708-1737`. Called only just before `open_new` (`:1505`). | `open.len() >= 16,375`. Evicts the least-recently-active. Structurally unreachable while the universe is ≤13,100 symbols. | event `at` | `received_at`. Also a `CapacityEviction` marker is persisted (`opportunity_shadow.rs:268-270`) |
| Capture end | `finish` `:1758-1764`. Called from `ShadowDriver::finish` (`opportunity_shadow.rs:301-302`) on `RecvError::Closed` only (`main.rs:434-441`) | graceful shutdown | `at` = shutdown `now` | same |
| Invalidation | **Not terminal.** `observe` `:1492-1500` absorbs `FollowThroughRejected`: `invalidations_absorbed += 1` and `episode_fragments += 1` (`:1618-1623`). | — | — | — |
| Replacement / supersession | **Does not exist.** `open_new` runs only when the symbol has no open opportunity (`:1497-1506`). | — | — | — |
| Process crash / kill | Not a path. Open opportunities vanish with no record. Outstanding anchors vanish **with no row** (existing gap, outside D4). | — | — | — |

Enum values today: `OpportunityCloseReason = {Inactivity, SessionBoundary, CaptureEnded, CapacityReached}` (`:746-756`), serialized in snake_case.

### D4.2 How the outcome collector binds anchors and settles them

- Live loop, per event (`main.rs:414-428`):
  1. `outcomes.observe_price(event, now)` publishes a forward price to every anchor for that symbol.
  2. `driver.observe(event, now)` runs `engine.observe` (expiry, close, open, record) and then `engine.rank(now)`.
  3. `outcomes.anchor_and_settle(&snapshots, now)` creates one anchor per snapshot, then calls `settle_due(now)`.
- Binding: `AnchorRequest { opportunity_id, window_id, symbol, session_date, anchor_at = snapshot.timestamp (= now), signal_price, opened_at, session_end, provenance }` (`opportunity_outcomes.rs:279-303`). The anchor is keyed `(anchor_at, next_id)` (`opportunity_outcome.rs:591`) and is also indexed by symbol (`by_symbol` `:536`). **There is no index by opportunity.** Every anchor refers to an opportunity that was open at `anchor_at`, because `rank` iterates `self.open` only (`opportunity.rs:1787-1790`).
- Settlement: `OUTCOME_SETTLE_AFTER_SECS = 1200 + 120 = 1320` (`opportunity_outcome.rs:95`). `settle_due(now)` finishes every anchor with `anchor_at <= now − 1320` (`:630-642`). Forward observations after `anchor_at + 1320` are ignored (`:360`), so the measured window does not depend on cadence.
- Other row writers: anchor-capacity eviction (`:576-600`, censor `PendingCapacityReached`), and `finish` (`:646-656`, censor `CaptureEnded`). `OutcomeDriver::finish` runs **after** `driver.finish` (`main.rs:436-441`).
- Disposition today: `Outstanding.disposition` is initialized to `StillOpen` (`:341`) and copied verbatim into the row (`:499`). Since there is no production writer, it is always `still_open`.

### D4.3 Two lifecycles, kept separate

- **Opportunity lifecycle.** Open → (record)* → one terminal close with a reason. It is owned by `OpportunityIntelligence`, and its identity is `opportunityId`.
- **Outcome settlement lifecycle.** Anchor → price observations → one row, at settle (`anchor_at + 1320`), anchor-capacity eviction, or capture end. It is owned by `OpportunityOutcomeCollector`, and its identity is the anchor `(opportunityId, windowId)`.
- The anchor **must keep measuring after the close** (`opportunity_outcome.rs:28-35`, the `anchor_survives_its_opportunity` test). Disposition is provenance and **never a censor**. That rule is correct and should stay.
- So disposition is a statement about the opportunity **as known at the instant the row is settled**. It cannot be "the eventual terminal state", because a close after settlement is future information for that row, and the row has already been written.

### D4.4 Design: how the close reaches the collector causally

1. **Emit.** `ShadowDriver::observe` returns the closed `Vec<Opportunity>` along with the snapshots, and `ShadowDriver::finish` returns its closed set. Stop discarding at `opportunity_shadow.rs:263,302`.
2. **Carry.** Define a `ClosureNotice { opportunity_id: String, opened_at, closed_at, close_observed_at /* = received_at of the processing event */, reason: OpportunityCloseReason }`.
3. **Order, inside one loop iteration** (`main.rs:414-428`): `observe_price` → `driver.observe` (produces closes C and snapshots S) → **`outcomes.apply_closures(C)`** → `anchor(S)` → `settle_due(now)`. Closures are applied **before** settling, so a close observed at `R` is visible to any anchor that settles at `R`.
4. **Causal rule (cadence-free).** For an anchor with deadline `D = anchor_at + 1320`, its row's disposition is the notice's reason **iff** `close_observed_at <= D` and the notice was applied before the row was written. Otherwise it is `still_open`.
   - For inactivity: `close_observed_at` is the first processed `now >= last_seen + 300`, and settlement is the first processed `now >= D`. Both are checked against the same receipt clock in the same iteration, with expiry first. So a close with `last_seen + 300 <= D` is always applied in time. The rule depends only on the event stream, never on how often `settle_due` happens to run, so replay is deterministic.
   - Use `close_observed_at`, **not** `closed_at`, for the comparison. `closed_at` can be backdated before `anchor_at` (D4-5). Write both into the row.
5. **Exactly once.**
   - The engine emits each close once (`close()` works through `open.remove`).
   - The collector applies a notice only to anchors that have `req.opportunity_id == id` and `req.opened_at == Some(notice.opened_at)`. The `opened_at` guard defends against any id reuse.
   - The collector is **first-terminal-wins**: it applies only while the anchor's disposition is `StillOpen`. A duplicate notice therefore does nothing. `CaptureEnded` never overwrites an earlier terminal.
   - Lookup goes through `by_symbol[symbol]` (O(anchors for that symbol) ≈ ≤44 per open opportunity at a 30 s cadence), not through the 297k-entry scan in `note_disposition`.
6. **Capture end.**
   - `driver.finish(now)` returns its closes; `outcomes.apply_closures` gives the matching anchors `capture_ended`; then `outcomes.finish(now)` writes rows with censor `CaptureEnded`.
   - Rows settled before shutdown are unaffected.
7. **Restart.** Both engines are in-memory and are rebuilt together.
   - No notice crosses processes. Pre-restart anchors are either settled, or (on a graceful shutdown) written as `capture_ended`; on a crash they are lost, which is an existing gap.
   - Post-restart opportunities get new ids, because `sequence_for(opened_at)` is strictly later.
   - Exactly-once holds per process. No persistence is needed for D4.
8. **Race cases, stated plainly.**
   - A close observed before `D`: the row gets the reason.
   - A close observed after `D`: the row says `still_open`, which is correct at its own timestamp.
   - A close observed after an earlier horizon (for example 300 s) but before 1200 s: the row gets the reason. Horizon values are **unchanged**, because closure never censors.
   - Close and settle in the same iteration: the close is applied first, so the row gets the reason.
9. **Persistence, needed for historical replay going forward.** Also write each `ClosureNotice` into the OI artifact as a compact record or marker (`opportunity_closed`). That lets `oi_outcome_replay` reproduce dispositions offline. Without it, D4 is live-only forever (D4-2).

### D4.5 One canonical disposition enum (observable labels only)

Keep the type name `OpportunityDisposition`. The variant set becomes the engine's close reasons plus `StillOpen`, with the same snake_case tokens so there is **no translation table**:

```text
still_open        -- no close observed at or before anchor_at + 1320 (as of settlement)
inactivity        -- OpportunityCloseReason::Inactivity
session_boundary  -- OpportunityCloseReason::SessionBoundary
capacity_reached  -- OpportunityCloseReason::CapacityReached   (replaces `capacity_evicted`)
capture_ended     -- OpportunityCloseReason::CaptureEnded
```

- Add `impl From<OpportunityCloseReason> for OpportunityDisposition`. Also add a compile-time or unit exhaustiveness test, so adding a close reason (for example the D5 reasons below) fails the build until it is mapped.
- Drop `closed`: it is not observable as a reason.
- Drop `capacity_evicted`: it is renamed to the engine token.
- Neither was ever written in production, so removing them loses no data. Keep `#[serde(alias = ...)]` only if a test fixture needs it.
- Invalidation is **not** a label under today's engine, because it is not terminal. It becomes one only if D5 makes it terminal.
- Row additions, all optional:
  - `opportunityClosedAt`
  - `opportunityCloseObservedAt`
  - `opportunityDispositionBasis: "as_of_settlement"`: this states the semantics, and it can be a doc comment instead of a field.

### D4.6 Tests required (D4)

1. **Inactivity.**
   - An anchor at `t`; the last event for the symbol at `t+10`; other symbols keep the clock moving.
   - The close is observed at about `t+310`, so the row at `t+1320` has `inactivity` and `closedAt = t+310`, and every horizon is still observed.
2. **Session close.**
   - Via event: a next-date event for the symbol gives `session_boundary`.
   - Via expiry across midnight: this also asserts that `closeObservedAt >= anchor_at` even when `closedAt < anchor_at` (the D4-5 regression).
3. **Capacity.** Use a small-capacity config and force an eviction. The row has `capacity_reached`, and the `CapacityEviction` marker is still emitted.
4. **Capture end.**
   - `finish` gives the open opportunity's outstanding anchors `capture_ended`, plus censor `CaptureEnded`.
   - An anchor whose opportunity had already closed keeps its earlier reason (first-terminal-wins).
5. **Invalidation (today).** A `FollowThroughRejected` does **not** produce a disposition, and the row stays `still_open`. This pins the absorb rule until D5 changes it.
6. **Still open.** The opportunity is active through the whole settlement window, so the row has `still_open`.
7. **Disposition before settlement.** A close at `anchor+100`: the row has the reason, and it is byte-identical in returns, excursion and targets to a no-close control. This extends `measurement_is_independent_of_everything_but_price`.
8. **After an earlier horizon but before 1200 s.** A close at `anchor+650`: the row has the reason, and the 30/60/120/300/600/1200 values equal the control's.
9. **Close after settlement.** A close at `anchor+1400`: the row already written says `still_open`, and no second row is written.
10. **Duplicate close.** The same notice is applied twice, and a conflicting reason is applied afterwards: the first terminal stays and the row count is unchanged.
11. **Id and opened_at guard.** A notice with a matching id but a different `opened_at` does nothing.
12. **Several anchors for one opportunity.** Anchors from 5 windows all get the reason; anchors for another opportunity on the same symbol (after a close and reopen) are unaffected.
13. **Restart/replay.**
    - Two `ShadowDriver + OutcomeDriver` pairs are fed the same event stream, one of them split across a simulated restart (finish, then fresh drivers).
    - The rows are deterministic.
    - Pre-restart anchors carry `capture_ended`, and post-restart ids are distinct.
    - A replay built from the persisted `opportunity_closed` records reproduces the live dispositions.
14. **Ordering.** A close and a settlement in the same iteration: the close wins.
15. **Performance.** Applying one notice touches only that symbol's anchors (bounded; the assertion is by counter, not wall-clock).

---

