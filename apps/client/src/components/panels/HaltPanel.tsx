// Halt early-warning panel (doc Panels #4, new) — the N riskiest tracked
// symbols, one live-updating card each: ticker, price, a radial pressure
// gauge showing how close the current move is to the LULD halt
// threshold, relative volume, and calm/amber/red color escalation.
// Capped rather than showing every tracked symbol (the panel's original
// behavior) -- readings are already sorted by proximityRatio descending
// (deriveLatestHaltBySymbol), so the top slice really is "the N stocks
// under the most halt pressure right now", not an arbitrary truncation.
//
// N is picked from a header dropdown (LIMIT_OPTIONS/limit state below),
// the same headerExtra slot and real shadcn Select this app already uses
// for a per-panel header control elsewhere (Top Gainers' own headerExtra
// is a date picker, not a count picker -- there's no existing "N items"
// dropdown to port verbatim, so this one is new, but it reuses the exact
// same Select primitive ChartPanel's symbol picker already uses rather
// than a plain native <select>).
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

import { useState } from "react";
import type { CatalystUpdate, HaltWarning } from "@stockspotter/shared-types";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CatalystBadge } from "../CatalystBadge";
import { PressureGauge } from "../PressureGauge";
import { TickerButton } from "../TickerButton";
import { formatPrice, formatTime } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";

const LIMIT_OPTIONS = [3, 6, 10, 15, 20];
const DEFAULT_LIMIT = 6;

export function HaltPanel(props: {
  readings: HaltWarning[];
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const [limit, setLimit] = useState(DEFAULT_LIMIT);
  const top = props.readings.slice(0, limit);

  const limitPicker = (
    <Select value={String(limit)} onValueChange={(v) => setLimit(Number(v))}>
      <SelectTrigger size="sm" className="halt-limit-select-trigger" aria-label="Number of symbols to show">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {LIMIT_OPTIONS.map((n) => (
          <SelectItem key={n} value={String(n)}>
            Top {n}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );

  return (
    <PanelShell
      title="Halt Early-Warning"
      subtitle={`top ${limit} by proximity to LULD threshold`}
      count={top.length}
      headerExtra={limitPicker}
      className={props.className}
    >
      {top.length === 0 ? (
        <EmptyState>No trades on tracked symbols yet…</EmptyState>
      ) : (
        <div className="halt-grid">
          {top.map((r) => {
            const bullish = r.currentPrice >= r.referencePrice;
            return (
            <div key={r.symbol} className={`halt-card halt-${r.level} ${bullish ? "halt-bullish" : "halt-bearish"}`}>
              <div className="halt-card-body">
                <PressureGauge proximityRatio={r.proximityRatio} bullish={bullish} />
                <div className="halt-card-info">
                  <div className="halt-card-header">
                    <span className="halt-card-ticker-group">
                      <TickerButton symbol={r.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved.has(r.symbol)} onToggleSaved={props.onToggleSaved} />
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
