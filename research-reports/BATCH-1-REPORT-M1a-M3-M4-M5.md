# Remediation Batch 1 — Implementation Report

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909`
**Base commit:** `bafae88` (the audited state)
**Scope:** M1a, M3, M4, M5 only
**Status:** implemented and validated — **not committed, not pushed**

---

## 0. Pre-implementation findings

Three things discovered during inspection materially affected the design. All are reported rather
than silently worked around.

### 0.1 There are two broadcast channels, and the audit conflated them

The audit (and therefore the batch instructions) described a single channel of capacity 1024. In
fact:

| Channel | Location | Original capacity | Subscribers |
|---|---|---|---|
| Upstream `ScanEvent` | `main.rs:57` | 1024 | history collector, live-signal tracker, push notifier |
| Client-facing `EventFrame` | `server.rs:70` | **4096** | every WebSocket client socket |

Client-facing headroom was therefore ~6 seconds, not ~1.5. **Raising only the named constant would
not have helped clients at all**, because a slow socket lags on the second channel. Both were
raised — see M4 below.

### 0.2 Lag was already partially handled; it was not silently swallowed

`server.rs` already responded to `RecvError::Lagged` by resending a retained `EventSnapshot`
(recent alerts plus latest-per-key state), and clients deduplicate by `eventId`. That is a state
resync, not fabricated history.

This existing recovery was **preserved**. Removing it would have been a regression. The batch
constraint "do not fabricate or replay missed market events" is still satisfied: the events that
fell in the gap are never replayed — only current state is restored.

### 0.3 `ClientKind::AutoTrader` deliberately disconnects on lag

`server.rs` bails for the auto-trader on lag, because that client requires historical
reconciliation rather than a snapshot. This behaviour was preserved exactly; the new notification
is sent only to clients that continue.

### 0.4 A startup guard exists that the audit had not recorded

`main.rs:87` already refuses to start any **non-loopback** listener unless
`STOCKSPOTTER_API_TOKEN` is present and at least 32 characters:

```rust
anyhow::ensure!(access::configured_token().is_some_and(|t| t.len() >= 32),
    "non-loopback listeners require STOCKSPOTTER_API_TOKEN (at least 32 characters)");
```

This confirms the release branch README's claim that "network listeners refuse to start without
it", and means M5 hardens a path that is unreachable in a correctly-configured production. That is
exactly the case the instructions anticipated, which is why the fail-closed tests are unit-level.

---

## 1. Files changed (8)

```
 apps/client/src/lib/useRealtimeFeed.ts |  10 ++
 apps/mobile/src/types.ts               |   2 +-
 apps/mobile/src/useRealtimeFeed.ts     |   6 +-
 crates/ws-server/src/access.rs         | 307 ++++++++++++++++++++++++++-------
 crates/ws-server/src/main.rs           |  19 +-
 crates/ws-server/src/protocol.rs       |  18 ++
 crates/ws-server/src/server.rs         |  23 ++-
 packages/shared-types/src/index.ts     |  17 ++
 8 files changed, 325 insertions(+), 77 deletions(-)
```

---

## 2. Behaviour changed

### M1a — `/health` no longer bypasses rate limiting

The rate-limit budget is now charged **before the authentication verdict is returned**, on every
path. `/health` still returns early *after* authentication, preserving its cheap semantics: no
expensive-endpoint budget, no concurrency permit, no 120-second timeout. It remains a fast
liveness/credential check that clients call on every launch.

Behaviour matrix, all verified by test:

| Case | Before | After |
|---|---|---|
| Valid credential | 200 | 200 |
| Invalid credential | 401 | 401 |
| Window exhausted | *(never — unmetered)* | **429** |
| Repeated wrong guesses | unlimited | **metered, then 429** |

The essential property is that **failed** attempts consume the window. A limiter that counts only
successful requests cannot slow an attacker down, which is precisely why the previous arrangement
was an oracle.

Per the batch scope, the limiter was **not** redesigned into per-IP state.

### M3 — Poisoned limiter mutexes no longer cause a permanent outage

Both request-path `.lock().unwrap()` calls were replaced with a single helper:

```rust
fn lock_budget(budget: &Mutex<Budget>) -> MutexGuard<'_, Budget> {
    budget.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
