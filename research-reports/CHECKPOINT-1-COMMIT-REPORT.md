# Remediation Checkpoint 1 — Commit Report

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909`
**Base audited commit:** `bafae88`
**Checkpoint commit:** `725179b21227b1c1ba1058ca04679a82dfaa93a3`
**Status:** committed locally — **not pushed, not deployed**

> **Note on sequencing:** the checkpoint instruction was received three times. The commit was created
> on the first receipt (`3a54c1d`), amended on the second to match a refined message body
> (`725179b`), and the third receipt was verified as already-addressed with **no new commit**. The
> reflog shows one commit, amended once. There is no duplicate.

---

## 1. Commit SHA

```
725179b21227b1c1ba1058ca04679a82dfaa93a3
```

## 2. Commit subject

```
Harden authentication and realtime stream resilience
```

### Full message

```
Harden authentication and realtime stream resilience

- add per-IP failed-auth throttling across HTTP and WebSocket
- derive client identity safely behind the Caddy/Docker proxy path
- hard-bound authentication limiter state under saturation
- recover poisoned rate-limit mutexes instead of panicking
- fail authentication closed when no API token is configured
- increase broadcast buffers and surface stream lag to clients
- preserve retained-snapshot resync and auto-trader lag semantics

The proxy trust fallback remains intentionally dependent on the
loopback-only container publishing used by the current deployment.
STOCKSPOTTER_TRUSTED_PROXIES can narrow that boundary explicitly.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

Body matches the approved text verbatim.

---

## 3. Files included (9)

```
apps/client/src/lib/useRealtimeFeed.ts
apps/mobile/src/types.ts
apps/mobile/src/useRealtimeFeed.ts
crates/ws-server/src/access.rs
crates/ws-server/src/http.rs
crates/ws-server/src/main.rs
crates/ws-server/src/protocol.rs
crates/ws-server/src/server.rs
packages/shared-types/src/index.ts
```

Each was staged **explicitly by pathname**. No `git add .`, no `git add -A`, no wildcard or
directory-wide staging at any point.

Every file was verified against an explicit allow-list of the M1/M3/M4/M5 surface before committing;
nothing outside that surface was present in the working tree.

### What each file contributes

| File | Findings |
|---|---|
| `crates/ws-server/src/access.rs` | M1 (per-IP limiter, client-IP derivation, trusted proxies), M3 (poison recovery), M5 (`ApiToken`, fail-closed) |
| `crates/ws-server/src/server.rs` | M1 (WS throttling), M4 (client channel capacity, `stream_lagged` emission) |
| `crates/ws-server/src/main.rs` | M1 (shared limiter wiring), M4 (`BROADCAST_CAPACITY = 16_384`) |
| `crates/ws-server/src/http.rs` | M1 (`ConnectInfo` plumbing, limiter wiring) |
| `crates/ws-server/src/protocol.rs` | M4 (`StreamLagged` wire message) |
| `packages/shared-types/src/index.ts` | M4 (`ServerStreamLagged`, union member) |
| `apps/client/src/lib/useRealtimeFeed.ts` | M4 (client lag handling) |
| `apps/mobile/src/useRealtimeFeed.ts` | M4 (client lag handling) |
| `apps/mobile/src/types.ts` | M4 (`DetectionEvent` exclusion) |

---

## 4. Diff check results

| Check | Result |
|---|---|
| `git diff --check` (pre-stage) | **exit 0**, no output |
| `git diff --cached --check` (post-stage) | **exit 0**, no output |
| `git diff HEAD^..HEAD --check` (post-commit) | **exit 0**, no output |

No whitespace errors, no conflict markers, no defects reported — so nothing required correction and
**no repository-wide formatting was performed**.

---

## 5. `git show --stat --oneline HEAD`

```
725179b Harden authentication and realtime stream resilience
 apps/client/src/lib/useRealtimeFeed.ts |  10 +
 apps/mobile/src/types.ts               |   2 +-
 apps/mobile/src/useRealtimeFeed.ts     |   6 +-
 crates/ws-server/src/access.rs         | 963 ++++++++++++++++++++++++++++++---
 crates/ws-server/src/http.rs           |  15 +-
 crates/ws-server/src/main.rs           |  28 +-
 crates/ws-server/src/protocol.rs       |  18 +
 crates/ws-server/src/server.rs         | 145 ++++-
 packages/shared-types/src/index.ts     |  17 +
 9 files changed, 1108 insertions(+), 96 deletions(-)
```

## 6. `git diff bafae88..HEAD --stat`

```
 apps/client/src/lib/useRealtimeFeed.ts |  10 +
 apps/mobile/src/types.ts               |   2 +-
 apps/mobile/src/useRealtimeFeed.ts     |   6 +-
 crates/ws-server/src/access.rs         | 963 ++++++++++++++++++++++++++++++---
 crates/ws-server/src/http.rs           |  15 +-
 crates/ws-server/src/main.rs           |  28 +-
 crates/ws-server/src/protocol.rs       |  18 +
 crates/ws-server/src/server.rs         | 145 ++++-
 packages/shared-types/src/index.ts     |  17 +
 9 files changed, 1108 insertions(+), 96 deletions(-)
```

