// Per-ticker catalyst indicator, reused across every panel that renders
// a symbol that made it through one of the detection gates (Gap & Go,
// Ignition, Halt, Bullish Momentum, Top Gainers, Highly Trading, the
// chart header) -- Roman: "the stocks that make it through our gates to
// any of our panels should have an icon that indicates whether there's
// any relevant catalyst associated with it." Renders nothing at all for
// a symbol with no catalyst record (real absence, not a placeholder
// icon) -- sourced from the same catalystsBySymbol map every panel
// already has access to (useRealtimeFeed + its own /catalysts/today
// backfill), not a second data path.
//
// Clicking it is the "actionable, practical" half of the same request --
// jumps straight to that symbol's chart (App.tsx's lifted selectedSymbol
// state) instead of only ever being readable text in the Catalysts panel.

import type { CatalystUpdate } from "@stockspotter/shared-types";
import { ChartIcon } from "./ChartIcon";
import { formatTag } from "../lib/format";

export function CatalystBadge(props: {
  symbol: string;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  onSelectSymbol: (symbol: string) => void;
}) {
  const record = props.catalystsBySymbol.get(props.symbol);
  if (!record || record.catalystTags.length === 0) return null;

  const tagList = record.catalystTags.map(formatTag).join(", ");

  return (
    <button
      type="button"
      className="catalyst-badge"
      title={`${tagList} — view ${props.symbol}'s chart`}
      aria-label={`${props.symbol} has a catalyst: ${tagList}`}
      onClick={(e) => {
        // Rows this sits inside are sometimes themselves clickable
        // (CatalystsPanel) -- stop that handler from also firing and
        // fighting over which symbol actually gets selected.
        e.stopPropagation();
        props.onSelectSymbol(props.symbol);
      }}
    >
      <ChartIcon name="catalyst" />
    </button>
  );
}
