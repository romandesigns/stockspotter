import { useEffect, useState } from "react";
import type { ExperimentalStrategyStatus, ExperimentalStrategyUpdate } from "../lib/experimentalStrategies";
import type { ConnectionStatus } from "../lib/useRealtimeFeed";

const names = { iac: "Ignition Anchor Candle", extreme_momentum: "🔥 Extreme Momentum" };
const kinds = { candidate: "Candidate", held: "Observed hold", continued: "Observed continuation", failed: "Anchor low broken", expired: "Observation ended", invalidated: "Evidence incomplete" };

export function ExperimentalStrategies(props: {
  rows: ExperimentalStrategyUpdate[];
  status: ExperimentalStrategyStatus | null;
  gap: boolean;
  connection: ConnectionStatus;
  enabled: boolean;
  onSelectSymbol: (symbol: string) => void;
}) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => { if (!props.enabled) return; const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, [props.enabled]);
  const status = props.status;
  if (!props.enabled || (status && !status.iacEnabled && !status.extremeMomentumEnabled)) return null;
  if (!status) return <section className="experimental-strategies" aria-label="Experimental strategies"><p>Experimental strategy feed unavailable or connecting.</p></section>;
  const stale = props.connection !== "open" || !status.healthy || props.gap
    || status.sourceAtMs === null || now - status.sourceAtMs > 90_000;
  return <section className="experimental-strategies" aria-label="Experimental strategies">
    <p>Experimental · completed minute bars · observed symbols only{stale ? " · feed incomplete or stale" : ""}{status.reason === "processing_budget_exceeded" ? " · research feed paused" : ""}</p>
    <div className="experimental-strategy-columns">
      {(["iac", "extreme_momentum"] as const).map(strategy => {
        const enabled = strategy === "iac" ? status.iacEnabled : status.extremeMomentumEnabled;
        if (!enabled) return null;
        const rows = props.rows.filter(row => row.strategy === strategy);
        return <section key={strategy} aria-label={names[strategy]}>
          <h2>{names[strategy]}</h2>
          {rows.length === 0 ? <p>No candidates in the observed feed yet. Warmup and coverage may be incomplete.</p> :
            <ul>{rows.map(row => <li key={row.lifecycleId}>
              <button type="button" onClick={() => props.onSelectSymbol(row.symbol)}>{row.symbol}</button>
              <span>{kinds[row.kind]} · ${row.observedPrice.toFixed(2)}</span>
              <small>{Math.max(0, Math.floor((now - row.eventTimeMs) / 1000))}s since price observation · float unavailable</small>
              <details><summary>Why this candidate</summary>
                <p>At the anchor: expansion {row.evidence.expansionRatio.toFixed(2)}×,
                  participation {row.evidence.participationRatio.toFixed(2)}×.
                  {row.evidence.compressionRatio !== null ? ` Compression ${row.evidence.compressionRatio.toFixed(2)}×.` : ""}
                  These observations do not predict a profitable trade.</p>
                <p>Market {new Date(row.eventTimeMs).toLocaleTimeString()} · received {new Date(row.receivedAtMs).toLocaleTimeString()} · available {new Date(row.decisionAtMs).toLocaleTimeString()}</p>
              </details>
            </li>)}</ul>}
        </section>;
      })}
    </div>
  </section>;
}
