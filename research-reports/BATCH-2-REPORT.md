# Remediation Batch 2 — Report (M1 completion: per-IP authentication-abuse protection)

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909`
**Base audited commit:** `bafae88`
**Status:** implemented and validated — **not committed, not staged, not pushed**

Batch 1's M1a/M3/M4/M5 changes were built on, not rewritten.

---

## 1. Architectural findings from inspection

Four discoveries materially shaped the design.

### 1.1 There was no peer address available to HTTP at all

`http::run` called `axum::serve(listener, router(..))` with **no `ConnectInfo`** anywhere in the
crate. The `protect` middleware had no way to observe the TCP peer, so there was no identity to key
a per-IP limiter by. `.into_make_service_with_connect_info::<SocketAddr>()` was added; this is a
prerequisite for the entire batch, not an incidental change.

### 1.2 Caddy is configured with bare `reverse_proxy` — so `X-Forwarded-For` is appended, not replaced

`ops/vps/Caddyfile.snippet` uses plain `reverse_proxy 127.0.0.1:8788` with no `header_up`
directives. Caddy's default behaviour is to **append** the address it observed to any existing
`X-Forwarded-For`. A client forging `X-Forwarded-For: 1.2.3.4` therefore arrives as
`1.2.3.4, <real client>`.

**Consequence:** the *leftmost* entry is attacker-controlled and the *rightmost* is the one our own
proxy vouched for. The implementation reads the rightmost. Reading the leftmost — the more common
naive choice — would have let an attacker mint a fresh allowance per request by rotating a header,
which is worse than having no limiter at all.

### 1.3 The deployment's trusted peer is **not** loopback — it is a Docker bridge address

This is the finding that most changes the specified design, and it deviates from the batch
instruction's stated assumption ("for the current deployment, loopback proxying is expected").

`ops/vps/docker-compose.yml` publishes `127.0.0.1:8788:8788`. Caddy runs natively on the host and
connects to the host's loopback — but **Docker rewrites the source address on the way into the
container**, so the container observes the bridge gateway, not `127.0.0.1`. This is confirmed by
production logs from earlier today:

```
INFO ws_server::server: client connected, awaiting hello peer=172.20.0.2:51646
```

**Had I trusted loopback only, `effective_client_ip` would have returned the Docker gateway for
every request in production, collapsing every client into a single bucket** — one guesser would
have throttled the entire user base, which is a worse outcome than the vulnerability being fixed.

The trust set therefore includes private/ULA/link-local ranges as well as loopback. This is safe
*specifically because* the ports are published loopback-only, so the sole route into the container
is the host's own proxy path. That dependency is documented in the code, and the compose file
already states the loopback-binding rationale explicitly.

### 1.4 The WebSocket path already had the peer, but no header visibility outside the upgrade callback

`handle_connection` receives `peer: SocketAddr`, but request headers are only reachable inside the
`accept_hdr_async_with_config` callback. The effective IP is therefore resolved *inside* that
callback and handed back through a small `Arc<Mutex<IpAddr>>`; the callback is synchronous, so no
lock is held across an `.await`.

---

## 2. Client-IP trust model implemented

```rust
pub fn effective_client_ip(peer: IpAddr, headers: &HeaderMap) -> IpAddr
```

| Peer | Behaviour |
|---|---|
| **Untrusted** (public address) | Use the TCP peer IP. Forwarded headers are **ignored entirely**. |
| **Trusted** (loopback v4/v6, RFC1918 private, IPv6 ULA `fc00::/7`, link-local) | Use the **rightmost** `X-Forwarded-For` entry; fall back to the peer if absent or unparseable. |

Accepted forwarded formats: bare `IpAddr`, `addr:port`, and bracketed IPv6 (`[2001:db8::1]:44321`).
Anything unparseable falls back to the peer rather than being guessed at.

Only `X-Forwarded-For` is consulted. `X-Real-IP` and `Forwarded` are **not** trusted, since Caddy
does not set them here and honouring headers the proxy doesn't control would widen the spoofing
surface for no benefit.

No generic configurable proxy-trust framework was introduced.

---

## 3. Failed-auth policy

| Property | Value |
|---|---|
| Allowance | **10 failed attempts** per IP |
| Window | **60 seconds**, fixed, per IP |
| Scope | Shared across HTTP **and** WebSocket — one limiter instance |
| Counts | **Only failures.** Successful authentications are never counted. |
| Reset on success | **No** — documented choice, see below |
| Ban | None. The window lapses on its own; there is no permanent block. |
| Storage | In-memory only. No filesystem, no database, no new dependency. |

**Why a successful authentication does not reset the counter:** resetting on success would let
anyone holding one valid credential clear their record between guessing bursts, reducing the
limiter to a formality. Letting the window expire on its own is the safer and simpler rule, and
costs a legitimate client nothing — they authenticate successfully and never accumulate failures at
all.

**Why 10:** a legitimate client holds one key and pastes it once, so even a fat-fingered human stays
far inside this. Ten leaves room for a retry loop or a stale stored key across a couple of devices
behind one NAT, while remaining far below any rate that makes guessing a 48-character token viable.

The limiter is shared between protocols deliberately: a guesser must not obtain a fresh allowance
simply by switching from HTTP to WebSocket.

---

## 4. HTTP authentication flow after Batch 2

```
1. effective_client_ip(peer, headers)
2. auth.retry_after(ip)?  ── Some ──▶ 429 + Retry-After   (credential never examined)
3. authorized(header, token)
4.   on failure ──▶ auth.record_failure(ip) ──▶ 401
5.   on success, path == /health ──▶ handler, return       (no workload charge)
6.   on success, other paths ──▶ global workload budget
                              ──▶ expensive-endpoint budget
                              ──▶ concurrency permit
                              ──▶ 120s timeout
