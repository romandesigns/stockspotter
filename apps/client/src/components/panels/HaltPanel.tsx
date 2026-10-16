// Halt early-warning panel, redesigned 2026-10-16 as a SPEED meter
// (approved Main/Regular boards). Roman: most of his real profit came from
// this panel, in premarket and regular hours alike -- i.e. mostly when no
// halt happens. What he reads is halt_detector's proximity_ratio (|price -
// 5-minute rolling reference| / LULD band width), which is computed in
// every session; only `level` is pinned to calm outside 09:30-16:00. So
// the panel now leads with "how fast is it moving" and adds what it could
// not say before: the gauge's last 10 minutes (sparkline), how fast it is
// filling ("+N pts / 1m", "filling fast"), how long the run has lasted,
// speed alerts, and what happened after each alert ("Recent alerts").
// Every number comes from shared-types' haltSpeed module via useHaltSpeed
// so the phone shows the same values.
//
// Premarket cards are NOT dimmed any more (the old .halt-dormant opacity):
// premarket is the session Roman uses this panel most, and dimming made
// exactly those cards read as unimportant. The subtitle says once that no
// LULD halt can happen before 9:30 instead.
//
// Kept from before: the PressureGauge ring (from Roman's own mockup:
// starts at 6 o'clock, sweeps left when bullish, right when bearish), the
// direction stripe, the Top N Select, and in regular hours the calm/amber/
// red background tint -- now with words ("nearing band", "at the limit-up
// band"), since a tint alone does not say what it means.
//
// Card click: the whole card opens the chart through one full-card
// overlay <button> behind the content (a "stretched" button), so the
// ticker's own buttons (chart, watchlist star, catalyst badge) stay real,
// separately focusable buttons instead of buttons nested in a button.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { SPEED_HISTORY_WINDOW_MS, movingLabel, type CatalystUpdate, type HaltWarning, type SpeedDirection, type SpeedPoint, type SpeedRow } from "@stockspotter/shared-types";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { CatalystBadge } from "../CatalystBadge";
import { PressureGauge } from "../PressureGauge";
import { SpeedAlertSettings } from "../SpeedAlertSettings";
import { TickerButton } from "../TickerButton";
import { formatPrice } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";
import { isLuldInEffect, isOutsideLuldHours } from "../../lib/panelHealth";
import { alertChangePct, formatClock, signedPct, sparklinePoints, speedSummary } from "../../lib/haltSpeedView";
import type { HaltSpeedState } from "../../lib/useHaltSpeed";

const LIMIT_OPTIONS = [3, 6, 10, 15, 20];
const DEFAULT_LIMIT = 6;
const RECENT_ALERTS_SHOWN = 5;
const SPARK_W = 100;
const SPARK_H = 20;

/** Wall clock for "moving N min" and the NEW tag; 5 s is finer than
 * either label can change. */
function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

/** Cards re-rank on every trade. Instead of jumping, each card slides from
 * where it was (FLIP). Skipped under prefers-reduced-motion. */
function useFlip(container: React.RefObject<HTMLDivElement | null>, order: string) {
  const previous = useRef(new Map<string, number>());
  useLayoutEffect(() => {
    const root = container.current;
    if (!root) return;
    const reduce = typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const next = new Map<string, number>();
    for (const el of root.querySelectorAll<HTMLElement>("[data-flip-key]")) {
      const key = el.dataset.flipKey!;
      const top = el.offsetTop;
      next.set(key, top);
      const before = previous.current.get(key);
      if (reduce || before === undefined || before === top) continue;
      el.style.transition = "none";
      el.style.transform = `translateY(${before - top}px)`;
      requestAnimationFrame(() => {
        el.style.transition = "transform 220ms ease-out";
        el.style.transform = "";
      });
    }
    previous.current = next;
  }, [container, order]);
}

