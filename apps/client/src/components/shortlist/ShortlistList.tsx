// Today's Shortlist -- the ranked list half of the Shortlist view
// (ShortlistView.tsx). Ranked by gap % x relative volume (research
// finding R3), which is a WATCHLIST ordering, not an entry signal -- the
// header says so in plain words rather than leaving the rank to imply it.
//
// Rows aren't buttons themselves (TickerButton inside a <button> would be
// nested interactive content); a row click and the ticker button both
// drive the same App-lifted selectedSymbol, and the ticker button is the
// keyboard target -- same pattern every other panel's rows already use.

import type { CatalystUpdate, ShortlistResponse } from "@stockspotter/shared-types";
import { assessTradability } from "@stockspotter/shared-types";
import { CatalystBadge } from "../CatalystBadge";
import { EmptyState, PanelShell } from "../PanelShell";
import { TickerButton } from "../TickerButton";
import { UpdatedAgo } from "../UpdatedAgo";
import { formatPct, formatPrice } from "../../lib/format";
import { SESSION_LABEL, shortlistObservedAt } from "../../lib/shortlist";
import type { ShortlistState } from "../../lib/useShortlist";
import { ShortlistChangesFeed } from "./ShortlistChangesFeed";
import { TradabilityDot } from "./TradabilityDot";

function emptyMessage(body: ShortlistResponse): string {
  if (body.session === "overnight") return "No candidates overnight — the shortlist rebuilds once premarket trading starts.";
  return `Nothing qualifies right now — ${body.poolSize} symbol${body.poolSize === 1 ? "" : "s"} checked.`;
}

export function ShortlistList(props: {
  shortlist: ShortlistState;
  selectedSymbol: string | null;
  onSelectSymbol: (symbol: string) => void;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
}) {
  const { body, error, changes } = props.shortlist;
  const excluded = body ? body.excluded.outsidePriceRange + body.excluded.notGappingUp + body.excluded.noRelativeVolume : 0;

  return (
    <PanelShell
      title="Today's Shortlist"
      count={body?.candidates.length}
      headerExtra={body && <UpdatedAgo lastUpdated={shortlistObservedAt(body)} />}
      className="shortlist-list-panel"
    >
      <div className="shortlist-intro">
        <p className="shortlist-subtitle">Ranked by gap × relative volume — where to look, not when to enter</p>
        {body && (
          <p className="dim">
            {SESSION_LABEL[body.session]} · {body.poolSize} in pool
            {excluded > 0 && (
              <span
                className="shortlist-excluded"
                title={`Outside price range: ${body.excluded.outsidePriceRange} · Not gapping up: ${body.excluded.notGappingUp} · No relative volume: ${body.excluded.noRelativeVolume}`}
              >
                {" "}· {excluded} excluded
              </span>
            )}
          </p>
        )}
      </div>
      {error && body && <div className="panel-warning">Couldn't refresh ({error}) — showing the last list received.</div>}
      {!body ? (
        <EmptyState>{error ? `Shortlist unavailable: ${error}` : "Loading shortlist…"}</EmptyState>
      ) : body.candidates.length === 0 ? (
        <EmptyState>{emptyMessage(body)}</EmptyState>
      ) : (
        <ul className="feed shortlist-rows" aria-label="Shortlist candidates">
          {body.candidates.map((c) => {
            const selected = c.symbol === props.selectedSymbol;
            const tradability = assessTradability(c);
            return (
              <li
                key={c.symbol}
                className={`shortlist-row${selected ? " shortlist-row-selected" : ""}`}
                aria-current={selected ? "true" : undefined}
                onClick={() => props.onSelectSymbol(c.symbol)}
              >
                <span className="shortlist-rank dim">{c.rank}</span>
                <span className="shortlist-symbol">
                  <TickerButton symbol={c.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved.has(c.symbol)} onToggleSaved={props.onToggleSaved} />
                  <CatalystBadge symbol={c.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
                </span>
                <span className="price">{formatPrice(c.price)}</span>
                <span className={c.gapPct >= 0 ? "pct-up" : "pct-down"}>{formatPct(c.gapPct)}</span>
                <span className="shortlist-rvol" title="Relative volume">{c.relativeVolume.toFixed(1)}×</span>
                <TradabilityDot assessment={tradability} />
              </li>
            );
          })}
        </ul>
      )}
      {body && <ShortlistChangesFeed changes={changes} onSelectSymbol={props.onSelectSymbol} />}
    </PanelShell>
  );
}
