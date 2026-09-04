// Nav-rail launcher for monitoring the auto-trader (2026-09-04, Roman's
// own "how can we monitor it" ask) -- same rail-icon + Popover mechanism
// WatchlistPopover already established, not a new pattern. Shows the
// dry-run journal's running stats, currently-open simulated positions,
// and a recent-activity feed (entered/exited/skipped) -- skips are shown
// too, visually de-emphasized rather than hidden, matching the journal's
// own "explain inaction, not just wins" design intent (see
// crates/auto-trader/src/journal.rs's doc comment).

import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ChartIcon } from "./ChartIcon";
import { formatPrice, formatTime } from "../lib/format";
import { useAutoTrader, type JournalEntry, type Strategy } from "../lib/useAutoTrader";

// Short, readable labels for the popover's tight row width -- the wire
// value itself (Strategy, PascalCase) stays the source of truth, this is
// purely display.
const STRATEGY_LABEL: Record<Strategy, string> = {
  FastFunnel: "Funnel",
  MomentumScorer: "Momentum",
  IgnitionDetector: "Ignition",
  ConsolidationBreakout: "Breakout",
  Micropullback: "Micropullback",
};

export function AutoTraderPopover() {
  const status = useAutoTrader();

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="ghost" size="icon" className="app-rail-btn" aria-label="Auto-Trader" title="Auto-Trader (dry run)">
          <ChartIcon name="bot" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" side="right" className="autotrader-popover-content">
        <div className="chart-popover-title">Auto-Trader — dry run</div>

        <div className="autotrader-stats-row">
          <span className="autotrader-stat">
            <span className="autotrader-stat-value">{status.trades}</span>
            <span className="autotrader-stat-label">trades</span>
          </span>
          <span className="autotrader-stat">
            <span className="autotrader-stat-value">
              {status.wins}/{status.losses}
            </span>
            <span className="autotrader-stat-label">W/L</span>
          </span>
          <span className="autotrader-stat">
            <span className={`autotrader-stat-value ${status.cumulativePnlUsd >= 0 ? "pct-up" : "pct-down"}`}>
              {status.cumulativePnlUsd >= 0 ? "+" : ""}
              {status.cumulativePnlUsd.toFixed(2)}
            </span>
            <span className="autotrader-stat-label">sim P&L</span>
          </span>
        </div>

        {status.openPositions.length > 0 && (
          <>
            <div className="chart-popover-divider" />
            <div className="chart-popover-title">Open positions</div>
            <ul className="autotrader-list">
              {status.openPositions.map((p) => (
                <li key={p.symbol} className="autotrader-row">
                  <span className="autotrader-row-symbol">{p.symbol}</span>
                  <span className="dim">
                    {STRATEGY_LABEL[p.strategy]} · {formatPrice(p.entryPrice)} × {p.qty}
                  </span>
                </li>
              ))}
            </ul>
          </>
        )}

        <div className="chart-popover-divider" />
        <div className="chart-popover-title">Recent activity</div>
        {status.recentEntries.length === 0 ? (
          <div className="watchlist-empty">Nothing yet — dry-run only, watching for real Micropullback, Ignition, and Breakout signals.</div>
        ) : (
          <ul className="autotrader-list">
            {status.recentEntries.map((entry, i) => (
              <li key={i} className="autotrader-row">
                <JournalRow entry={entry} />
              </li>
            ))}
          </ul>
        )}
      </PopoverContent>
    </Popover>
  );
}

function JournalRow(props: { entry: JournalEntry }) {
  const entry = props.entry;
  if (entry.type === "entered") {
    return (
      <>
        <span className="autotrader-row-main">
          <span className="autotrader-row-symbol">{entry.symbol}</span>
          <span>
            {STRATEGY_LABEL[entry.strategy]} entered {formatPrice(entry.entryPrice)}
          </span>
        </span>
        <span className="dim">{formatTime(entry.enteredAt)}</span>
      </>
    );
  }
  if (entry.type === "exited") {
    const win = entry.pnlUsd >= 0;
    return (
      <>
        <span className="autotrader-row-main">
          <span className="autotrader-row-symbol">{entry.symbol}</span>
          <span className={win ? "pct-up" : "pct-down"}>
            {win ? "+" : ""}
            {entry.pnlUsd.toFixed(2)} ({entry.exitReason.replace(/_/g, " ")})
          </span>
        </span>
        <span className="dim">{formatTime(entry.exitedAt)}</span>
      </>
    );
  }
  if (entry.type === "stop_adjusted") {
    return (
      <>
        <span className="autotrader-row-main">
          <span className="autotrader-row-symbol">{entry.symbol}</span>
          <span className="dim">stop raised to {formatPrice(entry.newStopPrice)}</span>
        </span>
        <span className="dim">{formatTime(entry.at)}</span>
      </>
    );
  }
  return (
    <>
      <span className="autotrader-row-main autotrader-row-skipped">
        <span className="autotrader-row-symbol">{entry.symbol}</span>
        <span className="dim">skipped — {entry.reason.replace(/_/g, " ")}</span>
      </span>
      <span className="dim">{formatTime(entry.at)}</span>
    </>
  );
}