Identical to the commit stat, and `git log --oneline bafae88..HEAD` returns a single line — this
checkpoint is the **only** delta from the audited base.

## 7. `git status --short`

```
?? .claude/
?? AUDIT-2026-09-09.md
```

Working tree otherwise clean.

---

## 8. Explicit confirmations

| Claim | Verified how | Result |
|---|---|---|
| `.claude/` remains untracked | `git ls-files --error-unmatch .claude` fails | ✓ |
| `AUDIT-2026-09-09.md` remains untracked | `git ls-files --error-unmatch` fails | ✓ |
| Neither was committed | `git show --name-only --format= HEAD \| grep` returns nothing | ✓ |
| No unrelated tracked files committed | All 9 matched against explicit allow-list | ✓ |
| Nothing was pushed | `upstream:[]`; remote refs are only `origin/master`, `origin/HEAD` | ✓ |
| No upstream configured | `git for-each-ref` shows empty upstream | ✓ |
| No deployment occurred | No deploy command run | ✓ |
| No production access occurred | No SSH; no credential changes | ✓ |

---

## 9. What this checkpoint contains

### M1 — per-IP authentication-abuse throttling (HTTP + WebSocket)

- Spoof-resistant `effective_client_ip(peer, headers)`; forwarded headers ignored from untrusted
  peers entirely.
- `STOCKSPOTTER_TRUSTED_PROXIES` (bare IPs and CIDRs, no new dependency) replaces the heuristic when
  set; private-range fallback retained for the current Docker topology.
- 10 failed attempts per IP per 60s, shared across HTTP and WS so switching protocol buys nothing.
- Successful authentications are never counted and never reset a counter.
- `429` with `Retry-After` on HTTP; `hello_rejected` with a coarse reason on WS.
- Limiter state hard-bounded at 4096 IPs, with fail-closed behaviour on saturation.

### M3 — poisoned mutex recovery

`lock_recover<T>` replaces every request-path `.lock().unwrap()`, covering both workload budgets and
the new limiter map. A panic under any of these locks can no longer brick the request path.

### M4 — broadcast resilience and visible lag

`BROADCAST_CAPACITY = 16_384` applied to **both** the upstream channel and the client-facing channel
in `server::run` (which was separately hardcoded at 4096). New `stream_lagged` control message
carries Tokio's dropped count. Retained-snapshot resync and auto-trader disconnect-on-lag semantics
preserved unchanged.

### M5 — fail-closed authentication

`authorized()` returns `false` when no token is configured. `ApiToken` newtype is non-empty by
construction, implements neither `Debug` nor `Display`, and exposes only `len()`. Constant-time
comparison and the `main.rs` startup guard are unchanged.

### Batch 2A corrections included

- Corrected Caddy/`X-Forwarded-For` reasoning (the original premise was wrong; rightmost selection
  retained because it is safe under both Caddy behaviours).
- Explicit `STOCKSPOTTER_TRUSTED_PROXIES` support.
- Safe fallback retained for the loopback-published Docker topology.
- Hard-bounded limiter state — this fixed a **real defect** where the map could grow past its cap.
- Fail-closed behaviour when limiter state is saturated.
- Adversarial saturation tests (4096 active + 10,000 more, bound asserted every iteration).

---

## 10. Validation backing this checkpoint

Run before the commit, on the exact tree that was committed:

| Command | Result |
|---|---|
| `cargo test -p ws-server` | **51 passed, 0 failed** |
| `cargo test --workspace --no-fail-fast` | **347 passed, 0 failed**, 0 SIGKILLs |
| `bun --cwd=apps/client run test` | **60 passed, 0 failed** |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **exit 0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **exit 0** |

`cargo fmt --check` remains failing repo-wide (63 files) from pre-existing drift unrelated to this
work; deliberately not addressed.

---

## 11. Carried-forward caveats

Two items are recorded rather than resolved, both stated in the commit message or in code comments:

1. **The deployed Caddy version is unverified.** Nothing in the repository pins it and no SSH was
   performed. The implementation is deliberately correct under both the current documented
   behaviour (incoming `X-Forwarded-*` ignored) and the older appending behaviour.

2. **The proxy trust boundary is wider than ideal by default.** It is safe only because
   `ops/vps/docker-compose.yml` publishes ports on `127.0.0.1` alone. If those were ever published
   on `0.0.0.0`, `STOCKSPOTTER_TRUSTED_PROXIES` becomes mandatory and the fallback must not be
   relied upon. This is documented in `is_trusted_proxy`'s doc comment.

**Deployability gate (audit finding H1):** this is a `release/*` branch, which `ops/vps/deploy.sh`
accepts without CI ever running — `validate.yml` triggers only on `master` and pull requests. This
commit is verified locally but would reach production without `validate.yml` seeing it. Closing that
gap is the stated next batch.

---

## 12. Next

Awaiting separate instructions for **H1 + M6** — CI coverage for the actual release/remediation
lineage, plus dependency vulnerability scanning. No work has begun on those, nor on M2, M7, H2, H3,
or any Low finding.
