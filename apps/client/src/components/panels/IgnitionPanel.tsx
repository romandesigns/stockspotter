// Ignition / explosive-move alert panel (doc Panels #3) — a live feed of
// ignition candidate/follow-through events, plus the post-ignition
// consolidation-breakout entry strategy folded in as a tagged row (not a
// separate panel — see deriveIgnitionFeed's doc comment).

import { formatPrice, formatTime } from "../../lib/format";
import type { IgnitionFeedItem } from "../../lib/derive";
import { EmptyState, PanelShell } from "../PanelShell";

const IGNITION_LABEL: Record<string, string> = {
  candidate_opened: "candidate opened",
  follow_through_confirmed: "confirmed",
  follow_through_rejected: "rejected",
};

const CONSOLIDATION_LABEL: Record<string, string> = {
  surge_detected: "surge detected",
  consolidation_confirmed: "consolidation confirmed",
  entry_triggered: "breakout entry",
};

function rowClass(item: IgnitionFeedItem): string {
  if (item.source === "ignition") {
    if (item.event.kind === "follow_through_confirmed") return "feed-row feed-row-hit";
    if (item.event.kind === "follow_through_rejected") return "feed-row feed-row-muted";
    return "feed-row";
  }
  if (item.event.kind === "entry_triggered") return "feed-row feed-row-hit";
  return "feed-row";
}

export function IgnitionPanel(props: { items: IgnitionFeedItem[] }) {
  return (
    <PanelShell title="Ignition" subtitle="explosive-move alerts + consolidation breakout" count={props.items.length}>
      {props.items.length === 0 ? (
        <EmptyState>Watching the full universe for a sudden surge…</EmptyState>
      ) : (
        <ul className="feed">
          {props.items.map((item, i) => (
            <li key={i} className={rowClass(item)}>
              <div className="feed-row-main">
                <span className="ticker">{item.event.symbol}</span>
                <span className="price">{formatPrice(item.event.price)}</span>
                {item.source === "consolidation" && <span className="chip chip-accent">CB</span>}
                <span className="dim time">{formatTime(item.event.timestamp)}</span>
              </div>
              <div className="feed-row-kind">
                {item.source === "ignition" ? IGNITION_LABEL[item.event.kind] : CONSOLIDATION_LABEL[item.event.kind]}
              </div>
            </li>
          ))}
        </ul>
      )}
    </PanelShell>
  );
}
