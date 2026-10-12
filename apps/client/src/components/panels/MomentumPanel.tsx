// Confirmed bullish momentum panel (doc Panels #2) — only the moments a
// symbol's momentum score crosses into qualifying territory (see
// deriveConfirmedMomentum), each with its 4-factor breakdown.

import type { CatalystUpdate, MomentumUpdate } from "@stockspotter/shared-types";
import { CatalystBadge } from "../CatalystBadge";
import { TickerButton } from "../TickerButton";
import { formatTime } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";
import { isUnderTwenty } from "../../lib/momentumPrices";

function Factor(props: { label: string; value: number }) {
  return (
    <div className="factor">
      <span className="factor-label">{props.label}</span>
      <div className="factor-bar">
        <div className="factor-bar-fill" style={{ width: `${Math.round(props.value * 100)}%` }} />
      </div>
    </div>
  );
}

export function MomentumPanel(props: {
  confirmations: MomentumUpdate[];
  pricesBySymbol: Map<string, number>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const visible = props.confirmations.filter((m) => isUnderTwenty(props.pricesBySymbol.get(m.symbol)));
  return (
    <PanelShell title="Bullish Momentum" subtitle="Prices < $20 · latest available reading" count={visible.length} className={props.className}>
      {visible.length === 0 ? (
        <EmptyState>No confirmed momentum stocks with an available price under $20.</EmptyState>
      ) : (
        <ul className="feed">
          {visible.map((m, i) => (
            <li key={i} className="feed-row feed-row-hit">
              <div className="feed-row-main">
                <TickerButton symbol={m.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved.has(m.symbol)} onToggleSaved={props.onToggleSaved} />
                <CatalystBadge symbol={m.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
                <span className="score">score {m.overall.toFixed(2)}</span>
                <span className="dim time">{formatTime(m.timestamp)}</span>
              </div>
              <div className="factors">
                <Factor label="volume" value={m.volumeConfirmation} />
                <Factor label="structure" value={m.structure} />
                <Factor label="MA slope" value={m.maSlope} />
                <Factor label="no wick" value={m.wickRejection} />
              </div>
            </li>
          ))}
        </ul>
      )}
    </PanelShell>
  );
}