function Sparkline(props: { points: SpeedPoint[]; threshold: number | null; bullish: boolean; pct: number }) {
  const line = sparklinePoints(props.points, SPARK_W, SPARK_H, SPEED_HISTORY_WINDOW_MS);
  const thresholdY = props.threshold === null ? null : 1 + SPARK_H - (props.threshold / 100) * SPARK_H;
  return (
    <svg className="halt-spark" viewBox={`0 0 ${SPARK_W} ${SPARK_H + 2}`} preserveAspectRatio="none" role="img" aria-label={`Gauge over the last 10 minutes, now ${props.pct}%`}>
      <line x1="0" y1={SPARK_H + 1} x2={SPARK_W} y2={SPARK_H + 1} className="halt-spark-base" vectorEffect="non-scaling-stroke" />
      {thresholdY !== null && <line x1="0" y1={thresholdY} x2={SPARK_W} y2={thresholdY} className="halt-spark-threshold" vectorEffect="non-scaling-stroke" />}
      {props.points.length > 1 && (
        <polyline points={line} transform="translate(0 1)" className={props.bullish ? "halt-spark-line pct-up" : "halt-spark-line pct-down"} vectorEffect="non-scaling-stroke" />
      )}
    </svg>
  );
}

function levelWords(row: SpeedRow): string | null {
  if (row.reading.level === "amber") return "nearing band";
  if (row.reading.level === "red") return row.bullish ? "at the limit-up band" : "at the limit-down band";
  return null;
}

