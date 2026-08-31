// Confirmed bullish momentum panel (doc Panels #2) — only the moments a
// symbol's momentum score crosses into qualifying territory (see
// deriveConfirmedMomentum), each with its 4-factor breakdown.

import type { MomentumUpdate } from "@stockspotter/shared-types";
import { formatTime } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";

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

export function MomentumPanel(props: { confirmations: MomentumUpdate[] }) {
  return (
    <PanelShell title="Bullish Momentum" subtitle="confirmed qualifications" count={props.confirmations.length}>
      {props.confirmations.length === 0 ? (
        <EmptyState>No symbol has crossed the momentum threshold yet…</EmptyState>
      ) : (
        <ul className="feed">
          {props.confirmations.map((m, i) => (
            <li key={i} className="feed-row feed-row-hit">
              <div className="feed-row-main">
                <span className="ticker">{m.symbol}</span>
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
