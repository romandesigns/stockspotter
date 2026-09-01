import { useMemo } from "react";
import { Input } from "@/components/ui/input";
import { ChartPanel } from "./components/panels/ChartPanel";
import { ConnectionStatus } from "./components/ConnectionStatus";
import { FunnelPanel } from "./components/panels/FunnelPanel";
import { HaltPanel } from "./components/panels/HaltPanel";
import { HighlyTradingPanel } from "./components/panels/HighlyTradingPanel";
import { IgnitionPanel } from "./components/panels/IgnitionPanel";
import { MomentumPanel } from "./components/panels/MomentumPanel";
import { PlaceholderPanel } from "./components/panels/PlaceholderPanel";
import { TopGainersPanel } from "./components/panels/TopGainersPanel";
import {
  deriveConfirmedMomentum,
  deriveIgnitionFeed,
  deriveLatestHaltBySymbol,
  filterFunnelSignals,
} from "./lib/derive";
import { useRealtimeFeed } from "./lib/useRealtimeFeed";
import { useTodayMovers } from "./lib/useMovers";

// Dashboard shape matches Roman's own target layout (Figma "Web 1920 – 1",
// see stockspotter-ui-target-layout memory) -- a fixed-viewport grid, not
// a growing vertical stack, so nothing requires scrolling the whole page
// to reach. Real panels are placed in their real slots (Halt Early-
// Warning -> the "Alerts/Notifications" slot -- the layout doesn't have
// a halt panel of its own, and a halt warning genuinely is an alert, so
// this is a deliberate real-data mapping, not a placeholder; kept its own
// honest title rather than relabeled "Alerts"). Top Gainers/Highly
// Trading are wired to real universe-wide rankings (market_data::movers,
// via ws-server's /movers/* endpoints) -- Catalysts/Markets Today still
// have no backend at all (per that memory's own gap list) and stay
// honestly-labeled placeholders in the correct position/size so the
// overall shape matches the reference, not faked data and not silently
// dropped (which would break the layout proportions). Stock Search and
// the left nav rail are new UI surface with no existing equivalent --
// present visually, not wired to anything yet.
function App() {
  const { status, events, barsBySymbol, momentumBySymbol, wsUrl } = useRealtimeFeed();
  const todayMovers = useTodayMovers();

  const funnelSignals = useMemo(() => filterFunnelSignals(events), [events]);
  const momentumConfirmations = useMemo(() => deriveConfirmedMomentum(events), [events]);
  const ignitionFeed = useMemo(() => deriveIgnitionFeed(events), [events]);
  const haltReadings = useMemo(() => deriveLatestHaltBySymbol(events), [events]);

  return (
    <div className="app">
      <header className="app-topbar">
        <h1 className="app-wordmark">stockspotter</h1>
        <Input className="app-search" type="text" placeholder="Stock Search" disabled title="Coming soon" />
        <ConnectionStatus status={status} wsUrl={wsUrl} />
      </header>

      <div className="app-body">
        <nav className="app-rail" aria-hidden="true" />

        <main className="dashboard-grid">
          <MomentumPanel confirmations={momentumConfirmations} className="grid-momentum" />
          <ChartPanel barsBySymbol={barsBySymbol} momentumBySymbol={momentumBySymbol} className="grid-chart" />
          <PlaceholderPanel
            title="Catalysts"
            note="News catalyst tags are already computed server-side (Python qualitative layer) but not broadcast over ws-server yet — real data, needs wiring."
            className="grid-catalysts"
          />
          <FunnelPanel signals={funnelSignals} className="grid-gapgo" />
          <IgnitionPanel items={ignitionFeed} className="grid-ignition" />
          <TopGainersPanel today={todayMovers} className="grid-topgainers" />
          <HighlyTradingPanel rows={todayMovers.mostActive} className="grid-highlytrading" />
          <HaltPanel readings={haltReadings} className="grid-alerts" />
          <PlaceholderPanel
            title="Markets Today"
            note="No backend yet — needs a broad index/market snapshot, not tied to any single-symbol strategy."
            className="grid-markets"
          />
        </main>
      </div>
    </div>
  );
}

export default App;