function HaltCard(props: {
  row: SpeedRow;
  now: number;
  isNew: boolean;
  threshold: number | null;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: boolean;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
}) {
  const { row } = props;
  const r = row.reading;
  const inEffect = isLuldInEffect(r);
  const moving = movingLabel(row.moving, props.now);
  const words = inEffect ? levelWords(row) : null;
  const level = inEffect ? r.level : "calm";
  return (
    <div data-flip-key={r.symbol} className={`halt-card halt-${level} ${row.bullish ? "halt-bullish" : "halt-bearish"}`}>
      <button type="button" className="halt-card-open" aria-label={`Open ${r.symbol} chart`} onClick={() => props.onSelectSymbol(r.symbol)} />
      <div className="halt-card-content">
        <div className="halt-card-body">
          <PressureGauge proximityRatio={r.proximityRatio} bullish={row.bullish} />
          <div className="halt-card-info">
            <div className="halt-card-header">
              <span className="halt-card-ticker-group">
                <TickerButton symbol={r.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved} onToggleSaved={props.onToggleSaved} />
                <CatalystBadge symbol={r.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
              </span>
              <span className={row.bullish ? "price pct-up" : "price pct-down"}>
                {row.bullish ? "▲" : "▼"} {formatPrice(r.currentPrice)}
              </span>
              {props.isNew && <span className="halt-new-tag">NEW</span>}
            </div>
            <div className="halt-card-move">
              {row.movePct === null ? "—" : signedPct(row.movePct)} <span className="dim">in 5 min{moving ? ` · moving ${moving}` : ""}</span>
            </div>
            <div className="halt-card-footer">
              <span className="dim">rel vol {r.relativeVolume === null ? "—" : `${r.relativeVolume.toFixed(1)}x`}</span>
              {row.fillingFast && <span className="halt-fast-chip">filling fast</span>}
              {words && <span className={`halt-level-words halt-level-words-${r.level}`}>{words}</span>}
              {inEffect && r.bandDoubled && <span className="chip">2x band</span>}
              {inEffect && (
                <span className="halt-bands dim" title={r.estimatedBands === false ? "Price bands supplied by SIP" : "Estimated bands; exchange eligibility and opening rules may differ"}>
                  {r.estimatedBands === false ? "SIP bands" : "estimated bands"}
                </span>
              )}
            </div>
          </div>
        </div>
        <div className="halt-card-spark-row">
          <Sparkline points={row.points} threshold={props.threshold} bullish={row.bullish} pct={row.pct} />
          <span className="halt-card-delta">{row.delta1m === null ? "— / 1m" : `${row.delta1m >= 0 ? "+" : "−"}${Math.abs(row.delta1m)} pts / 1m`}</span>
        </div>
      </div>
    </div>
  );
}

export function HaltPanel(props: {
  readings: HaltWarning[];
  speed: HaltSpeedState;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const { speed } = props;
  const [limit, setLimit] = useState(DEFAULT_LIMIT);
  const now = useNow(5000);
  const top = speed.rows.slice(0, limit);
  // LULD bands only exist 9:30-16:00 ET; outside them every gauge is
  // real but nothing can halt. Said once, here, rather than per card.
  const outsideLuldHours = isOutsideLuldHours(props.readings);
  const summary = speedSummary(speed.rows, speed.recentAlerts);
  const threshold = speed.settings.enabled ? speed.settings.threshold : null;
  const priceBySymbol = useMemo(() => new Map(props.readings.map((r) => [r.symbol, r.currentPrice])), [props.readings]);
  const grid = useRef<HTMLDivElement | null>(null);
  useFlip(grid, top.map((row) => row.reading.symbol).join(","));

  const headerExtra = (
    <>
      <SpeedAlertSettings settings={speed.settings} onChange={speed.setSettings} />
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
    </>
  );

  const emptyDirection = speed.direction === "up" ? "No stock is moving up right now." : speed.direction === "down" ? "No stock is moving down right now." : null;

  return (
    <PanelShell
      id="halt-panel"
      tabIndex={-1}
      title="Halt Early-Warning"
      count={top.length}
      headerExtra={headerExtra}
      className={props.className ? `halt-panel ${props.className}` : "halt-panel"}
    >
      {/* Not PanelShell's subtitle: .panel-subtitle is hidden app-wide,
          and this line is what tells premarket readers why nothing can
          halt yet, so it has to be visible. */}
      {props.readings.length > 0 && (
        <p className="halt-subtitle">
          {outsideLuldHours
            ? "Fastest moves vs the last 5 min · premarket: no LULD halts until 9:30"
            : "Fastest moves vs the last 5 min · LULD bands in force"}
        </p>
      )}
      {props.readings.length === 0 ? (
        <EmptyState>No trades on tracked symbols yet…</EmptyState>
      ) : (
        <div className="halt-speed">
          <ToggleGroup
            type="single"
            size="sm"
            spacing={0}
            className="halt-segmented"
            aria-label="Direction"
            value={speed.direction}
            onValueChange={(v) => v && speed.setDirection(v as SpeedDirection)}
          >
            <ToggleGroupItem value="up" className="halt-segmented-item">▲ Up {speed.counts.up}</ToggleGroupItem>
            <ToggleGroupItem value="down" className="halt-segmented-item">▼ Down {speed.counts.down}</ToggleGroupItem>
            <ToggleGroupItem value="all" className="halt-segmented-item">All</ToggleGroupItem>
          </ToggleGroup>
          {summary.length > 0 && (
            <p className="halt-summary">
              {summary[0].strong && <span className="halt-summary-dot" aria-hidden="true" />}
              <span>
                {summary.map((part, i) => (
                  <span key={part.text}>
                    {i > 0 && " · "}
                    {part.strong ? <strong>{part.text}</strong> : part.text}
                  </span>
                ))}
              </span>
            </p>
          )}
          {top.length === 0 ? (
            <EmptyState>{emptyDirection}</EmptyState>
          ) : (
            <div className="halt-grid" ref={grid}>
              {top.map((row) => (
                <HaltCard
                  key={row.reading.symbol}
                  row={row}
                  now={now}
                  isNew={speed.isNew(row.reading.symbol, now)}
                  threshold={threshold}
                  catalystsBySymbol={props.catalystsBySymbol}
                  saved={props.saved.has(row.reading.symbol)}
                  onToggleSaved={props.onToggleSaved}
                  onSelectSymbol={props.onSelectSymbol}
                />
              ))}
            </div>
          )}
          {speed.recentAlerts.length > 0 && (
            <section className="halt-recent" aria-labelledby="halt-recent-title">
              <div className="halt-recent-header">
                <h3 id="halt-recent-title">Recent alerts</h3>
                <span className="dim">since the alert</span>
              </div>
              <ul>
                {speed.recentAlerts.slice(0, RECENT_ALERTS_SHOWN).map((alert) => {
                  const current = priceBySymbol.get(alert.symbol);
                  const change = alertChangePct(alert.price, current);
                  return (
                    <li key={alert.id} className="halt-recent-row">
                      <span className="dim">{formatClock(alert.at)}</span>
                      <button type="button" className="halt-recent-symbol" onClick={() => props.onSelectSymbol(alert.symbol)} title={`View ${alert.symbol}'s chart`}>
                        {alert.symbol}
                      </button>
                      <span className="dim halt-recent-prices">
                        {formatPrice(alert.price)} → {current === undefined ? "—" : formatPrice(current)}
                      </span>
                      <span className={change === null ? "dim" : change >= 0 ? "pct-up" : "pct-down"}>{change === null ? "—" : signedPct(change)}</span>
                    </li>
                  );
                })}
              </ul>
            </section>
          )}
        </div>
      )}
    </PanelShell>
  );
}
