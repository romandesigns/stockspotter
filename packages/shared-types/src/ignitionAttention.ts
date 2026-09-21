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
