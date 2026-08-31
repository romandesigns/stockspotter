// Halt early-warning panel (doc Panels #4, new) — one live-updating card
// per tracked symbol (per the doc's own UI concept), not a scrolling
// feed: ticker, price, a proximity gauge showing how close the current
// move is to the LULD halt threshold, relative volume, and calm/amber/red
// color escalation.

import type { HaltWarning } from "@stockspotter/shared-types";
import { formatPrice, formatTime } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";

export function HaltPanel(props: { readings: HaltWarning[]; className?: string }) {
  return (
    <PanelShell title="Halt Early-Warning" subtitle="proximity to LULD threshold" count={props.readings.length} className={props.className}>
      {props.readings.length === 0 ? (
        <EmptyState>No trades on tracked symbols yet…</EmptyState>
      ) : (
        <div className="halt-grid">
          {props.readings.map((r) => (
            <div key={r.symbol} className={`halt-card halt-${r.level}`}>
              <div className="halt-card-header">
                <span className="ticker">{r.symbol}</span>
                <span className="price">{formatPrice(r.currentPrice)}</span>
              </div>
              <div className="gauge">
                <div className="gauge-fill" style={{ width: `${Math.min(100, r.proximityRatio * 100)}%` }} />
              </div>
              <div className="halt-card-footer">
                <span className="dim">{(r.proximityRatio * 100).toFixed(0)}% of band</span>
                <span className="dim">
                  rel vol {r.relativeVolume === null ? "—" : `${r.relativeVolume.toFixed(1)}x`}
                </span>
                {r.bandDoubled && <span className="chip chip-accent">2x band</span>}
              </div>
              <div className="dim time">{formatTime(r.timestamp)}</div>
            </div>
          ))}
        </div>
      )}
    </PanelShell>
  );
}
