import { useMemo, useState } from "react";
import { Input } from "@/components/ui/input";
import { AutoTraderPopover } from "./components/AutoTraderPopover";
import { CatalystsPanel } from "./components/panels/CatalystsPanel";
import { ChartPanel } from "./components/panels/ChartPanel";
import { ConnectionStatus } from "./components/ConnectionStatus";
import { FunnelPanel } from "./components/panels/FunnelPanel";
import { HaltPanel } from "./components/panels/HaltPanel";
import { HighlyTradingPanel } from "./components/panels/HighlyTradingPanel";
import { IgnitionPanel } from "./components/panels/IgnitionPanel";
import { MarketsTodayPanel } from "./components/panels/MarketsTodayPanel";
import { MicropullbackToast } from "./components/MicropullbackToast";
import { MomentumPanel } from "./components/panels/MomentumPanel";
import { ReplayLauncher } from "./components/ReplayLauncher";
import { TopGainersPanel } from "./components/panels/TopGainersPanel";
import { WatchlistPopover } from "./components/WatchlistPopover";
import {
  catalystRows,
  deriveIgnitionFeed,
  deriveLatestHaltBySymbol,
} from "./lib/derive";
import { useMicropullbackAlerts } from "./lib/useMicropullbackAlerts";
import { useRealtimeFeed } from "./lib/useRealtimeFeed";
import { useTodayMovers } from "./lib/useMovers";
import { useMarketsToday } from "./lib/useMarketsToday";
import { useWatchlist } from "./lib/useWatchlist";

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
//
// selectedSymbol is lifted here (not local to ChartPanel) so any panel's
// CatalystBadge, or a CatalystsPanel row, can drive what the chart shows
// -- Roman's "actionable, practical" ask for the Catalysts panel: click a
// catalyst, jump straight to that symbol's chart, instead of it only
// ever being readable text. catalystsBySymbol is threaded into every
// panel that renders a ticker which made it through a detection gate,
// per Roman's other half of the same request.
function App() {
  const {
    status,
    events,
    barsBySymbol,
    subMinuteBarsBySymbol,
    momentumBySymbol,
    catalystsBySymbol,
    funnelSignals,
    momentumConfirmations,
    micropullbackEvents,
    wsUrl,
  } = useRealtimeFeed();
  const todayMovers = useTodayMovers();
  const marketsToday = useMarketsToday();
  const { saved, toggleSaved } = useWatchlist();
  const [selectedSymbol, setSelectedSymbol] = useState<string | null>(null);
  const { toasts, dismissToast } = useMicropullbackAlerts(micropullbackEvents, momentumBySymbol);

  const ignitionFeed = useMemo(() => deriveIgnitionFeed(events), [events]);
  const haltReadings = useMemo(() => deriveLatestHaltBySymbol(events), [events]);
  const catalysts = useMemo(() => catalystRows(catalystsBySymbol), [catalystsBySymbol]);

  return (
    <div className="app">
      <MicropullbackToast toasts={toasts} onDismiss={dismissToast} onSelectSymbol={setSelectedSymbol} />
      <header className="app-topbar">
        <h1 className="app-wordmark">stockspotter</h1>
        <Input className="app-search" type="text" placeholder="Stock Search" disabled title="Coming soon" />
        <ConnectionStatus status={status} wsUrl={wsUrl} />
      </header>

      <div className="app-body">
        <nav className="app-rail">
          <ReplayLauncher />
          <WatchlistPopover saved={saved} onToggleSaved={toggleSaved} barsBySymbol={barsBySymbol} onSelectSymbol={setSelectedSymbol} />
          <AutoTraderPopover />
        </nav>

        <main className="dashboard-grid">
          <MomentumPanel
            confirmations={momentumConfirmations}
            catalystsBySymbol={catalystsBySymbol}
            saved={saved}
            onToggleSaved={toggleSaved}
            onSelectSymbol={setSelectedSymbol}
            className="grid-momentum"
          />
          <ChartPanel
            barsBySymbol={barsBySymbol}
            subMinuteBarsBySymbol={subMinuteBarsBySymbol}
            momentumBySymbol={momentumBySymbol}
            catalystsBySymbol={catalystsBySymbol}
            selectedSymbol={selectedSymbol}
            onSelectedSymbolChange={setSelectedSymbol}
            className="grid-chart"
          />
          <CatalystsPanel rows={catalysts} momentumBySymbol={momentumBySymbol} onSelectSymbol={setSelectedSymbol} className="grid-catalysts" />
          <FunnelPanel signals={funnelSignals} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} className="grid-gapgo" />
          <IgnitionPanel items={ignitionFeed} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} className="grid-ignition" />
          <TopGainersPanel today={todayMovers} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} className="grid-topgainers" />
          <HighlyTradingPanel
            rows={todayMovers.mostActive}
            lastUpdated={todayMovers.lastUpdated}
            catalystsBySymbol={catalystsBySymbol}
            saved={saved}
            onToggleSaved={toggleSaved}
            onSelectSymbol={setSelectedSymbol}
            className="grid-highlytrading"
          />
          <HaltPanel readings={haltReadings} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} className="grid-alerts" />
          <MarketsTodayPanel readings={marketsToday.readings} sparklines={marketsToday.sparklines} className="grid-markets" />
        </main>
      </div>
    </div>
  );
}

export default App;
