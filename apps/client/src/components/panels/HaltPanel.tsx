// Halt early-warning panel (doc Panels #4, new) — the top 6 riskiest
// tracked symbols (per Roman's own "top 6" ask), one live-updating card
// each: ticker, price, a radial pressure gauge showing how close the
// current move is to the LULD halt threshold, relative volume, and
// calm/amber/red color escalation. Capped rather than showing every
// tracked symbol (the panel's previous behavior) -- readings are already
// sorted by proximityRatio descending (deriveLatestHaltBySymbol), so the
// top slice really is "the 6 stocks under the most halt pressure right
// now", not an arbitrary truncation.
//
// The gauge itself is PressureGauge (a real Recharts RadialBarChart, see
// that component's own doc comment) instead of the flat linear bar this
// panel used to draw by hand -- a circular dial reads more intuitively
// as "pressure toward a threshold" than a linear fill, which is the
// whole "momentum pressure" ask.
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

import type { CatalystUpdate, HaltWarning } from "@stockspotter/shared-types";
import { CatalystBadge } from "../CatalystBadge";
import { PressureGauge } from "../PressureGauge";
import { TickerButton } from "../TickerButton";
import { formatPrice, formatTime } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";

const TOP_N = 6;

export function HaltPanel(props: {
  readings: HaltWarning[];
  catalystsBySymbol: Map<string, CatalystUpdate>;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const top = props.readings.slice(0, TOP_N);

  return (
    <PanelShell title="Halt Early-Warning" subtitle={`top ${TOP_N} by proximity to LULD threshold`} count={top.length} className={props.className}>
      {top.length === 0 ? (
        <EmptyState>No trades on tracked symbols yet…</EmptyState>
      ) : (
        <div className="halt-grid">
          {top.map((r) => {
            const bullish = r.currentPrice >= r.referencePrice;
            return (
            <div key={r.symbol} className={`halt-card halt-${r.level} ${bullish ? "halt-bullish" : "halt-bearish"}`}>
              <div className="halt-card-body">
                <PressureGauge proximityRatio={r.proximityRatio} level={r.level} />
                <div className="halt-card-info">
                  <div className="halt-card-header">
                    <span className="halt-card-ticker-group">
                      <TickerButton symbol={r.symbol} onSelectSymbol={props.onSelectSymbol} />
                      <CatalystBadge symbol={r.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
                    </span>
                    <span className={bullish ? "price pct-up" : "price pct-down"}>
                      {bullish ? "▲" : "▼"} {formatPrice(r.currentPrice)}
                    </span>
                  </div>
                  <div className="halt-card-footer">
                    <span className="dim">
                      rel vol {r.relativeVolume === null ? "—" : `${r.relativeVolume.toFixed(1)}x`}
                    </span>
                    {r.bandDoubled && <span className="chip chip-accent">2x band</span>}
                  </div>
                  <div className="dim time">{formatTime(r.timestamp)}</div>
                </div>
              </div>
            </div>
            );
          })}
        </div>
      )}
    </PanelShell>
  );
}