```

A throttled caller is refused **before** its credential is examined, so the response reveals nothing
beyond "not now".

---

## 5. WebSocket authentication flow after Batch 2

```
1. TCP accept  (64-slot connection semaphore — capacity control, NOT auth throttling)
2. upgrade callback:
     a. effective_client_ip(peer, request headers)  ── stored for later stages
     b. auth.retry_after(ip)? ── Some ──▶ reject upgrade with HTTP 429
     c. Authorization header present and invalid ──▶ record_failure ──▶ reject with 401
     d. otherwise ──▶ upgrade proceeds, header-auth result recorded
3. await_hello:
     a. auth.retry_after(ip)? ── Some ──▶ hello_rejected "Too many authentication attempts"
     b. invalid token in hello ──▶ record_failure ──▶ hello_rejected "Unauthorized"
     c. valid ──▶ welcome  (wire protocol unchanged)
```

Rejection is immediate in every case. **No sleeping, no deliberately held-open connections** — the
throttle is expressed purely as a refusal.

The audited weakness (upgrade cheaply → bad token → rejected → reconnect indefinitely) is closed at
two points: the reconnect itself is refused at the upgrade once the IP is throttled, and the hello
stage is guarded independently for connections that upgraded before the allowance ran out.

---

## 6. Bounding stale limiter entries

State is `Mutex<HashMap<IpAddr, Attempts>>` where `Attempts { window_started: Instant, failures: u32 }`.

Two mechanisms keep it bounded, both allocation-free and without a background task:

1. **Opportunistic prune.** On `record_failure`, if the map has reached `MAX_TRACKED_IPS` (4096),
   `retain` drops every entry whose window has already expired.
2. **In-place window rollover.** An entry whose window has lapsed is reset when next touched rather
   than accumulating alongside a new one.

`retry_after` is a pure read and never grows the map, so unauthenticated traffic that never fails
authentication cannot inflate it either.

Concurrency: every operation is synchronous and short; **no lock is held across an `.await`**
anywhere. Locking goes through the poison-recovering helper (see §10), so the M3 property extends to
this new state — a panic under the limiter lock cannot brick authentication.

Covered by `stale_entries_do_not_accumulate_without_bound`, which inserts 4160 distinct IPs and
asserts the map stays at or below the cap.

---

## 7. Files changed (9)

```
 apps/client/src/lib/useRealtimeFeed.ts |  10 +
 apps/mobile/src/types.ts               |   2 +-
 apps/mobile/src/useRealtimeFeed.ts     |   6 +-
 crates/ws-server/src/access.rs         | 726 ++++++++++++++++++++++++++++++---
 crates/ws-server/src/http.rs           |  15 +-
 crates/ws-server/src/main.rs           |  28 +-
 crates/ws-server/src/protocol.rs       |  18 +
 crates/ws-server/src/server.rs         | 145 ++++++-
 packages/shared-types/src/index.ts     |  17 +
 9 files changed, 871 insertions(+), 96 deletions(-)
