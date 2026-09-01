// Makes a symbol's own ticker text the click target for "load this
// symbol into the Super Chart panel" -- the same App.tsx-lifted
// selectedSymbol mechanism CatalystBadge already drives, now available
// on every ticker regardless of whether it has a catalyst. Once selected,
// the chart shows live price action automatically for any currently
// live-tracked symbol (ChartPanel's liveBars already re-derives from
// barsBySymbol on every bar_update) -- no extra wiring needed here, this
// component only ever changes *which* symbol is selected.
//
// `saved`/`onToggleSaved` are optional -- when passed, this also renders
// a star toggle for the watchlist (useWatchlist.ts) right next to the
// ticker. Added here rather than duplicated into every panel that shows
// a ticker: TickerButton is already the one shared mechanism appearing
// in MomentumPanel, FunnelPanel, IgnitionPanel, HaltPanel, and MoversList
// (Top Gainers + Highly Trading), so wiring the star in once here gives
// watchlist-ability everywhere a ticker already is, for the cost of one
// component edit instead of five. Omitted entirely (no star renders) on
// any call site that doesn't pass onToggleSaved -- CatalystsPanel and
// MarketsTodayPanel don't use TickerButton at all (different row shapes)
// and are unaffected either way.

export function TickerButton(props: {
  symbol: string;
  onSelectSymbol: (symbol: string) => void;
  saved?: boolean;
  onToggleSaved?: (symbol: string) => void;
}) {
  return (
    <span className="ticker-group">
      <button
        type="button"
        className="ticker ticker-clickable"
        onClick={(e) => {
          e.stopPropagation();
          props.onSelectSymbol(props.symbol);
        }}
        title={`View ${props.symbol}'s chart`}
      >
        {props.symbol}
      </button>
      {props.onToggleSaved && (
        <button
          type="button"
          className={`ticker-save${props.saved ? " ticker-save-active" : ""}`}
          onClick={(e) => {
            e.stopPropagation();
            props.onToggleSaved!(props.symbol);
          }}
          aria-label={`${props.saved ? "Remove" : "Add"} ${props.symbol} ${props.saved ? "from" : "to"} watchlist`}
          title={`${props.saved ? "Remove from" : "Add to"} watchlist`}
        >
          {props.saved ? "★" : "☆"}
        </button>
      )}
    </span>
  );
}
