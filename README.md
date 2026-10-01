# Step 4 campaign ledger: code-free, append-only

This orphan branch holds **only** the Step-4 campaign record. It shares no
history with any code branch, so it can never be deployed.

**Never** amend, rebase, force-push or rewrite it. Every campaign event is
its own commit, pushed as soon as it is made. GitHub's push time is the
independent evidence that a designation preceded its deadline.

`* -text` keeps git from ever rewriting these bytes.

| File | Holds |
|---|---|
| `campaign/step4-campaign-ledger.ndjson` | The frozen `observation::campaign` event log: one canonical JSON event per line (`designated`, `captureRecorded`, ...). The state is whatever replaying it produces. |
| `campaign/step4-operational-skips.ndjson` | Operating-rule record (not a campaign event): a regular session that could not be designated prospectively, with the reason. Schema `step4-operational-skip-v1`. |

## Bound identities (freeze generation 2)

| Item | Value |
|---|---|
| Implementation | `de5b55e66bd77b70fa7a026d178fb09634704264` |
| Preregistration JCS | `746446819af285d20a749a81eb27cf8984d7d488860beb4f8f575106942be7f8` |
| Freeze manifest JCS | `ef5075a16d1bf44e41d8bc11e7892299e30905ac86264417799c5e82b660dbf2` |
| Artifacts | `freeze/step4-preregistration-20260930` @ `4acb2fea5a944108d32a5eb2477f9869008ed341` |
| OI fingerprint | `oi-cfg-73ccdbaf661996ed` |
| Campaign start | **2026-10-05**, the first potentially designatable session; GPT, 2026-10-01 |

## Operating rules (GPT, 2026-10-01)

1. **Every** eligible regular NYSE session from the campaign start, in
   calendar order.
   - A session that cannot be designated prospectively is **skipped, with
     the reason recorded**.
   - It is never replaced by a later session chosen selectively.
   - Weekends and holidays are never designated.
2. **The designation window** for session *d* runs from the 20:10 ET
   boundary that opens *d* (its observer run must already exist) to **04:00
   ET of market day *d***.
3. **One session in flight.** `PendingCapture`: the next `designated` is
   refused until the previous designated session's `captureRecorded` is in
   the ledger.
4. **What designation may never depend on:** signal counts, market
   activity, candidate quality, runners, volatility, news, apparent
   opportunity, or anything outcome-adjacent.
5. **Outcomes stay sealed.** They need closure (10 qualifying sessions) **and**
   an explicit `outcomeFetchAuthorized` event. No tool on this branch, and no
   evaluator command, fetches them.

## Qualification decision (GPT, 2026-10-01)

The frozen ">= 20 discriminating windows per session" gate counts **all
valid discriminating windows** in the Step-4 session (`|P(W)| >= 6`). It adds
no RTH-only restriction. `qualify-session` reports the primary-scope count
alongside, but qualification uses the total. No preregistration change.

Campaign evidence is archived off-box under
`H:\wavystack\stockspotter-research\step4-campaign\sessions\<d>\` (GPT
storage decision, 2026-10-01).

## Tooling

The evaluator is `ws-server step4-eval`, built with `--features offline-eval`
from `topic/step4-offline-evaluator-20261001`. It is never deployed, and it
writes these files only through:

```
ws-server step4-eval replay-campaign    campaign/step4-campaign-ledger.ndjson campaign/step4-operational-skips.ndjson 2026-10-05
ws-server step4-eval record-designation campaign/step4-campaign-ledger.ndjson campaign/step4-operational-skips.ndjson 2026-10-05 <YYYY-MM-DD>
ws-server step4-eval record-skip        campaign/step4-campaign-ledger.ndjson campaign/step4-operational-skips.ndjson 2026-10-05 <YYYY-MM-DD> "<reason>"
ws-server step4-eval record-capture     campaign/step4-campaign-ledger.ndjson <qualification.json>
```

After each `record-*`:

```
git add campaign/ && git commit -m "<event>: <session>" && git push origin campaign/step4-ledger
```

**State at creation:** empty. No session is designated.
