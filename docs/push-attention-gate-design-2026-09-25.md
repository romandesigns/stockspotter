# Server-side $25 push gate: design only (2026-09-25)

**Status: DESIGN. Nothing here is implemented.** This document specifies
the change. It does not make it. The push task ships to the live VPS with
the rest of ws-server, and a behaviour change on a phone's lock screen is a
product decision for Roman. It is not a side effect of the P3 measurement
work.

## 1. Current behaviour

`crates/ws-server/src/main.rs`, the `push_rx` task (around line 240–270 at
`280f00c`), is a fourth independent broadcast subscriber:

```rust
Ok(ScanEvent::IgnitionEvent { symbol, price, kind: IgnitionEventKind::FollowThroughConfirmed, .. }) => {
    let now = chrono::Utc::now();
    let cooling_down = last_pushed.get(&symbol).is_some_and(|last| now - *last < push::IGNITION_PUSH_COOLDOWN);
    if cooling_down { continue; }
    last_pushed.insert(symbol.clone(), now);
    let tokens = push_tokens_for_task.snapshot().await;
    push::send_ignition_push(&http_client, &tokens, &symbol, price).await;
}
```

- It pushes **every** `FollowThroughConfirmed`, **at any price**. A $218
  confirmation reaches a locked phone.
- The only suppression is `push::IGNITION_PUSH_COOLDOWN` (15 minutes),
  applied per symbol and keyed on wall-clock `Utc::now()`.
- `ConsolidationEvent { kind: EntryTriggered }` is green on the clients
  but has **never** been pushed by the server. This design does not change
  that.
- The clients already apply the $25 ceiling to everything they control:
  the Ignition panel, mobile Alerts, the visible count, and in-app
  notifications. `packages/shared-types/src/ignitionAttention.ts` (lines
  89–99) documents that the server push is the one surface the ceiling
  does not reach. `apps/mobile/App.tsx` (lines 86–89) says the same at
  its call site.

## 2. Required semantics (future)

A push is sent for an event **iff**:

```text
event is ScanEvent::IgnitionEvent
  AND kind == FollowThroughConfirmed
  AND price is finite AND price > 0 AND price <= 25.00        (inclusive)
  AND the symbol is not inside its existing 15-minute cooldown
```

- **The price is the event's own causal price.** For `IgnitionEvent` that
  is `price: trade.price`, the trade that resolved the follow-through
  window (`crates/market-data/src/live.rs`). It is never a later quote or
  bar. Eligibility is therefore fixed at emission, as on the clients:
  $24.80 stays eligible if the stock later trades at $25.40, and $25.30 is
  never promoted later.
- **It is the same predicate as `withinUserAttentionPrice`**
  (`ignitionAttention.ts:115-118`): `typeof price === "number" &&
  Number.isFinite(price) && price > 0 && price <= USER_ATTENTION_PRICE_CEILING`
  with the ceiling 25.0 **inclusive**. A price that is invalid (NaN, ±inf,
  0, negative) is **excluded**. Unknown is never eligible.
- **Only ineligible events are skipped, and a skipped event must not touch
  cooldown state.** The price check has to come **before** the cooldown
  lookup and insert. Today's order (check the cooldown, then insert) would,
  if the price test were simply appended, let a $31 confirmation start a
  15-minute cooldown that silently swallows a $24 confirmation of the same
  symbol two minutes later. That would be a regression dressed up as a
  filter.
- **No new suppression policy.** There is no one-per-session rule, no
  re-arm or reset-on-new-high rule, and no per-day cap. The only
  suppression remains the existing 15-minute per-symbol cooldown, and it
  applies only among eligible events. Any further policy is a separate
  decision and is explicitly out of scope.
- **Internal events are untouched.** The gate lives inside the push task
  only. The broadcast channel, WS client fanout, research capture (OI,
  measurement, outcomes, discovery), live-signal collection, Auto-Trader
  and the detectors keep receiving every event at every price. This is the
  same boundary the client predicate states (`ignitionAttention.ts:36-41`).

## 3. Minimal change sketch

In `crates/ws-server/src/push.rs`, add the single server-side statement
of the rule, mirroring the TypeScript:

```rust
/// Inclusive: "higher than 25" is excluded, so 25.00 itself is in.
/// Must equal packages/shared-types USER_ATTENTION_PRICE_CEILING.
pub const USER_ATTENTION_PRICE_CEILING: f64 = 25.0;

/// Server twin of `withinUserAttentionPrice`. Unknown or invalid is never eligible.
pub fn within_user_attention_price(price: f64) -> bool {
    price.is_finite() && price > 0.0 && price <= USER_ATTENTION_PRICE_CEILING
}

/// Pure decision, so it is testable without a broadcast channel or a clock.
pub fn should_push(
    last_pushed: &HashMap<String, DateTime<Utc>>,
    symbol: &str,
    price: f64,
    now: DateTime<Utc>,
) -> bool {
    within_user_attention_price(price)
        && !last_pushed.get(symbol).is_some_and(|last| now - *last < IGNITION_PUSH_COOLDOWN)
}
```

In the `push_rx` loop in `main.rs`:

```rust
Ok(ScanEvent::IgnitionEvent { symbol, price, kind: IgnitionEventKind::FollowThroughConfirmed, .. }) => {
    let now = chrono::Utc::now();
    if !push::should_push(&last_pushed, &symbol, price, now) {
        continue;                      // above the ceiling, invalid, or cooling down
    }
    last_pushed.insert(symbol.clone(), now);   // only eligible pushes start a cooldown
    ...
}
```

The change touches two files and adds no dependency and no configuration.
Do not make the ceiling an environment variable: a runtime knob would let
the server and client rules diverge silently. Do not change the
`IgnitionEvent` shape.

## 4. Tests to write (with the implementation)

In `push.rs` `mod tests`, against `should_push` and
`within_user_attention_price`:

1. **Boundary:** 24.99 → push; **25.00 → push (inclusive)**; 25.01 → no push; 25.000001 → no push.
2. **Invalid prices:** `f64::NAN`, `f64::INFINITY`, `f64::NEG_INFINITY`, 0.0, -1.0 → no push.
3. **Sub-dollar:** 0.0001 and 0.5 → push. The ceiling has no lower bound other than > 0.
4. **Cooldown among eligible events is unchanged:** a push at t, then the same symbol at t+14m59s → no; at t+15m → yes. The boundary matches today's `<`.
5. **An ineligible event does not start a cooldown (the ordering bug):** $31 at t, then $24 for the same symbol at t+2m → **push**. Assert that `last_pushed` has no entry after the $31 event.
6. **An ineligible event does not extend a running cooldown:** $24 at t (pushed), $40 at t+10m (skipped), $24 at t+15m → push. The cooldown runs from t, not from t+10m.
7. **Per-symbol independence:** AAA inside its cooldown does not block BBB.
8. **Only `FollowThroughConfirmed`:** `CandidateOpened`, `FollowThroughRejected` and `ConsolidationEvent::EntryTriggered` at $5 → no push. This pins the current scope.
9. **Parity with the client (contract test):** read
   `packages/shared-types/src/ignitionAttention.ts` and assert it contains
   `USER_ATTENTION_PRICE_CEILING = 25.0` and the `price > 0 && price <=`
   form. Assert the Rust constant is equal. This follows the pattern
   `runbook_contract_tests.rs` uses to keep `session.sh` literals equal to
   code, so either side changing alone fails the build.
10. **Parity table:** one fixed table of prices, `[-1, 0, 0.0001, 1, 24.99, 25, 25.01, 218, NaN, inf]`, whose expected booleans are asserted in Rust. If the shared-types package gains a test runner, assert the same table there. Until it has one, test 9 is the enforcement.
11. **No fanout change:** an existing WS/research test that counts events on another subscriber still sees the $218 confirmation.

## 5. Dependency: client predicate parity

- This design **depends on** `withinUserAttentionPrice` /
  `qualifiesForUserAttention` staying the canonical client rule (topic
  branch `topic/ignition-attention-25-cap-20260921`, commit `057f93e`).
  The two sides must change together, or not at all.
- When the server gate ships, the notes at `ignitionAttention.ts:89-99`
  and `apps/mobile/App.tsx:86-89` become **false**. They must be updated in
  the same release, so the code does not keep asserting that the lock
  screen is ungated.
- **Deploy order.** The server change is independent of client versions
  because it only removes pushes, so no client release has to come first.
  But it reaches production only by pushing stockspotter `master`, which
  deploys to the VPS within about 2 minutes. Ship it outside market hours,
  and not on a designated session's day: it changes a production-visible
  behaviour, although no research artifact.
- Not a qualification input. The research capture and the P3 gates are
  unaffected either way.