```

The three TypeScript files and `protocol.rs` are Batch 1's changes, unmodified by this batch.

---

## 8. Tests added / modified

### IP derivation (`access.rs`)

| Test | Asserts |
|---|---|
| `an_untrusted_peer_cannot_spoof_its_identity` | A public peer's `X-Forwarded-For` is ignored entirely |
| `a_trusted_proxy_may_supply_the_client_ip` | Loopback v4, loopback v6, and the real Docker gateway `172.20.0.1` are all trusted |
| `the_rightmost_forwarded_entry_wins` | `1.2.3.4, 198.51.100.7` resolves to the proxy-appended value, not the forged one |
| `a_malformed_forwarded_value_falls_back_to_the_peer` | `not-an-ip`, empty, `" , "`, `999.999.999.999`, and a missing header all fall back |
| `forwarded_entries_carrying_a_port_are_understood` | `198.51.100.7:44321` and `[2001:db8::1]:44321` |
| `loopback_and_private_ranges_are_trusted_but_public_ones_are_not` | Full trust-set boundary, v4 and v6 |

### Limiter unit behaviour (`access.rs`)

`an_ip_is_throttled_only_after_exhausting_its_own_allowance`,
`one_hostile_ip_does_not_throttle_anyone_else`,
`retry_after_reports_a_positive_whole_number_of_seconds`,
`an_expired_window_becomes_usable_again`,
`stale_entries_do_not_accumulate_without_bound`,
`a_poisoned_limiter_still_answers`.

### HTTP end-to-end (`access.rs`)

| Test | Asserts |
|---|---|
| `valid_credentials_still_work_and_workload_limits_still_apply` | 401 / 200 / slot-429 / budget-429 all preserved |
| `health_cannot_be_guessed_indefinitely` | Guesses return 401, then 429 **with a positive `Retry-After`** |
| `one_attacker_cannot_lock_out_another_client` | Attacker throttled; unrelated IP still gets 200 — the Batch 1 weakness, directly |
| `successful_health_checks_do_not_consume_workload_capacity` | 25 successful `/health` calls leave the global budget at **0** |
| `a_successful_authentication_is_never_counted_as_a_failure` | 10 successful calls leave the IP unthrottled |

### WebSocket (`server.rs`)

| Test | Asserts |
|---|---|
| `browser_hello_requires_authentication_before_welcome_or_event_delivery` | **Preserved**, adapted to the new signature |
| `repeated_failed_hellos_exhaust_that_source_ip_and_then_are_refused` | Rejections become `"Too many authentication attempts"`; even a *correct* token is refused while throttled, so reconnecting buys no guesses |
| `one_throttled_source_does_not_affect_another_client` | Second IP still receives `welcome` |
| `a_rejection_reveals_nothing_about_the_expected_credential` | Rejection payload contains no substring of the expected token |

Window lifecycle is tested via `AuthLimiter::with_policy(Duration, u32)` with millisecond windows —
**no real 60-second sleeps anywhere**.

---

## 9. Validation results

| Command | Result |
|---|---|
| `cargo test -p ws-server` | **45 passed, 0 failed** (was 28) |
| `cargo test --workspace --no-fail-fast` | **341 passed, 0 failed**, **0 SIGKILLs**, 0 assertion failures |
| `bun --cwd=apps/client run test` | **60 passed, 0 failed** |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **exit 0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **exit 0** |

**Anomaly note, resolved:** the previous batch's workspace runs showed test harnesses terminated
with SIGKILL when two `cargo` invocations ran concurrently. Run sequentially this time, the
workspace completed cleanly with **zero** SIGKILLs and a higher total (341 vs 324, the difference
being this batch's 17 new tests). The earlier kills were contention, not a code defect.

**Formatting:** no reformatting was performed. Targeted `rustfmt --check` on the four changed Rust
files reports pre-existing drift — `main.rs` alone shows 83 hunks against the 4 lines this batch
added, which demonstrates the drift is baseline. New code was written to match surrounding style.

---

## 10. Batch 1 preservation

| Item | Status |
|---|---|
| **M3** poisoned-mutex recovery | Preserved and **extended**. `lock_budget` was generalised to `lock_recover<T>`, now covering the new limiter map as well. `a_poisoned_budget_still_admits_requests` retained; `a_poisoned_limiter_still_answers` added. |
| **M4** `BROADCAST_CAPACITY = 16_384` | Unchanged |
| **M4** both channels use it | Unchanged |
| **M4** `stream_lagged` | Unchanged |
| **M4** retained snapshot resync | Unchanged |
| **M4** auto-trader disconnect-on-lag | Unchanged |
| **M5** fail-closed token | Unchanged |
| **M5** explicit `ApiToken` | Unchanged |
| **M5** constant-time comparison | Unchanged |
| **M5** startup guards | Unchanged (`main.rs:87` untouched) |

---

## 11. Batch 1's global `/health` charging: **superseded**

**Superseded, deliberately.**

Batch 1 charged `/health` — and every authentication attempt — to the process-wide workload budget.
That closed the unlimited-oracle hole but created the availability weakness this batch exists to
remove: an unauthenticated flood could exhaust a shared 120/minute allowance and force 429s on
legitimate clients.

With a dedicated per-IP failure limiter in place, that trade is no longer necessary. `/health` now:

- is **not** charged to the global workload budget on success (asserted: the budget stays at 0
  across 25 successful checks);
- **is** protected against guessing, because failures are charged to the attacker's own IP.

Both halves of the end-state invariant now hold simultaneously: `/health` is never an unlimited
credential oracle, **and** one hostile IP cannot exhaust authentication capacity for anyone else.

The Batch 1 tests that asserted the superseded architecture — `health_participates_in_rate_limiting`
and `failed_health_credential_attempts_consume_the_budget`, both of which asserted the *global*
budget was charged — were replaced rather than retained, as instructed. Their security intent is now
covered by `health_cannot_be_guessed_indefinitely` and `one_attacker_cannot_lock_out_another_client`.

---

## 12. Compatibility and security tradeoffs

**Trust widened beyond loopback, out of necessity.** Per finding 1.3, private ranges must be trusted
or per-IP limiting is inert in production. The safety of this rests entirely on the loopback-only
port publishing in `ops/vps/docker-compose.yml`. **If those ports were ever republished on
`0.0.0.0`, a LAN attacker could spoof `X-Forwarded-For` and evade the limiter.** This is documented
in `is_trusted_proxy`'s doc comment; it is the single most important operational assumption in this
batch.

**Clients behind a shared NAT share an allowance.** Ten failures per minute is generous for one
person with one key, but a large shared egress IP with several misconfigured clients could
collectively throttle. Acceptable for a single-operator tool; worth revisiting if the user base
grows.

**Signature changes, all crate-internal:** `router()`, `http::run()` and `server::run()` each take
an `Arc<AuthLimiter>`; `await_hello` takes the limiter and client IP; `Access::from_env` takes the
limiter. No wire-format, API or client-visible change.

**WS rejection reasons are new strings, not a protocol change.** `hello_rejected` already carried a
free-text `reason`; a throttled client now receives `"Too many authentication attempts"` instead of
`"Unauthorized"`. Both existing clients treat any `hello_rejected` identically (set status closed),
so older builds cannot crash on it. No expected-token information, match length, or failure detail
is exposed — asserted by test.

**`Retry-After` is included** on authentication 429s only, via axum's tuple response. Workload 429s
were left alone; adding it there would have meant threading window state through paths this batch
should not touch.

**The 64-slot WS connection semaphore was left in place** and is explicitly *not* treated as
authentication throttling, per instruction.

---

## 13. `git diff --stat` and `git status --short`

Both are reproduced in §7 and here:

```
$ git status --short
 M apps/client/src/lib/useRealtimeFeed.ts
 M apps/mobile/src/types.ts
 M apps/mobile/src/useRealtimeFeed.ts
 M crates/ws-server/src/access.rs
 M crates/ws-server/src/http.rs
 M crates/ws-server/src/main.rs
 M crates/ws-server/src/protocol.rs
 M crates/ws-server/src/server.rs
 M packages/shared-types/src/index.ts
