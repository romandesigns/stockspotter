// Row styling for the Ignition feed, kept out of the component so it can be
// tested without pulling the component tree (and its `@/` path alias) into
// the test runner.
//
// Green is decided by the one canonical predicate, not by a second copy of
// the rule -- see qualifiesForIgnitionAttention in shared-types for what
// green has always meant and why it is safe as a visibility filter.
import { qualifiesForIgnitionAttention } from "@stockspotter/shared-types";
import type { IgnitionFeedItem } from "./derive";

export function rowClass(item: IgnitionFeedItem): string {
  if (qualifiesForIgnitionAttention(item.event)) return "feed-row feed-row-hit";
  // Retained for the diagnostic "All" mode: a rejection is still worth
  // showing as explicitly de-emphasised rather than as a neutral row.
  if (item.source === "ignition" && item.event.kind === "follow_through_rejected") return "feed-row feed-row-muted";
  return "feed-row";
}
