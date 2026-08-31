// Ross Cameron gap-and-go setup panel (doc Panels #1) — every Stage 1/2
// fast-funnel verdict, most recent first. `passed` rows (cleared price +
// float + relative-volume + gap simultaneously) are highlighted; the
// rest still show so a false/near-miss is visible, not hidden.

import type { FunnelSignal } from "@stockspotter/shared-types";
import { formatPct, formatPrice, formatTime, formatVolume } from "../../lib/format";
import { EmptyState, PanelShell } from "../PanelShell";

function Condition(props: { label: string; ok: boolean }) {
  return (
    <span className={`chip ${props.ok ? "chip-good" : "chip-bad"}`}>
      {props.label}
    </span>
  );
}

export function FunnelPanel(props: { signals: FunnelSignal[] }) {
  return (
    <PanelShell title="Gap & Go" subtitle="Stage 1/2 fast funnel" count={props.signals.length}>
      {props.signals.length === 0 ? (
        <EmptyState>Waiting for a symbol to clear the funnel…</EmptyState>
      ) : (
        <ul className="feed">
          {props.signals.map((s, i) => (
            <li key={i} className={`feed-row ${s.passed ? "feed-row-hit" : ""}`}>
              <div className="feed-row-main">
                <span className="ticker">{s.symbol}</span>
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
