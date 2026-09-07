// Ross Cameron gap-and-go setup panel (doc Panels #1) — every Stage 1/2
// fast-funnel verdict, most recent first. `passed` rows (cleared price +
// float + relative-volume + gap simultaneously) are highlighted; the
// rest still show so a false/near-miss is visible, not hidden.

import type { CatalystUpdate, FunnelHealth, FunnelSignal } from "@stockspotter/shared-types";
import { CatalystBadge } from "../CatalystBadge";
import { TickerButton } from "../TickerButton";
import { formatPct, formatPrice, formatTime, formatVolume } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";
import { funnelBlindReason } from "../../lib/panelHealth";

function Condition(props: { label: string; ok: boolean }) {
  return (
    <span className={`chip ${props.ok ? "chip-good" : "chip-bad"}`}>
      {props.label}
    </span>
  );
}

export function FunnelPanel(props: {
  signals: FunnelSignal[];
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  /** Latest Stage-1 float-budget health; null before the first rescan. */
  health?: FunnelHealth | null;
  className?: string;
}) {
  // Extracted to lib/panelHealth.ts so the decision itself is unit
  // tested (see panelHealth.test.ts) rather than only exercised through
  // a rendered component.
  const blindReason = funnelBlindReason(props.health);

  return (
    <PanelShell title="Gap & Go" subtitle="Stage 1/2 fast funnel" count={props.signals.length} className={props.className}>
      {blindReason && <div className="panel-warning">{blindReason}</div>}
      {props.signals.length === 0 ? (
        <EmptyState>
          {blindReason ? "Funnel is blind right now — see above." : "Waiting for a symbol to clear the funnel…"}
        </EmptyState>
      ) : (
        <ul className="feed">
          {props.signals.map((s, i) => (
            <li key={i} className={`feed-row ${s.passed ? "feed-row-hit" : ""}`}>
              <div className="feed-row-main">
                <TickerButton symbol={s.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved.has(s.symbol)} onToggleSaved={props.onToggleSaved} />
                <CatalystBadge symbol={s.symbol} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
                <span className="price">{formatPrice(s.price)}</span>
                <span className={s.gapPct >= 0 ? "pct-up" : "pct-down"}>{formatPct(s.gapPct)}</span>
                <span className="dim">{formatVolume(s.sessionVolume)} vol</span>
                <span className="dim time">{formatTime(s.timestamp)}</span>
              </div>
              <div className="feed-row-conditions">
                <Condition label="price" ok={s.priceOk} />
                <Condition label="float" ok={s.floatOk} />
                <Condition label="rel vol" ok={s.relVolOk} />
                <Condition label="gap" ok={s.gapOk} />
              </div>
            </li>
          ))}
        </ul>
      )}
    </PanelShell>
  );
}
