// Shared ranked-list row rendering for Top Gainers and Highly Trading --
// was duplicated verbatim in both panels; extracted once both needed the
// same CatalystBadge wiring, so that stays in one place too.

import type { CatalystUpdate } from "@stockspotter/shared-types";
import { CatalystBadge } from "./CatalystBadge";
import { TickerButton } from "./TickerButton";
import { EmptyState } from "./PanelShell";
import { formatPct, formatPrice, formatVolume } from "../lib/format";
import type { Mover, TradingSession } from "../lib/useMovers";

// Abbreviated -- there isn't room for "After-Hours"/"Overnight" spelled
// out alongside price/pct/volume/star at this panel's real width (see
// .movers-row-main's own comment). SESSION_TITLE carries the full word
// as a hover tooltip instead of just cutting it silently.
const SESSION_LABEL: Record<TradingSession, string> = {
  premarket: "Pre",
  regular: "Reg",
  after_hours: "AH",
  overnight: "ON",
};
const SESSION_TITLE: Record<TradingSession, string> = {
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
          {/* A real grid, not a packed flex row -- fixed-width columns for
              price/pct/volume/session so they line up down the whole list
              regardless of how wide any one row's symbol/price text is,
              with the symbol column absorbing the leftover space (1fr) so
              the row always spans the panel's full width and the star
              lands flush at the true right edge, not wherever the last
              piece of text happened to end. The save star moved out of
              TickerButton (which still renders it, unstarred, in every
              OTHER panel that reuses it) into its own final grid column
              here -- scoped to this list only. */}
          <div className="movers-row-main">
            <span className="dim movers-rank">{i + 1}</span>
            <span className="movers-symbol-cell">
              <TickerButton symbol={r.symbol} onSelectSymbol={props.onSelectSymbol} />
              <CatalystBadge symbol={r.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
            </span>
            <span className="price">{formatPrice(r.price)}</span>
            <span className={r.changePct >= 0 ? "pct-up" : "pct-down"}>{formatPct(r.changePct)}</span>
            {/* "vol" suffix dropped -- no room for it at this panel's real
                width, and the Highly Trading/Top Gainers panel title
                already makes clear what the number is. */}
            <span className="dim">{formatVolume(r.volume)}</span>
            {/* Always rendered, even when empty -- the grid has a fixed
                7-column template, so omitting this cell for the historical
                date-lookup path (session: null) would shift the star into
                this column instead of removing a column cleanly. Content
                is still real-or-nothing, matching Mover.session's own
                doc comment: never a fabricated label. */}
            <span className="dim" title={r.session ? SESSION_TITLE[r.session] : undefined}>
              {r.session ? SESSION_LABEL[r.session] : ""}
            </span>
            <button
              type="button"
              className={`ticker-save${props.saved.has(r.symbol) ? " ticker-save-active" : ""}`}
              onClick={(e) => {
                e.stopPropagation();
                props.onToggleSaved(r.symbol);
              }}
              aria-label={`${props.saved.has(r.symbol) ? "Remove" : "Add"} ${r.symbol} ${props.saved.has(r.symbol) ? "from" : "to"} watchlist`}
              title={`${props.saved.has(r.symbol) ? "Remove from" : "Add to"} watchlist`}
            >
              {props.saved.has(r.symbol) ? "★" : "☆"}
            </button>
          </div>
        </li>
      ))}
    </ul>
  );
}
