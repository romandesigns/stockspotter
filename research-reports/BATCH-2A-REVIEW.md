# Batch 2A — Pre-commit review of two Batch 2 security assumptions

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909`
**Base audited commit:** `bafae88`
**Status:** review complete, corrections applied — **not committed, not staged, not pushed**

**Headline: both challenged assumptions were wrong. One was a documentation error; the other was a
real defect that violated the invariant I claimed. Both are now corrected and tested.**

---

## 1. Was the Caddy / XFF assumption correct?

**Incorrect.**

Batch 2's report claimed that with bare `reverse_proxy`, a client-supplied `X-Forwarded-For: 1.2.3.4`
would arrive upstream as `1.2.3.4, <real-client-ip>`, and that rightmost selection was needed to skip
the forged entry. That premise does not hold.

### Evidence available locally

| Source | Finding |
|---|---|
| Caddy version pinned in repo | **None.** Caddy is an apt-installed native systemd service on the VPS (`ops/vps/README.md`); no image tag, package version or lockfile anywhere in the repository. |
| `ops/vps/Caddyfile.snippet` | Three bare `reverse_proxy` directives. **No `trusted_proxies`**, no `header_up`, no XFF manipulation of any kind. |
| `trusted_proxies` anywhere in repo | Not present. |
| Caddy documentation (`caddyserver.com/docs/caddyfile/directives/reverse_proxy`) | Caddy "will ignore their values from incoming requests, to prevent spoofing" for `X-Forwarded-*`. `trusted_proxies` is what enables using client-supplied values, and it defaults to trusting no upstream proxies. |

I did not SSH to production, so the exact deployed Caddy version is **unverified** — that is a real
limit on this conclusion, not an oversight.

### Can an arbitrary public client cause multiple XFF entries to reach Stockspotter?

**No.** With `trusted_proxies` unset — which is the case here — Caddy discards whatever the client
sent and writes the address it observed. A forged header is dropped at the proxy, never appended to.

### What XFF value should Stockspotter expect?

**Exactly one entry: the client address Caddy observed.** Not a chain, not a forged prefix.

This applies identically to the WebSocket route. Caddy's docs do not carve out upgrade requests, and
`/ws` is served by the same `reverse_proxy` directive as `/api/*`, so header handling is the same on
both paths.

### Was rightmost parsing retained?

**Yes — retained, with corrected reasoning, not with the incorrect one.**

With a single entry present, rightmost and leftmost are the same value, so the code was never
producing a wrong answer. Rightmost is kept because it is the choice that stays correct if the
assumption stops holding:

- an older Caddy that appends rather than ignores → rightmost is the proxy-vouched value;
- a `trusted_proxies` line added later → appending resumes, rightmost stays correct;
- leftmost would, in both of those cases, silently begin reading attacker-controlled data.

Since the repository pins no Caddy version and the deployed one cannot be checked without SSH, the
option that is safe under **both** behaviours is the defensible choice. Spoof resistance is not
weakened; the parsing is unchanged and the misleading comment is replaced with the accurate account,
including an explicit warning that a real proxy *chain* (a CDN in front of Caddy) would require
revisiting this, because rightmost would then name the CDN.

---

## 2. Final client-IP derivation logic

```rust
pub fn effective_client_ip(peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    if !is_trusted_proxy(peer) {
        return peer;                      // forwarded headers ignored entirely
    }
    headers.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').map(str::trim)
                       .find(|e| !e.is_empty())
                       .and_then(parse_forwarded_addr))
        .unwrap_or(peer)                  // absent or unparseable → the peer
}
```

Accepted forwarded forms: bare `IpAddr`, `addr:port`, bracketed IPv6. Only `X-Forwarded-For` is
consulted; `X-Real-IP` and `Forwarded` remain untrusted because Caddy does not set them here.

---

## 3. Final trusted-proxy boundary, and why

### What changed

An explicit, deployment-supplied trust list was added:

```
STOCKSPOTTER_TRUSTED_PROXIES=172.20.0.1
STOCKSPOTTER_TRUSTED_PROXIES=10.0.0.0/8,::1        # CIDRs and lists also accepted
```

When set it **replaces the heuristic entirely** — exactly those entries are trusted and nothing
else. Parsing is a ~35-line `Cidr` type supporting bare addresses and v4/v6 prefixes, with no new
dependency and no proxy framework.

### Why the private-range fallback is retained

Working through the stated preference order:

1. **An exact/narrow proxy identity from configuration — not available.**
   `ops/vps/docker-compose.yml` declares **no `networks:` block at all**. The stack runs on
   Compose's default bridge, whose subnet Docker assigns dynamically, so the gateway address is not
   derivable from anything checked in. Hard-coding an observed `172.20.0.1` would be exactly the
   fragile operational coupling the instruction warns against — it would break silently on any
   network recreation, and the failure mode is *trusting nothing*, which collapses all clients into
   one bucket again.

2. **An explicit CIDR from deployment configuration — now available, but opt-in.**
   The mechanism above exists. I did not modify `ops/vps/docker-compose.yml` to set it, because that
   is production deployment configuration and this batch is explicitly review-only.

3. **Private-range fallback — retained as the default**, for the reason in (1).

The narrower boundary is therefore one line of deployment config away, and the code documents
exactly what to set. Two ways to close it properly, whenever you want:

- add `STOCKSPOTTER_TRUSTED_PROXIES=<gateway>` to the compose `ws` service environment; or
- pin an explicit subnet in a `networks:` block, making the gateway stable and knowable, then set
  the variable to that gateway.

### If the container port were ever published beyond loopback

This is the assumption the fallback rests on, and it is now stated in the code:

> If these ports were ever published on `0.0.0.0`, a LAN peer could forge `X-Forwarded-For` and
> evade per-IP limiting.

What would have to change: `STOCKSPOTTER_TRUSTED_PROXIES` becomes **mandatory**, set to the real
proxy address only, and the private-range fallback must no longer be relied upon. The fallback is
safe *only* because `127.0.0.1:8788:8788` means the sole route in is the host's own proxy path.

---

## 4. Was the limiter truly bounded? — **No. Real defect, now fixed.**

Your reading was correct and my Batch 2 report's claim was wrong.

The old `record_failure` pruned only *expired* entries when the map reached capacity, then inserted
unconditionally via `entry().or_insert_with()`. With 4096 entries all still active, `retain` removed
nothing and the insert took the map to 4097 — and onward without limit.

**It passed my Batch 2 test only because that test used a 1 ms window, so every entry had expired
and pruning always succeeded.** The test exercised the easy path and never the worst case. That is a
straightforward testing failure on my part, and the reason your review caught it.

### Behaviour at capacity now

```
record_failure(ip):
  1. already tracked?  → charge it, return          (always, saturated or not)
  2. at capacity?      → prune lapsed windows
  3. room now?         → insert with failures = 1, return
  4. still full        → DO NOT insert; mark saturated_until = now + window
```

An entry is inserted **only** when `entries.len() < MAX_TRACKED_IPS`, so
`entries.len() <= MAX_TRACKED_IPS` holds at every exit — the bound is enforced by the admission
check itself, with pruning as an optimisation that makes room rather than as the bound.

### Why not evict to make room

Evicting an active entry would let an attacker erase accumulated throttling by rotating source
addresses — precisely the abuse the limiter exists to stop. So active entries are never evicted.

### Why an unseen IP is not simply admitted

Refusing to allocate must not become a free pass. While saturated, `retry_after` treats **unknown**
IPs as throttled:

```rust
// Unknown IP. Normally fine -- but while the limiter is saturated we are
// unable to track anyone new, so admitting unknown IPs would hand an attacker
// unlimited guesses simply by rotating source addresses.
let until = state.saturated_until?;
let now = Instant::now();
(now < until).then(|| (until - now).as_secs() + 1)
```

### The availability/security tradeoff, stated plainly

**While saturated, previously unseen clients are refused authentication until the window lapses.**
That is a genuine availability cost, and it is chosen deliberately:

- Reaching 4096 *simultaneously active, currently-failing* source addresses is not ordinary traffic
  — it is a distributed credential-guessing attack in progress.
- The alternative (admitting untracked IPs) converts saturation into unlimited guessing from any new
  address, which defeats the control entirely at exactly the moment it matters most.
- The cost is **time-bounded**: saturation expires with the window, never becomes a permanent ban,
  and requires no operator intervention to clear.
- Already-authenticated sessions are unaffected — this gates authentication attempts only.
- Legitimate clients that are *already tracked* keep their normal allowance throughout.

Failing closed under active attack is the right default for a single-operator tool holding brokerage
credentials.

---

## 5. Proof that state is bounded

**Structural:** insertion is guarded by `if state.entries.len() < MAX_TRACKED_IPS`. It is the only
insert site. `retry_after` is a pure read and never allocates, so unauthenticated traffic that never
fails cannot grow the map either.

**Empirical** — `the_map_is_hard_bounded_even_when_no_entry_can_be_pruned`:

- fills to exactly `MAX_TRACKED_IPS` using a **600-second** window, so nothing can expire mid-test;
- then drives **10,000** further distinct addresses through `record_failure`;
- asserts `tracked() <= MAX_TRACKED_IPS` **after every single iteration**, not merely at the end;
- asserts the final size is exactly `MAX_TRACKED_IPS`.

That test fails against the previous implementation and passes against the current one.

---

## 6. Tests added / changed

| Test | Asserts |
|---|---|
| `the_shape_caddy_actually_forwards_resolves_to_the_real_client` | The realistic single-entry XFF that Caddy actually sends, via the Docker gateway peer |
| `an_explicit_trusted_proxy_list_parses_addresses_and_cidrs` | Bare IPs, v4 CIDR, v6 CIDR, cross-family non-match, malformed input rejected, `/99` rejected |
| `the_map_is_hard_bounded_even_when_no_entry_can_be_pruned` | **The worst case.** 4096 active + 10,000 more; bound checked every iteration |
| `an_already_tracked_ip_stays_throttled_while_saturated` | Saturation never releases an existing throttle |
| `saturation_does_not_hand_a_new_ip_unlimited_guesses` | An untrackable IP is refused, not admitted |
| `saturation_lapses_and_normal_admission_resumes` | Saturation is time-bounded; a new client is admitted again afterwards |
| `expired_entries_are_pruned_so_the_map_does_not_creep` | Renamed from the old `stale_entries_...`; now clearly the *easy* path, not the bound |

Retained and still passing from Batch 2: spoofed-XFF-from-untrusted-peer, trusted-proxy path,
rightmost selection, malformed fallback, port-carrying entries, trust-set boundary, per-IP isolation,
expiry, poisoned-limiter recovery, and all four WebSocket throttling tests.

No real 60-second sleeps anywhere; window lifecycle is driven by `with_policy(Duration, u32)` with
millisecond windows.

---

## 7. Validation results

| Command | Result |
|---|---|
| `cargo test -p ws-server` | **51 passed, 0 failed** (was 45) |
| `cargo test --workspace --no-fail-fast` | **347 passed, 0 failed**, **0 SIGKILLs** |
| `bun --cwd=apps/client run test` | **60 passed, 0 failed** |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **exit 0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **exit 0** |

No repository-wide rustfmt churn was performed.

---

## 8. Files changed by Batch 2A

Only one:

```
crates/ws-server/src/access.rs
```

Batch 2A changed no other file. The other eight modified files are Batch 1 and Batch 2 work,
untouched here.

---

## 9. `git diff --stat`

```
 apps/client/src/lib/useRealtimeFeed.ts |   10 +
 apps/mobile/src/types.ts               |    2 +-
 apps/mobile/src/useRealtimeFeed.ts     |    6 +-
 crates/ws-server/src/access.rs         |  963 ++++++++++++++++++++++++++++---
 crates/ws-server/src/http.rs           |   15 +-
 crates/ws-server/src/main.rs           |   28 +-
 crates/ws-server/src/protocol.rs       |   18 +
 crates/ws-server/src/server.rs         |  145 ++++-
 packages/shared-types/src/index.ts     |   17 +
 9 files changed, 1108 insertions(+), 96 deletions(-)
```

## 10. `git status --short`

```
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

`.claude/` and `AUDIT-2026-09-09.md` untouched and unstaged. Nothing staged, committed or pushed.

---

## 11. Recommendation: are Batch 1 + 2 + 2A ready to commit?

**Yes, with two caveats worth recording in the commit message rather than blocking on.**

The security properties now hold and are tested:

- `/health` is not an unlimited credential oracle, and successful checks cost no shared capacity;
- one hostile IP cannot exhaust anyone else's authentication allowance;
- WebSocket reconnect-and-retry is throttled on the same shared per-IP budget;
- limiter state is hard-bounded, with the worst case tested rather than assumed;
- authentication fails closed; constant-time comparison and startup guards intact;
- poisoned mutexes cannot brick the request path;
- broadcast lag is visible to clients instead of silent.

**Caveat 1 — one assumption is unverified.** The deployed Caddy version is unknown, because nothing
in the repository pins it and I did not SSH. The implementation is deliberately correct under both
the current documented behaviour and the older appending behaviour, so this does not block a commit
— but it should be stated rather than presented as verified.

**Caveat 2 — the trust boundary is wider than ideal by default.** Closing it properly needs one line
of deployment configuration (`STOCKSPOTTER_TRUSTED_PROXIES`, or a pinned Docker subnet). Safe today
purely because the ports are loopback-published; that dependency is now documented in the code.

**Two process observations, offered rather than acted on:**

The bound defect existed for a full batch and my own test hid it by exercising only the easy path.
Worth assuming the same failure mode elsewhere in this remediation — a test that passes because it
never reaches the hard case.

And per finding H1 of the audit: this branch is a `release/*` branch, which the deploy script
accepts without CI ever running. Whatever is committed here will reach production without
`validate.yml` seeing it unless that gap is closed first.

I would commit this. I have not staged anything, per instruction.
