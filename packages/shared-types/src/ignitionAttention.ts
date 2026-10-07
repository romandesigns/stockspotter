// The one predicate that decides whether an Ignition-family event earns the
// user's attention.
//
// This is NOT a new score, threshold or detector. It is the EXISTING green
// highlight, extracted verbatim from the rule that already drove it so that
// one definition can serve three call sites that must never disagree:
// green styling, panel visibility, and notification eligibility.
//
// # Where it came from
//
// apps/client/src/components/panels/IgnitionPanel.tsx's `rowClass` returned
// the CSS class `feed-row-hit` for exactly two cases, and `.feed-row-hit` is
// the green treatment (`border-left-color: var(--good)` / `background:
// var(--good-bg)`, with `--good: #0ca30c`). Everything else got plain
// `feed-row`, or `feed-row-muted` (opacity 0.5) for a rejection. So "green"
// already meant, precisely:
//
//     ignition_event      with kind === "follow_through_confirmed"
//  OR consolidation_event with kind === "entry_triggered"
//
// and nothing else. No thresholds, no numeric fields, no momentum score --
// the predicate reads one discriminant. That is reproduced here unchanged.
//
// # Why it is safe as a visibility filter: the predicate is immutable
//
// It depends only on `kind`, which is fixed when the server emits the event.
// A row therefore cannot transition non-green -> green: when a candidate
// later confirms, the server emits a SEPARATE event with
// kind = follow_through_confirmed, which arrives as its own item and is
// green on arrival. So hiding a non-green row can never hide something that
// becomes meaningful later, and no shadow retention of hidden items is
// needed to preserve a future transition. (Measured for context on
// 2026-09-21, 14:46-15:47Z: 383,477 candidate + 358,829 rejected + 19,543
// confirmed.)
//
// # What this does NOT do
//
// It does not remove any event from `useRealtimeFeed`'s `events` list, so
// every other panel and derivation still sees the whole stream, and it has
// no bearing at all on the server: research capture, OI, ranking and
// Auto-Trader consume the detector output directly and are untouched.

/** The minimum shape needed to decide attention. Structural on purpose, so
 *  both the web panel's wrapped feed item and mobile's raw event list can
 *  use one implementation. */
export interface IgnitionAttentionCandidate {
  type: string;
  /** Optional so the predicate can be applied to a mixed event union
   *  directly. Events outside the Ignition family have no `kind`, and both
   *  functions return false for them, so accepting the wider shape is the
   *  contract rather than a loosening of it. */
  kind?: string;
}

/**
 * True for exactly the events the app already highlights green.
 *
 * Returns false for anything outside the Ignition family, so it is safe to
 * apply to a mixed event list without silently reclassifying halt warnings,
 * catalysts or funnel signals.
 */
export function qualifiesForIgnitionAttention(event: IgnitionAttentionCandidate): boolean {
  if (event.type === "ignition_event") return event.kind === "follow_through_confirmed";
  if (event.type === "consolidation_event") return event.kind === "entry_triggered";
  return false;
}

/** True for an event that belongs to the Ignition family at all, green or
 *  not. Lets a caller filter the family without also deciding attention --
 *  needed by mobile, whose alert list mixes families. */
export function isIgnitionFamilyEvent(event: IgnitionAttentionCandidate): boolean {
  return event.type === "ignition_event" || event.type === "consolidation_event";
}

// ---------------------------------------------------------------------------
// The user's price ceiling.
//
// Roman, 2026-09-21: "I am not interested in stocks with a price higher than
// 25 dollars." That is a statement about HIS TRADING UNIVERSE, not about
// signal quality, so it is deliberately kept separate from the green
// predicate above rather than folded into it. Green still means exactly what
// the detector means by green; user attention means green AND inside the
// universe he actually trades.
//
// Keeping them apart matters in practice: the diagnostic "All" view still
// renders a $218 confirmation with its green treatment, because it IS a
// meaningful detector event -- it just is not one he will act on.
//
// # What the ceiling does NOT reach: server push
//
// This is a CLIENT rule. It gates the Ignition panel, mobile's Alerts list
// and the in-app notifications both clients raise from
// `ignitionConfirmedEvents`. It does NOT gate the server-side Expo push that
// reaches a locked or backgrounded phone: ws-server's push task
// (crates/ws-server/src/main.rs, the `push_rx` loop) sends on every
// FollowThroughConfirmed at ANY price, subject only to its own per-symbol
// cooldown. Until that task applies the same rule, a lock-screen push can
// still arrive for a $218 confirmation. Changing that is a separate,
// server-side decision and is deliberately not implied here.

/** Inclusive ceiling. "Higher than 25" is excluded, so 25.00 itself is in. */
export const USER_ATTENTION_PRICE_CEILING = 25.0;

/**
 * Is this the price of an event the user could act on?
 *
 * Conservative by construction: a missing, non-numeric, non-finite or
 * non-positive price returns FALSE. Unknown price must never be treated as
 * eligible -- defaulting it to zero, or to "probably cheap", would put
 * un-vetted rows on the attention surface and fire notifications for them.
 * Measured over the 2026-09-21 regular session, 142,472 confirmations
 * carried a usable price and zero were missing or invalid, so this branch is
 * defensive rather than routine.
 */
export function withinUserAttentionPrice(price: unknown): boolean {
  return typeof price === "number" && Number.isFinite(price) && price > 0
    && price <= USER_ATTENTION_PRICE_CEILING;
}

/** The candidate shape plus the causal event price.
 *
 * `price` is whatever the wire event carries, and the two families source it
 * DIFFERENTLY in crates/market-data/src/live.rs:
 *
 * - IgnitionEvent: `price: trade.price`, sent alongside `timestamp:
 *   trade.timestamp`, from the single trade that resolved the follow-through
 *   window. A trade price at the confirmation instant.
 * - ConsolidationEvent: `price: bar.close` of the COMPLETED 1-minute bar the
 *   breakout / micropullback monitor evaluated, stamped `bar.timestamp + 1
 *   min` (the moment that bar closed). A bar close, not a trade print, so it
 *   can differ from the last trade seen at the same instant.
 *
 * Both are causal -- fixed when the server emits the event, never a later
 * quote or a later bar -- which is all the ceiling needs, and neither needs a
 * client-side lookup. */
export interface UserAttentionCandidate extends IgnitionAttentionCandidate {
  price?: unknown;
}

/**
 * The single rule for what reaches the user: a meaningful detector event,
 * inside the user's price universe.
 *
 *     qualifiesForUserAttention = qualifiesForIgnitionAttention
 *                              && withinUserAttentionPrice(event.price)
 *
 * Used by panel visibility, the visible count, and notification eligibility,
 * on every client. Nothing compares against 25 anywhere else.
 *
 * Eligibility is decided from the event's own price and is therefore
 * permanent: a confirmation at $24.80 stays eligible if the stock later
 * trades at $25.40, and one at $25.30 is never retroactively promoted if it
 * later falls to $24.50. No future price can change a past decision.
 */
export function qualifiesForUserAttention(event: UserAttentionCandidate): boolean {
  return qualifiesForIgnitionAttention(event) && withinUserAttentionPrice(event.price);
}
