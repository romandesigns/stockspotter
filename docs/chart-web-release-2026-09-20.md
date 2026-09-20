# Chart web release — September 20, 2026

## Scope and decision

Based directly on running production `7e365866ce5ae18ea77cce05d6d8f45bc69c7faa`,
including the V2.1 prospective measurement foundation. Ported only incremental
series updates and live viewport preservation from audit commit `0c5a25b`.
Replay explicitly retains its prior progressive auto-fit behavior. Historical
corrections, eviction, timeframe changes and empty resets still replace complete
series; indicator formulas and candle inputs do not change.

**Excluded:** the 50 ms publication cadence, trailing/rollover aggregation,
late-trade buckets, eligibility/idle-loop changes, native client and every
backend, ranking, detector, execution, configuration and qualification change.
The full audit remains an isolated candidate, not an approved backend release.

## Validation and measured benefit

- 63 client tests, 518 assertions pass on this release, including OHLC/style
  updates, historical correction, empty clearing and replay-shaped resets.
- Client lint exits successfully with existing warnings; TypeScript and Vite
  production build pass. No dependency/lockfile changes.
- Real browser parity: 7 input scenarios across all 12 series, zero differences
  from full replacement. Release engine file matches the measured audit engine
  byte-for-byte (SHA256 `664b80ab9d701d80154d48c6161467e62f3c65d158fcbc4296aeb197d9b41b67`).
- Isolated 500-bar engine benchmark, Chromium with 4x CPU slowdown, 200 samples:
  p50 **10.5 → 3.4 ms**, p95 **13.4 → 4.8 ms**, p99 **15.2 → 5.4 ms**,
  max **20.1 → 5.6 ms**. User range `[100,200]` remains unchanged; baseline
  resets it to `[0,501]`. These are local submission costs, not production latency.
- The audit's source-to-canvas p99 **500.1 → 60.6 ms** includes excluded backend
  changes and **must not be attributed to this web-only release**.

Full architecture, source replay, OHLCV boundary/premarket findings, reconnect
gaps and mobile limitations are in `docs/chart-fidelity-latency-audit-2026-09-20.md`
on branch `audit/chart-fidelity-20260920` (commit `0c5a25b`).

## Frozen experiment and Monday analysis

The prospective-session runbook requires production checkout HEAD, deployment
marker, and running completeness commit to agree. Keep all at `7e36586`.
The observed OI fingerprint is `oi-cfg-b4f21c8b311a1b99`. No protected source
file changes; no restart of ws, auto-trader, qualify or discovery-review.

Use a separate Git release worktree and the existing production Compose project
to rebuild/replace **web only**. The scoped script takes the existing deployment
lock, preserves the old image, verifies public served assets against image
contents and records before/after protected container identities and config
checksums. It does not advance the main checkout or `.deployed-commit`; its
separate web revision evidence lives outside that checkout. The deploy timer
therefore remains a no-op. Future full-stack releases must include this web fix
or explicitly supersede it; otherwise a rebuild from the frozen checkout would
restore the old web code.

The inspected systemd/user-cron inventory exposed the deploy timer but no
dedicated prospective-analysis schedule. This release preserves existing jobs;
it does not establish or certify Monday's orchestration. Market-open preflight,
capture and post-close qualification remain the existing operator workflow.

## Deployment and rollback

Push `release/chart-web-20260920` before running its script. Fetch that exact
branch into a separate VPS worktree under `/opt/apps/stockspotter-web-releases/`.
From that clean worktree run `bash ops/vps/deploy-chart-web.sh` (Sunday, before
the frozen market session). The script refuses an unexpected production base
or an unpushed/dirty release. Docker's client build runs tests and build again.

Evidence is `/opt/apps/stockspotter-web-releases/evidence-<web-sha>/` and contains
the protected-state snapshots, old image ID, public asset checksums, served HTML
and successful web revision. Build failure leaves the old container untouched;
verification failure automatically restores the old web image.

Manual rollback, under the production deployment lock, uses the retained image:

```sh
cd /opt/apps/stockspotter
exec 9>.git/stockspotter-deploy.lock
flock -w 30 9
docker tag stockspotter-chart-web:rollback-<web-sha> stockspotter-vps-web:latest
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml up -d --no-deps --no-build --wait web
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml exec -T qualify python /app/check_health.py
```

Never run a full-stack deployment/rollback during the frozen session. This
web-only scope leaves known sparse-tail/rollover loss, reconnect incompleteness,
REST/live provenance, and native rendering costs unresolved. An already open
browser tab needs a normal reload to load the new hashed bundle. Real market
latency and physical mobile performance remain unmeasured.
