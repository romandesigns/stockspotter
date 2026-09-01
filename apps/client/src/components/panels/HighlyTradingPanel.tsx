// Highly Trading panel -- stocks most active during the current session,
// ranked by raw session share volume across the whole tracked universe
// (see market_data::movers's doc comment on why raw volume, not relative
// volume: relative volume already has its own home in the funnel/halt
// panels). Always the live session -- no date toggle, unlike Top Gainers.

import { formatPct, formatPrice, formatVolume } from "../../lib/format";
import type { Mover } from "../../lib/useMovers";
import { EmptyState, PanelShell } from "../PanelShell";

export function HighlyTradingPanel(props: { rows: Mover[]; className?: string }) {
  return (
    <PanelShell title="Highly Trading" subtitle="most active, current session" count={props.rows.length} className={props.className}>
      {props.rows.length === 0 ? (
        <EmptyState>Waiting for the universe scan's first pass…</EmptyState>
      ) : (
        <ul className="feed">
          {props.rows.map((r, i) => (
            <li key={r.symbol} className="feed-row">
              <div className="feed-row-main">
                <span className="dim movers-rank">{i + 1}</span>
                <span className="ticker">{r.symbol}</span>
                <span className="price">{formatPrice(r.price)}</span>
                <span className={r.changePct >= 0 ? "pct-up" : "pct-down"}>{formatPct(r.changePct)}</span>
                <span className="dim">{formatVolume(r.volume)} vol</span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </PanelShell>
  );
}