```

To avoid duplicating recovery logic, the counter itself was extracted into a `Budget` struct with
an `admit(max)` method that owns window rollover and limit checking. Both budgets now share one
code path (`admit(&mutex, max)`), so there is exactly one place where locking happens.

Rate-limit semantics after recovery are unchanged: same 60-second fixed window, same 120/6 limits.

No other mutexes anywhere in the project were modified.

### M5 — Authentication fails closed

`authorized()` previously returned `true` when no token was configured, relying entirely on outer
startup and deploy guards. It now returns `false`.

Token configuration is represented explicitly by a new newtype rather than by scattered
empty-string checks:

```rust
pub struct ApiToken(String);   // non-empty by construction
```

- `ApiToken::new` returns `None` for an empty string, so absent and empty collapse into one state.
- `Option<ApiToken>` *is* the answer to "is a token configured?" — no downstream code re-checks.
- Deliberately implements neither `Debug` nor `Display`, and exposes only `len()` (required by the
  existing startup guard). The secret value has no accessor that could reach a log line or an
  error message.

Constant-time comparison for configured credentials is preserved verbatim.

The existing startup validation was **not** weakened — `main.rs:87` is untouched.

### M4 — Broadcast resilience and observable lag

**Capacity.** `BROADCAST_CAPACITY` raised `1024 → 16_384` as specified, **and** the client-facing
channel in `server.rs` changed from a hardcoded `4096` to that same constant. Per finding 0.1, the
second change is what actually delivers the intent; the first alone would not have.

At the measured ~650 events/second this is roughly 25 seconds of headroom, up from ~1.5s (upstream)
and ~6s (client-facing). Cost is only the queued events themselves.

**Lag notification.** A new server-to-client control message:

```rust
#[serde(rename = "stream_lagged", rename_all = "camelCase")]
StreamLagged { missed_events: u64 },
```

serialising as:

```json
{"type":"stream_lagged","missedEvents":123}
```

Note the field name is **`missedEvents`**, not the `missed_events` shown in the batch instructions.
`protocol.rs`'s own header states it mirrors `packages/shared-types/src/index.ts` with camelCase
field names, and `welcome`/`serverTime` sets that precedent. The instructions explicitly permitted
following project convention over the literal example.

On `RecvError::Lagged(n)` the server now:

1. logs the lag exactly as before;
2. disconnects the auto-trader exactly as before (unchanged, first);
3. sends **exactly one** `StreamLagged` carrying Tokio's own dropped count;
4. resends the retained snapshot as before;
5. continues the connection.

Nothing is fabricated, nothing missed is replayed, and lag is no longer hidden from the client.

**Client handling.** Both clients were wired to the existing connection-status mechanism, since
each already had a `"stale"` state and the change was two lines: on lag they mark the feed stale,
and the next event carrying a fresh timestamp clears it. Richer UI indication is deliberately left
to a later client-focused batch.

---

## 3. Tests added / modified

| Test | File | Covers |
|---|---|---|
| `fails_closed_when_no_token_is_configured` | `access.rs` | M5 — unconfigured token authorises nothing |
| `an_empty_token_is_not_a_configured_token` | `access.rs` | M5 — empty ≡ unconfigured |
| `requires_exact_bearer_credential_when_configured` | `access.rs` | M5 — **modified**: `authorized(None, None)` now asserts `false` |
| `a_poisoned_budget_still_admits_requests` | `access.rs` | M3 — poisons a real mutex from a panicking thread, asserts recovery |
| `a_budget_refuses_once_its_window_is_exhausted` | `access.rs` | M3 — window logic after refactor |
| `health_participates_in_rate_limiting` | `access.rs` | M1a — valid 200 / invalid 401 / exhausted 429 |
| `failed_health_credential_attempts_consume_the_budget` | `access.rs` | M1a — the oracle test proper |
| `stream_lagged_serializes_with_shared_types_field_names` | `protocol.rs` | M4 — exact wire shape |

The pre-existing `http_auth_and_workload_limits_are_enforced` test was retained and adjusted only
where the `Budget` refactor changed field access.

---

## 4. Validation results

| Command | Result |
|---|---|
| `cargo test -p ws-server` | **28 passed, 0 failed** |
| `cargo test --workspace` | **324 passed, 0 failed** (33 test binaries) |
| `bun --cwd=apps/client run test` | **60 passed, 0 failed** |
| `bun --cwd=apps/client run lint` | **exit 0** — warnings only, all pre-existing |
| `bun --cwd=apps/client run build` | succeeded (chunk-size warnings only) |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **exit 0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **exit 0** |
| `cargo fmt --check` | **fails — pre-existing, see below** |

### `cargo fmt --check` — reported, not worked around

This command **cannot pass on this repository** and did not before this change. **63 files** fail,
including many untouched by this batch: `push.rs`, `http.rs`, `auto_trader_status.rs`, and all of
`market-data`, `backtest-metrics`, `halt-detector`, `ignition-detector`, `replay-engine`,
`consolidation-breakout`, `fast-funnel`, `momentum-scorer` and `auto-trader`.

The codebase is deliberately hand-formatted in a dense style, and `.github/workflows/validate.yml`
runs `cargo test`, never `cargo fmt --check` — so the style was never enforced and has drifted
repo-wide.

`cargo fmt` was **not** run. Doing so would rewrite 63 files, discard the project's intentional
formatting, and bury a 325-line security change in thousands of lines of unrelated churn.

### Lint warning provenance

The client lint emits warnings including one at `useRealtimeFeed.ts:424`. This batch's only hunk in
that file is at lines 211–220, so the warning is outside the change and pre-existing. Lint exits 0.

---

## 5. Compatibility concerns and design tradeoffs

### 5.1 Unauthenticated callers can now consume the shared budget — accepted, with a caveat

Closing the oracle *requires* charging failed attempts. Because the limiter remains global rather
than per-IP (per-IP being explicitly out of scope for this batch), an unauthenticated flood can now
exhaust the shared allowance and deny service to legitimate users.

This is a **real availability regression under attack**, accepted deliberately as strictly better
than an unlimited credential-guessing oracle. **Per-IP throttling is the actual fix for both
problems**, and sequencing that batch soon is recommended.

### 5.2 The new message is additive but not invisible to typed clients

`RealtimeMessage` gained a member, so TypeScript exhaustiveness forced explicit handling in both
clients. This is why `apps/mobile/src/types.ts` required `"stream_lagged"` in the `DetectionEvent`
exclusion list — without it the lag notification would have been typed as a detection event and
pushed into the events array.

Untyped or older clients are unaffected: they receive an unknown `type` and, in both current
implementations, would simply not match any handler.

### 5.3 Internal signature change

`await_hello`'s `expected_token` parameter changed from `Option<&str>` to `Option<&ApiToken>`.
Crate-internal only; no wire-format or API change.

### 5.4 Preserved contracts

- HTTP authentication contract — unchanged
- WebSocket authentication handshake — unchanged
- Existing event payloads — unchanged
- `/health` semantics used by both `AccessGate` implementations — unchanged
- Deploy-script health checks — unchanged
- Constant-time token comparison — unchanged
- No password scheme introduced
- No per-IP state introduced
- No production secrets touched

---

## 6. `git diff --stat`

```
 apps/client/src/lib/useRealtimeFeed.ts |  10 ++
 apps/mobile/src/types.ts               |   2 +-
 apps/mobile/src/useRealtimeFeed.ts     |   6 +-
 crates/ws-server/src/access.rs         | 307 ++++++++++++++++++++++++++-------
 crates/ws-server/src/main.rs           |  19 +-
 crates/ws-server/src/protocol.rs       |  18 ++
 crates/ws-server/src/server.rs         |  23 ++-
 packages/shared-types/src/index.ts     |  17 ++
 8 files changed, 325 insertions(+), 77 deletions(-)
