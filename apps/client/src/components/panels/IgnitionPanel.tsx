// Ignition / explosive-move alert panel (doc Panels #3) — a live feed of
// ignition candidate/follow-through events, plus the post-ignition
// consolidation-breakout entry strategy folded in as a tagged row (not a
// separate panel — see deriveIgnitionFeed's doc comment).

import type { CatalystUpdate } from "@stockspotter/shared-types";
import { CatalystBadge } from "../CatalystBadge";
import { TickerButton } from "../TickerButton";
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

// Micropullback's entry_triggered gets its own wording -- "act fast" is
// the whole point of this signal (a 1-candle pause resuming within
// seconds), and reusing "breakout entry" verbatim would read identically
// to the slower, already-validated consolidation-breakout signal it's
// deliberately meant to be faster than. See ConsolidationStrategy's own
// doc comment in shared-types.
const MICROPULLBACK_LABEL: Record<string, string> = {
  surge_detected: "surge detected",
  consolidation_confirmed: "pullback holding",
  entry_triggered: "micropullback entry — act fast",
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

export function IgnitionPanel(props: {
  items: IgnitionFeedItem[];
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  return (
    <PanelShell title="Ignition" subtitle="explosive-move alerts + consolidation breakout" count={props.items.length} className={props.className}>
      {props.items.length === 0 ? (
        <EmptyState>Watching the full universe for a sudden surge…</EmptyState>
      ) : (
        <ul className="feed">
          {props.items.map((item, i) => (
            <li key={i} className={rowClass(item)}>
              <div className="feed-row-main">
                <TickerButton symbol={item.event.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved.has(item.event.symbol)} onToggleSaved={props.onToggleSaved} />
                <CatalystBadge symbol={item.event.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
                <span className="price">{formatPrice(item.event.price)}</span>
                {item.source === "consolidation" && item.event.strategy === "micropullback" && (
                  <span className="chip chip-warning" title="Micropullback: a 1-candle pause resuming within seconds — faster, thinner-evidence entry than a standard consolidation breakout">
                    MPB
                  </span>
                )}
                {item.source === "consolidation" && item.event.strategy === "consolidation_breakout" && <span className="chip chip-accent">CB</span>}
                <span className="dim time">{formatTime(item.event.timestamp)}</span>
              </div>
              <div className="feed-row-kind">
                {item.source === "ignition"
                  ? IGNITION_LABEL[item.event.kind]
                  : item.event.strategy === "micropullback"
                    ? MICROPULLBACK_LABEL[item.event.kind]
                    : CONSOLIDATION_LABEL[item.event.kind]}
              </div>
            </li>
          ))}
        </ul>
      )}
    </PanelShell>
  );
}
