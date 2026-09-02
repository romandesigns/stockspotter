// Shared ranked-list row rendering for Top Gainers and Highly Trading --
// was duplicated verbatim in both panels; extracted once both needed the
// same CatalystBadge wiring, so that stays in one place too.

import type { CatalystUpdate } from "@stockspotter/shared-types";
import { CatalystBadge } from "./CatalystBadge";
import { TickerButton } from "./TickerButton";
import { EmptyState } from "./PanelShell";
import { formatPct, formatPrice, formatVolume } from "../lib/format";
import type { Mover, TradingSession } from "../lib/useMovers";

const SESSION_LABEL: Record<TradingSession, string> = {
  premarket: "Premarket",
  regular: "Regular",
  after_hours: "After-Hours",
  overnight: "Overnight",
};

export function MoversList(props: {
  rows: Mover[];
  emptyLabel: string;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
}) {
  if (props.rows.length === 0) {
    return <EmptyState>{props.emptyLabel}</EmptyState>;
  }
  return (
    <ul className="feed">
      {props.rows.map((r, i) => (
        <li key={r.symbol} className="feed-row">
          <div className="feed-row-main">
            <span className="dim movers-rank">{i + 1}</span>
            <TickerButton symbol={r.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved.has(r.symbol)} onToggleSaved={props.onToggleSaved} />
            <CatalystBadge symbol={r.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
            <span className="price">{formatPrice(r.price)}</span>
            <span className={r.changePct >= 0 ? "pct-up" : "pct-down"}>{formatPct(r.changePct)}</span>
            <span className="dim">{formatVolume(r.volume)} vol</span>
            {/* Omitted, not defaulted, when null -- the historical date-
                lookup path genuinely can't classify a session (daily-bar
                resolution only), see Mover.session's own doc comment. */}
            {r.session && <span className="dim">{SESSION_LABEL[r.session]}</span>}
          </div>
        </li>
      ))}
    </ul>
  );
}
