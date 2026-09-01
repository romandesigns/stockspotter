// Halt early-warning panel (doc Panels #4, new) — one live-updating card
// per tracked symbol (per the doc's own UI concept), not a scrolling
// feed: ticker, price, a proximity gauge showing how close the current
// move is to the LULD halt threshold, relative volume, and calm/amber/red
// color escalation.
//
// Direction (bullish/bearish) added as a colored left-accent stripe --
// real, not fabricated: halt_detector's own proximity_ratio is
// (price - reference).abs() / band, direction-agnostic by design (it only
// cares how close to the threshold, not which one), but currentPrice and
// referencePrice are both already on the wire, so which side price sits
// on is a direct, honest computation from data already sent, not a new
// backend field. Kept as a border accent rather than folded into the
// background, which already carries the calm/amber/red urgency tint --
// two different signals, so two different visual channels (same
// reasoning .feed-row's own accent stripe used).

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
          {props.readings.map((r) => {
            const bullish = r.currentPrice >= r.referencePrice;
            return (
            <div key={r.symbol} className={`halt-card halt-${r.level} ${bullish ? "halt-bullish" : "halt-bearish"}`}>
              <div className="halt-card-header">
                <span className="ticker">{r.symbol}</span>
                <span className={bullish ? "price pct-up" : "price pct-down"}>
                  {bullish ? "▲" : "▼"} {formatPrice(r.currentPrice)}
                </span>
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
            );
          })}
        </div>
      )}
    </PanelShell>
  );
}
