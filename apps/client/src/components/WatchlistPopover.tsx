// Nav-rail launcher for the persistent Watchlist (useWatchlist.ts) --
// same rail-icon mechanism ReplayLauncher already established for the
// left rail (Button variant="ghost" size="icon" className="app-rail-btn"
// as the trigger), but opens into a real shadcn Popover here instead of
// a Dialog: this is a lightweight list, not a full feature surface with
// its own toolbar/playback controls, so the lighter primitive (already
// used for ReplayLauncher's own Sessions filter, and SessionDatePicker's
// calendar) is the right fit, not the heavier one.
//
// Doesn't need its own data source: saved comes from the same
// useWatchlist() App.tsx already calls for every panel's star toggle,
// and barsBySymbol is the same live map ChartPanel already reads --
// reused here only to show a saved symbol's latest price/change when
// it's currently live-tracked, not fetched specially for this popover.
// A saved symbol the live universe scan isn't currently returning bars
// for shows an honest "not currently tracked" instead of a fabricated
// price.

import type { BarUpdate } from "@stockspotter/shared-types";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ChartIcon } from "./ChartIcon";
import { TickerButton } from "./TickerButton";
import { formatPct, formatPrice } from "../lib/format";

export function WatchlistPopover(props: {
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  barsBySymbol: Map<string, BarUpdate[]>;
  onSelectSymbol: (symbol: string) => void;
}) {
  const symbols = [...props.saved].sort();

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="ghost" size="icon" className="app-rail-btn" aria-label="Watchlist" title="Watchlist">
          <ChartIcon name="star" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" side="right" className="watchlist-popover-content">
        <div className="chart-popover-title">Watchlist</div>
        {symbols.length === 0 ? (
          <div className="watchlist-empty">Star a ticker anywhere to add it here.</div>
        ) : (
          <ul className="watchlist-list">
            {symbols.map((symbol) => {
              const bars = props.barsBySymbol.get(symbol);
              const last = bars && bars.length > 0 ? bars[bars.length - 1] : null;
              const first = bars && bars.length > 0 ? bars[0] : null;
              const changePct = last && first && first.open !== 0 ? ((last.close - first.open) / first.open) * 100 : null;
              return (
                <li key={symbol} className="watchlist-row">
                  <TickerButton symbol={symbol} onSelectSymbol={props.onSelectSymbol} saved onToggleSaved={props.onToggleSaved} />
                  {last ? (
                    <span className="watchlist-row-price">
                      <span className="price">{formatPrice(last.close)}</span>
                      {changePct !== null && (
                        <span className={changePct >= 0 ? "pct-up" : "pct-down"}>{formatPct(changePct)}</span>
                      )}
                    </span>
                  ) : (
                    <span className="dim">not currently tracked</span>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </PopoverContent>
    </Popover>
  );
}
