import { useEffect, useState } from "react";
import type { ExperimentalStrategyStatus, ExperimentalStrategyUpdate } from "../lib/experimentalStrategies";
import type { ConnectionStatus } from "../lib/useRealtimeFeed";
import { EmptyState, PanelShell } from "./PanelShell";
import { TickerButton } from "./TickerButton";

const names = { iac: "Ignition Anchor Candle", extreme_momentum: "🔥 Extreme Momentum" };
const kinds = { candidate: "Candidate", held: "Observed hold", continued: "Observed continuation", failed: "Anchor low broken", expired: "Observation ended", invalidated: "Evidence incomplete" };

export function ExperimentalStrategies(props: {
  rows: ExperimentalStrategyUpdate[];
  status: ExperimentalStrategyStatus | null;
  gap: boolean;
  connection: ConnectionStatus;
  enabled: boolean;
  error?: string | null;
  onSelectSymbol: (symbol: string) => void;
  saved?: Set<string>;
  onToggleSaved?: (symbol: string) => void;
}) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => { if (!props.enabled) return; const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, [props.enabled]);
  const status = props.status;
  if (!props.enabled || (status && !status.iacEnabled && !status.extremeMomentumEnabled)) return null;
  const stale = !status || props.connection !== "open" || !status.healthy || props.gap
    || status.sourceAtMs === null || now - status.sourceAtMs > 90_000;
  return <section className="experimental-strategies" aria-label="Experimental strategies">
      {(["iac", "extreme_momentum"] as const).map(strategy => {
        const enabled = !status || (strategy === "iac" ? status.iacEnabled : status.extremeMomentumEnabled);
        if (!enabled) return null;
        const rows = props.rows.filter(row => row.strategy === strategy);
        const ready = strategy === "iac" ? status?.iacReadySymbols : status?.extremeReadySymbols;
        const capacityLimited = !!status && ((status.capacityDroppedUpdates ?? 0) > 0 || status.evictions > 0);
        const feedState = props.error ? "Research connection refused" : !status ? "Connecting to research feed" : status.reason === "processing_budget_exceeded"
          ? "Research feed paused" : stale ? "feed incomplete or stale" : capacityLimited ? "Research coverage limited"
          : ready === 0 ? "Histories warming up" : "Connected · completed minute bars";
        return <PanelShell key={strategy} title={names[strategy]} count={rows.length} className="experimental-strategy-panel"
          headerExtra={<span className="experimental-label" title="Count shows retained lifecycle observations, including resolved observations">Experimental</span>}>
          <div className={`experimental-feed-status${stale || capacityLimited || ready === 0 ? " experimental-feed-status-stale" : ""}`}>
            <span>{feedState}</span>
            {status && <span>{ready === undefined ? `${status.activeSymbols} observed symbols` : `${ready} / ${status.activeSymbols} histories ready`} · observed feed only</span>}
          </div>
          {status && status.symbolLimit !== undefined && <details className="experimental-observation-meta dim"><summary>Feed coverage</summary>
            <p>History limit {status.symbolLimit.toLocaleString()} symbols. Readiness means enough continuous bars for the next evaluation; it does not mean a qualifying signal or coverage of the whole market.</p>
            <p>This list holds up to 100 lifecycle observations across both strategies. A reconnect restores only the latest 100 updates, so it is not a complete session log.</p>
            <p>{status.missingMinuteResets ?? 0} missing-minute resets · {status.revisionResets ?? 0} revised-bar resets · {status.invalidBarUpdates ?? 0} stale or invalid updates · {status.idleResets ?? 0} inactive histories reset.</p>
            {capacityLimited && <p>Capacity interrupted coverage: {status.capacityDroppedUpdates ?? 0} incoming updates rejected; {status.evictions} histories evicted. {status.recentCapacityDroppedSymbols?.length ? `Recently excluded: ${status.recentCapacityDroppedSymbols.join(", ")}.` : ""}</p>}
            {(status.outputLaggedEvents ?? 0) > 0 && <p>{status.outputLaggedEvents} research updates missed by connected clients.</p>}
          </details>}
          {rows.length === 0 ? <EmptyState>{props.error ? `Research feed: ${props.error}. Switch Research panels off and on to retry.` : !status ? "Waiting for the research connection." : stale || capacityLimited ? "Results are incomplete; check feed coverage." : ready === 0 ? "Waiting for enough continuous minute bars." : ready !== undefined ? "No candidates among ready histories yet." : "No candidates in the observed feed yet. Histories may still be warming up."}</EmptyState> :
            <ul className="feed">{rows.map(row => <li key={row.lifecycleId} className="feed-row">
              <div className="feed-row-main">
                <TickerButton symbol={row.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved?.has(row.symbol)} onToggleSaved={props.onToggleSaved} />
                <span className="experimental-observation">{kinds[row.kind]} · ${row.observedPrice.toFixed(2)}</span>
                <span className="dim time">{Math.max(0, Math.floor((now - row.eventTimeMs) / 1000))}s since price observation</span>
              </div>
              <div className="experimental-observation-meta dim">Float unavailable</div>
              <details><summary>Why this candidate</summary>
                <p>At the anchor: expansion {row.evidence.expansionRatio.toFixed(2)}×,
                  participation {row.evidence.participationRatio.toFixed(2)}×.
                  {row.evidence.compressionRatio !== null ? ` Compression ${row.evidence.compressionRatio.toFixed(2)}×.` : ""}
                  These observations do not predict a profitable trade.</p>
                <p>Market {new Date(row.eventTimeMs).toLocaleTimeString()} · received {new Date(row.receivedAtMs).toLocaleTimeString()} · available {new Date(row.decisionAtMs).toLocaleTimeString()}</p>
              </details>
            </li>)}</ul>}
        </PanelShell>;
      })}
  </section>;
}
