import { useMemo } from "react";
import { ChartPanel } from "./components/panels/ChartPanel";
import { ConnectionStatus } from "./components/ConnectionStatus";
import { FunnelPanel } from "./components/panels/FunnelPanel";
import { HaltPanel } from "./components/panels/HaltPanel";
import { IgnitionPanel } from "./components/panels/IgnitionPanel";
import { MomentumPanel } from "./components/panels/MomentumPanel";
import {
  deriveConfirmedMomentum,
  deriveIgnitionFeed,
  deriveLatestHaltBySymbol,
  filterFunnelSignals,
} from "./lib/derive";
import { useRealtimeFeed } from "./lib/useRealtimeFeed";

function App() {
  const { status, events, barsBySymbol, wsUrl } = useRealtimeFeed();

  const funnelSignals = useMemo(() => filterFunnelSignals(events), [events]);
  const momentumConfirmations = useMemo(() => deriveConfirmedMomentum(events), [events]);
  const ignitionFeed = useMemo(() => deriveIgnitionFeed(events), [events]);
  const haltReadings = useMemo(() => deriveLatestHaltBySymbol(events), [events]);

  return (
    <div className="app">
      <header className="app-header">
        <h1>stockspotter</h1>
        <ConnectionStatus status={status} wsUrl={wsUrl} />
      </header>

      <div className="chart-section">
        <ChartPanel barsBySymbol={barsBySymbol} />
      </div>

      <main className="panel-grid">
        <FunnelPanel signals={funnelSignals} />
        <MomentumPanel confirmations={momentumConfirmations} />
        <IgnitionPanel items={ignitionFeed} />
        <HaltPanel readings={haltReadings} />
      </main>
    </div>
  );
}

export default App;
