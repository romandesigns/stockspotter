import { useMemo } from "react";
import { Input } from "@/components/ui/input";
import { CatalystsPanel } from "./components/panels/CatalystsPanel";
import { ChartPanel } from "./components/panels/ChartPanel";
import { ConnectionStatus } from "./components/ConnectionStatus";
import { FunnelPanel } from "./components/panels/FunnelPanel";
import { HaltPanel } from "./components/panels/HaltPanel";
import { HighlyTradingPanel } from "./components/panels/HighlyTradingPanel";
import { IgnitionPanel } from "./components/panels/IgnitionPanel";
import { MarketsTodayPanel } from "./components/panels/MarketsTodayPanel";
import { MomentumPanel } from "./components/panels/MomentumPanel";
import { ReplayLauncher } from "./components/ReplayLauncher";
import { TopGainersPanel } from "./components/panels/TopGainersPanel";
import {
  catalystRows,
  deriveConfirmedMomentum,
  deriveIgnitionFeed,
  deriveLatestHaltBySymbol,
  filterFunnelSignals,
} from "./lib/derive";
import { useRealtimeFeed } from "./lib/useRealtimeFeed";
import { useTodayMovers } from "./lib/useMovers";
import { useMarketsToday } from "./lib/useMarketsToday";

// Dashboard shape matches Roman's own target layout (Figma "Web 1920 – 1",
// see stockspotter-ui-target-layout memory) -- a fixed-viewport grid, not
// a growing vertical stack, so nothing requires scrolling the whole page
// to reach. Real panels are placed in their real slots (Halt Early-
// Warning -> the "Alerts/Notifications" slot -- the layout doesn't have
// a halt panel of its own, and a halt warning genuinely is an alert, so
// this is a deliberate real-data mapping, not a placeholder; kept its own
// honest title rather than relabeled "Alerts"). Top Gainers/Highly
// Trading are wired to real universe-wide rankings (market_data::movers,
// via ws-server's /movers/* endpoints). Catalysts turned out to already
// have a real wire event (ScanEvent::CatalystUpdate) flowing from the
// Python qualitative layer -- the earlier placeholder note claiming it
// "wasn't broadcast yet" was stale; only useRealtimeFeed/CatalystsPanel
// were missing. Markets Today is now real too (market_data::indices --
// 4 index-proxy ETFs, ws-server's /markets/today), the last placeholder
// from the original target-layout gap list -- PlaceholderPanel.tsx has
// no remaining callers and was deleted rather than left as dead code.
// Stock Search is still new UI surface with no existing equivalent --
// present visually, not wired to anything yet. The left nav rail's own
// first real content is ReplayLauncher -- a dialog launching the Super
// Chart prototype's Backtest Replay scenario (still a design prototype,
// not ported into this app; see that component's own doc comment).
function App() {
  const { status, events, barsBySymbol, momentumBySymbol, catalystsBySymbol, wsUrl } = useRealtimeFeed();
  const todayMovers = useTodayMovers();
  const marketsToday = useMarketsToday();

  const funnelSignals = useMemo(() => filterFunnelSignals(events), [events]);
  const momentumConfirmations = useMemo(() => deriveConfirmedMomentum(events), [events]);
  const ignitionFeed = useMemo(() => deriveIgnitionFeed(events), [events]);
  const haltReadings = useMemo(() => deriveLatestHaltBySymbol(events), [events]);
  const catalysts = useMemo(() => catalystRows(catalystsBySymbol), [catalystsBySymbol]);

  return (
    <div className="app">
      <header className="app-topbar">
        <h1 className="app-wordmark">stockspotter</h1>
        <Input className="app-search" type="text" placeholder="Stock Search" disabled title="Coming soon" />
        <ConnectionStatus status={status} wsUrl={wsUrl} />
      </header>

      <div className="app-body">
        <nav className="app-rail">
          <ReplayLauncher />
        </nav>

        <main className="dashboard-grid">
          <MomentumPanel confirmations={momentumConfirmations} className="grid-momentum" />
          <ChartPanel barsBySymbol={barsBySymbol} momentumBySymbol={momentumBySymbol} className="grid-chart" />
          <CatalystsPanel rows={catalysts} className="grid-catalysts" />
          <FunnelPanel signals={funnelSignals} className="grid-gapgo" />
          <IgnitionPanel items={ignitionFeed} className="grid-ignition" />
          <TopGainersPanel today={todayMovers} className="grid-topgainers" />
          <HighlyTradingPanel rows={todayMovers.mostActive} className="grid-highlytrading" />
          <HaltPanel readings={haltReadings} className="grid-alerts" />
          <MarketsTodayPanel readings={marketsToday.readings} sparklines={marketsToday.sparklines} className="grid-markets" />
        </main>
      </div>
    </div>
  );
}

export default App;
