# Collector capacity amendment — 2026-10-13

This prospective operational amendment replaces the 256-symbol LRU policy in
IAC-EXTREME-MOMENTUM-V1.md. The original contract, pure detector, thresholds,
evaluator, captured datasets and measurement receipts remain byte-identical.
It does not revise the historical efficacy results or widen upstream subscriptions.

The live collector was pinned at 256 symbols with 6,925 capacity evictions.
A 512-symbol sequential minute cycle reproduced continual resets: no history
survived beyond one bar. The replacement holds at most 16,384 monitors, each with
the unchanged 64-bar bound. New symbols at capacity are rejected explicitly;
continuously observed incumbents are not evicted to admit them. No linear LRU
scan runs on each bar. This ceiling provides bounded headroom for the observed
feed; it does not assert that all market symbols supply finalized minute bars.

The existing source-idle rule still invalidates history after more than 120s
without a valid receipt. Idle entries are reclaimed only after more than the
default 600s cooldown since their last receipt of a finalized update (including
duplicates or invalid updates). Receipt regressions cannot shorten that lease.
This conservative lease
preserves cooldown state; invalid inputs do not become retrospective bars.
Lag, revision, missing-minute, invalid-input and market-day resets remain intact.

Optional additive status fields report the symbol limit, per-strategy histories
ready for the **next** evaluation, rejected updates, bounded recent excluded
symbols, idle resets/reclaims, sticky accepted/warmup/duplicate/revision/gap/invalid
totals, and aggregate missed research updates across connected clients. `evictions`
continues to mean capacity eviction and stays zero under the new policy.
`healthy` describes subscriber operation, not complete market coverage. Readiness
is calculated outside the hash-pinned core from its public counter deltas and
configuration: IAC needs 25 prior contiguous bars, Extreme needs 22. Day changes
reset readiness even though the core does not increment a gap counter for them.

The subscribed feed remains limited: ordinary finalized bars are emitted for
momentum-tracked symbols; updated bars can also reach the collector. Wildcard
trade coverage does not provide wildcard finalized bars. Quiet-only or briefly
subscribed symbols may never accumulate enough continuous history. No historical
backfill is injected into live detectors to disguise that limitation.

Qualification requires the >256-symbol detection regression with an old-LRU
negative control, direct-core event equivalence, overflow stability, cooldown,
idle/revision/gap/day/lag tests, unchanged full captured-bar replay, browser
coverage-state checks, and a release-mode 16,384-symbol × 64-bar load run.
The latter requires p99 below the existing 5ms warning threshold, harness peak
RSS below 512MiB on Linux, and an idle sweep below 250ms. Recorded timings qualify
the test host only; running-server coverage and budget counters must be checked.
The research output channel stays 128 and the retained event ring stays 100.
Normal feed consumers, frozen capture settings, orders and notifications are unchanged.
