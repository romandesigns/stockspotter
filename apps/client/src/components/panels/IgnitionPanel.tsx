// Ignition / explosive-move alert panel (doc Panels #3) — a live feed of
// ignition candidate/follow-through events, plus the post-ignition
// consolidation-breakout entry strategy folded in as a tagged row (not a
// separate panel — see deriveIgnitionFeed's doc comment).

import { useMemo, useState } from "react";
import type { CatalystUpdate } from "@stockspotter/shared-types";
import { qualifiesForUserAttention, USER_ATTENTION_PRICE_CEILING } from "@stockspotter/shared-types";
import { CatalystBadge } from "../CatalystBadge";
import { TickerButton } from "../TickerButton";
import { formatPrice, formatTime } from "../../lib/format";
import type { IgnitionFeedItem } from "../../lib/derive";
import { rowClass } from "../../lib/ignitionRow";
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


export function IgnitionPanel(props: {
  items: IgnitionFeedItem[];
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  // Default is the user's own attention universe: green AND at or below the
  // user's price ceiling. Two filters, measured separately on the 2026-09-21
  // regular session -- green alone removes 97.4% of Ignition-family rows, and
  // the price ceiling then removes a further 45.8% of what survives (65,256
  // of 142,472 confirmations were above $25).
  //
  // "All" deliberately shows the raw stream, including green events above the
  // ceiling: those are real detector signals, just outside the universe the
  // user trades, and hiding them from diagnostics too would lose information.
  const [showAll, setShowAll] = useState(false);
  const visible = useMemo(
    () => (showAll ? props.items : props.items.filter((it) => qualifiesForUserAttention(it.event))),
    [props.items, showAll],
  );
  // The count follows what is on screen. Showing a raw total beside a short
  // list would misdescribe the panel; "All" mode makes the raw total visible
  // in its own right.
  return (
    <PanelShell
      title="Ignition"
      subtitle={showAll ? "all detector events (diagnostic)" : `confirmed follow-through + breakout entries ≤ $${USER_ATTENTION_PRICE_CEILING.toFixed(0)}`}
      count={visible.length}
      className={props.className}
      headerExtra={
        <button
          type="button"
          className="ignition-mode-toggle"
          onClick={() => setShowAll((v) => !v)}
          title={
            showAll
              ? "Showing every Ignition event, including candidates, rejections and prices above the ceiling"
              : `Showing only confirmed follow-through and breakout entries at or below $${USER_ATTENTION_PRICE_CEILING.toFixed(2)}`
          }
        >
          {showAll ? "All" : "Signals"}
        </button>
      }
    >
      {visible.length === 0 ? (
        <EmptyState>Watching the full universe for a sudden surge…</EmptyState>
      ) : (
        <ul className="feed">
          {visible.map((item, i) => (
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