```

## 7. `git status --short`

```
 M apps/client/src/lib/useRealtimeFeed.ts
 M apps/mobile/src/types.ts
 M apps/mobile/src/useRealtimeFeed.ts
 M crates/ws-server/src/access.rs
 M crates/ws-server/src/main.rs
 M crates/ws-server/src/protocol.rs
 M crates/ws-server/src/server.rs
 M packages/shared-types/src/index.ts
?? .claude/
?? AUDIT-2026-09-09.md
```

The two untracked entries were left untouched and unstaged. **Nothing was staged at all**, and no
commit was created.

---

## 8. Key changed code

### `crates/ws-server/src/access.rs`

```rust
/// Non-empty by construction, so `Option<ApiToken>` alone answers "is a
/// token configured?". No Debug/Display: the value must never reach a log.
pub struct ApiToken(String);

impl ApiToken {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        (!raw.is_empty()).then_some(Self(raw))
    }
}

/// Rate-limit counters are disposable operational state. `lock().unwrap()`
/// would turn one panic under this lock into a permanent outage: every later
/// request would panic on the poison until the process was restarted, while
/// a plain TCP health check still looked fine.
fn lock_budget(budget: &Mutex<Budget>) -> MutexGuard<'_, Budget> {
    budget.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Fails closed: with no token configured, NOTHING is authorized.