?? .claude/
?? AUDIT-2026-09-09.md
```

`.claude/` and `AUDIT-2026-09-09.md` were not touched or staged. **Nothing was staged; no commit was
created; nothing was pushed.**

---

## 14. Important changed code

### Client identity — `crates/ws-server/src/access.rs`

```rust
/// Loopback covers a native reverse proxy on the same host. Private ranges
/// are included because of how this actually deploys: the container publishes
/// its ports as `127.0.0.1:8788:8788`, so the only route in is through the
/// host's own proxy -- but Docker rewrites the source address on the way, and
/// the container sees the bridge gateway (observed in production as
/// `172.20.0.x`), never `127.0.0.1`. Trusting loopback alone would therefore
/// collapse *every* client into a single bucket in production.
fn is_trusted_proxy(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00   // fc00::/7
                || (v6.segments()[0] & 0xffc0) == 0xfe80   // fe80::/10
        }
    }
}

/// Behind a trusted proxy the **last** entry of `X-Forwarded-For` is used,
/// not the first. Caddy *appends* the address it saw to whatever the client
/// sent, so a request forging `X-Forwarded-For: 1.2.3.4` arrives as
/// `1.2.3.4, <real client>`. The leftmost entry is attacker-controlled; the
/// rightmost is the one our own proxy vouched for.
pub fn effective_client_ip(peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    if !is_trusted_proxy(peer) {
        return peer;
    }
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.rsplit(',').map(str::trim)
                .find(|entry| !entry.is_empty())
                .and_then(parse_forwarded_addr)
        })
        .unwrap_or(peer)
}
```

### The limiter

```rust
/// Successful authentications are never counted, and never reset an existing
/// failure count. Not resetting is the safer of the two options: a reset on
/// success would let anyone holding one valid credential clear the record
/// between guessing bursts, turning the limiter into a formality.
pub struct AuthLimiter {
    window: Duration,
    max_failures: u32,
    state: Mutex<HashMap<IpAddr, Attempts>>,
}

