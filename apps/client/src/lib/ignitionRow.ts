// Row styling for the Ignition feed, kept out of the component so it can be
// tested without pulling the component tree (and its `@/` path alias) into
// the test runner.
//
// Green is decided by the one canonical predicate, not by a second copy of
// the rule -- see qualifiesForIgnitionAttention in shared-types for what
// green has always meant and why it is safe as a visibility filter.
import { qualifiesForIgnitionAttention, qualifiesForUserAttention, USER_ATTENTION_PRICE_CEILING } from "@stockspotter/shared-types";
import type { IgnitionFeedItem } from "./derive";

export function rowClass(item: IgnitionFeedItem): string {
  if (qualifiesForIgnitionAttention(item.event)) return "feed-row feed-row-hit";
  // Retained for the diagnostic "All" mode: a rejection is still worth
  // showing as explicitly de-emphasised rather than as a neutral row.
  if (item.source === "ignition" && item.event.kind === "follow_through_rejected") return "feed-row feed-row-muted";
  return "feed-row";
}

// ---------------------------------------------------------------------------
// What the panel shows, and what it is holding back.
//
// Pure so the panel's visible list, its count and its subtitle are testable
// without a DOM renderer. Visibility is decided by qualifiesForUserAttention
// and nothing else -- the same call mobile's buildAlerts and both clients'
// notification call sites make -- so this adds no rule of its own; it only
// counts what that rule left out.
//
// The hidden count is split into its two causes because they mean different
// things to the user: "not confirmed" is detector bookkeeping, "above $25" is
// a real confirmation outside the universe he trades. Lumping them into one
// number would make a burst of expensive confirmations look like noise.
// (Hidden-count subtitle idea salvaged from ee47759 on
// claude/ignition-green-only-ux, extended here for the price ceiling.)
export interface IgnitionPanelView {
  visible: IgnitionFeedItem[];
  /** Hidden because the detector did not confirm (non-green). */
  hiddenUnconfirmed: number;
  /** Green, but priced above the user's ceiling. */
  hiddenAboveCeiling: number;
  /** Green, but the event carries no usable price (missing, NaN, <= 0).
   *  Hidden by the conservative rule in withinUserAttentionPrice; counted
   *  apart so the subtitle never calls an unpriced event "above $25". */
  hiddenUnpriced: number;
}

export function ignitionPanelView(items: IgnitionFeedItem[], showAll: boolean): IgnitionPanelView {
  if (showAll) return { visible: items, hiddenUnconfirmed: 0, hiddenAboveCeiling: 0, hiddenUnpriced: 0 };
  const visible: IgnitionFeedItem[] = [];
  let hiddenUnconfirmed = 0;
  let hiddenAboveCeiling = 0;
  let hiddenUnpriced = 0;
  for (const item of items) {
    if (qualifiesForUserAttention(item.event)) visible.push(item);
    else if (!qualifiesForIgnitionAttention(item.event)) hiddenUnconfirmed++;
    // Green but not attention: the price is either above the ceiling or
    // unusable. withinUserAttentionPrice(p) is false for both; a usable
    // price is one it would accept with the ceiling lifted.
    else if (typeof item.event.price === "number" && Number.isFinite(item.event.price) && item.event.price > 0) hiddenAboveCeiling++;
    else hiddenUnpriced++;
  }
  return { visible, hiddenUnconfirmed, hiddenAboveCeiling, hiddenUnpriced };
}

export function ignitionPanelSubtitle(view: IgnitionPanelView, showAll: boolean): string {
  if (showAll) return "all detector events (diagnostic)";
  const base = `confirmed + breakout entries ≤ $${USER_ATTENTION_PRICE_CEILING.toFixed(0)}`;
  const parts: string[] = [];
  if (view.hiddenAboveCeiling > 0) parts.push(`${view.hiddenAboveCeiling} above $${USER_ATTENTION_PRICE_CEILING.toFixed(0)}`);
  if (view.hiddenUnpriced > 0) parts.push(`${view.hiddenUnpriced} unpriced`);
  if (view.hiddenUnconfirmed > 0) parts.push(`${view.hiddenUnconfirmed} unconfirmed`);
  return parts.length === 0 ? base : `${base} · hidden: ${parts.join(", ")}`;
}