pub fn authorized(header: Option<&str>, token: Option<&ApiToken>) -> bool {
    let Some(token) = token else {
        return false;                       // was: return true
    };
    let Some(value) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let token = token.as_str();
    value.len() == token.len()              // constant work, unchanged
        && value.bytes().zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

pub async fn protect(State(access): State<Access>, req: Request, next: Next) -> Response {
    // Charged BEFORE the auth verdict, on every path including /health --
    // a limiter that counts only successes cannot slow an attacker down.
    if !admit(&access.budget, REQUESTS_PER_MINUTE) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if !authorized(/* ... */, access.token.as_ref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // Still cheap past this point: no expensive budget, no permit, no timeout.
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    /* expensive budget, concurrency permit, 120s timeout — unchanged */
}
```

### `crates/ws-server/src/protocol.rs`

```rust
/// This connection's receiver fell behind and the server dropped
/// `missed_events` events for it. Sent once per lag occurrence, and never as
/// a substitute for data -- the dropped events are gone and are not replayed.
#[serde(rename = "stream_lagged", rename_all = "camelCase")]
StreamLagged { missed_events: u64 },
```

### `crates/ws-server/src/server.rs`

```rust
// Sized by the same constant as the upstream channel in `main` -- raising
// only that one would have done nothing for clients, since a slow socket
// lags *here*.
let (live_tx, _) = broadcast::channel::<EventFrame>(crate::BROADCAST_CAPACITY);

// ...

Err(broadcast::error::RecvError::Lagged(skipped)) => {
    warn!(%peer, skipped, "recovering retained events for lagging client");
    if client == ClientKind::AutoTrader {
        anyhow::bail!("trader feed gap requires historical reconciliation");
    }
    // The resend restores current state but cannot restore an event that
    // came and went inside the gap. A client never told would render a
    // complete-looking picture with holes in it, and a missed signal would
    // be indistinguishable from one that never fired.
    send_json(&mut ws, &HandshakeMessage::StreamLagged { missed_events: skipped }).await?;
    let history = snapshot.lock().await.frames();
    for frame in history { send_json(&mut ws, &frame).await?; }
}
```

### `packages/shared-types/src/index.ts`

```ts
export interface ServerStreamLagged {
  type: "stream_lagged";
  missedEvents: number;
}

export type RealtimeMessage =
  | ClientHello
  | ServerWelcome
  | ServerHelloRejected
  | ServerStreamLagged      // added
  | Ping
  | Pong
  /* ...detection events unchanged... */;
```

---

## 9. Scope confirmation

**Implemented:** M1a, M3, M4, M5 — and nothing else.

**Explicitly not touched**, as instructed: per-IP authentication throttling, WebSocket failed-auth
throttling, Docker `USER` directives, CI / release-branch changes, dependency scanning,
monitoring/alerting, credential rotation, documentation cleanup.

**Not done:** no commit, no push, no deployment, no SSH to production, no credential changes.

Awaiting review or Batch 2 instructions.