impl AuthLimiter {
    pub fn retry_after(&self, ip: IpAddr) -> Option<u64> {
        let state = lock_recover(&self.state);
        let attempts = state.get(&ip)?;
        let elapsed = attempts.window_started.elapsed();
        if elapsed >= self.window || attempts.failures < self.max_failures {
            return None;
        }
        Some((self.window - elapsed).as_secs() + 1)
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut state = lock_recover(&self.state);
        if state.len() >= MAX_TRACKED_IPS {
            let window = self.window;
            state.retain(|_, a| a.window_started.elapsed() < window);
        }
        let attempts = state.entry(ip).or_insert_with(Attempts::new);
        if attempts.window_started.elapsed() >= self.window {
            *attempts = Attempts::new();
        }
        attempts.failures = attempts.failures.saturating_add(1);
    }
}
```

### HTTP flow

```rust
pub async fn protect(
    State(access): State<Access>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = effective_client_ip(peer.ip(), req.headers());

    if let Some(retry_after) = access.auth.retry_after(ip) {
        return throttled(retry_after);                    // 429 + Retry-After
    }
    if !authorized(/* header */, access.token.as_ref()) {
        access.auth.record_failure(ip);                   // charged to this IP only
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // /health no longer consumes shared workload capacity (supersedes Batch 1)
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    /* global budget, expensive budget, permit, timeout — unchanged */
}
```

### WebSocket — `crates/ws-server/src/server.rs`

```rust
// Inside the upgrade callback (headers are only visible here):
let ip = crate::access::effective_client_ip(peer.ip(), request.headers());
*ip_slot.lock().unwrap_or_else(|p| p.into_inner()) = ip;
// Reject a throttled source at the upgrade itself -- it is the
// reconnect-and-retry loop that made this path abusable.
if upgrade_auth.retry_after(ip).is_some() {
    return Err(/* HTTP 429 */);
}
...
} else {
    upgrade_auth.record_failure(ip);
    Err(/* HTTP 401 */)
}

// Inside await_hello:
if auth.retry_after(client_ip).is_some() {
    send_json(ws, &HandshakeMessage::HelloRejected {
        reason: "Too many authentication attempts".into() }).await?;
    anyhow::bail!("throttled WebSocket hello");
}
if !header_authenticated && !crate::access::authorized(credential.as_deref(), expected_token) {
    auth.record_failure(client_ip);
    send_json(ws, &HandshakeMessage::HelloRejected { reason: "Unauthorized".into() }).await?;
    anyhow::bail!("unauthorized WebSocket hello");
}
```

### `crates/ws-server/src/http.rs`

```rust
// ConnectInfo is what makes the real TCP peer address reachable from the
// `protect` middleware; without it there is no spoof-resistant identity
// to key the per-IP authentication limiter by.
axum::serve(
    listener,
    router(cfg, today_movers, catalysts, qualify_url, push_tokens, auth)
        .into_make_service_with_connect_info::<std::net::SocketAddr>(),
).await?;
```

---

## 15. Scope confirmation

**Implemented:** M1 completion only — per-IP authentication-abuse limiting for HTTP and WebSocket,
spoof-resistant client-IP derivation, bounded limiter state, and the `/health` architecture
correction that supersedes Batch 1's temporary charging.

**Not implemented, as instructed:** M2 (Docker `USER`), M6 (dependency scanning), M7
(monitoring/alerting), H1, H2, H3, and every Low finding.

**Not done:** no commit, no staging, no push, no deploy, no SSH to production, no credential
changes.
