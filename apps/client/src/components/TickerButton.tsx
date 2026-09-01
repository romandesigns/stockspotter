// Makes a symbol's own ticker text the click target for "load this
// symbol into the Super Chart panel" -- the same App.tsx-lifted
// selectedSymbol mechanism CatalystBadge already drives, now available
// on every ticker regardless of whether it has a catalyst. Once selected,
// the chart shows live price action automatically for any currently
// live-tracked symbol (ChartPanel's liveBars already re-derives from
// barsBySymbol on every bar_update) -- no extra wiring needed here, this
// component only ever changes *which* symbol is selected.

export function TickerButton(props: { symbol: string; onSelectSymbol: (symbol: string) => void }) {
  return (
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
  );
}
