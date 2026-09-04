import { useMemo, useState } from "react";
import { Group, Panel, Separator, useDefaultLayout } from "react-resizable-panels";
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
import { IgnitionAlertToast } from "./components/IgnitionAlertToast";
import { MomentumPanel } from "./components/panels/MomentumPanel";
import { ReplayLauncher } from "./components/ReplayLauncher";
import { ResetLayoutButton } from "./components/ResetLayoutButton";
import { TopGainersPanel } from "./components/panels/TopGainersPanel";
import { WatchlistPopover } from "./components/WatchlistPopover";
import {
  catalystRows,
  deriveIgnitionFeed,
  deriveLatestHaltBySymbol,
} from "./lib/derive";
import { useIsNarrowViewport } from "./lib/useIsNarrowViewport";
import { useMicropullbackAlerts } from "./lib/useMicropullbackAlerts";
import { useIgnitionAlerts } from "./lib/useIgnitionAlerts";
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
    ignitionConfirmedEvents,
    wsUrl,
  } = useRealtimeFeed();
  const todayMovers = useTodayMovers();
  const marketsToday = useMarketsToday();
  const { saved, toggleSaved } = useWatchlist();
  const [selectedSymbol, setSelectedSymbol] = useState<string | null>(null);
  const { toasts, dismissToast } = useMicropullbackAlerts(micropullbackEvents, momentumBySymbol);
  // Same real mechanism, wider net (2026-09-04, real gap found live --
  // see useIgnitionAlerts.ts's own header comment): any symbol's
  // confirmed ignition, not just Micropullback triggers.
  const { toasts: ignitionToasts, dismissToast: dismissIgnitionToast } = useIgnitionAlerts(ignitionConfirmedEvents);
  const isNarrow = useIsNarrowViewport();
  // Bumped by ResetLayoutButton to force the whole Group tree to remount
  // (React `key`) once its own persisted localStorage entries have been
  // cleared -- the only reliable way to make react-resizable-panels
  // forget a saved layout and re-initialize from each Panel's own
  // defaultSize, short of it exposing its own imperative reset API.
  const [layoutResetKey, setLayoutResetKey] = useState(0);
  // One useDefaultLayout call per Group -- it owns reading/writing
  // localStorage itself (default storage, confirmed from the library's
  // own source), keyed by `id` (stored under
  // `react-resizable-panels:<id>`, which is why ResetLayoutButton
  // matches on the substring "dashboardLayout" rather than needing to
  // know that exact prefix). Three separate ids (not one) so each row's
  // column widths and the outer row-height split persist independently,
  // matching the three-Group tree below.
  const outerLayout = useDefaultLayout({ id: "stockspotter.dashboardLayout.outer.v1" });
  const row1Layout = useDefaultLayout({ id: "stockspotter.dashboardLayout.row1.v1" });
  const row2Layout = useDefaultLayout({ id: "stockspotter.dashboardLayout.row2.v1" });

  const ignitionFeed = useMemo(() => deriveIgnitionFeed(events), [events]);
  const haltReadings = useMemo(() => deriveLatestHaltBySymbol(events), [events]);
  const catalysts = useMemo(() => catalystRows(catalystsBySymbol), [catalystsBySymbol]);

  // Built once, rendered into whichever tree below actually applies
  // (resizable on a wide viewport, plain stacked below the 1400px
  // breakpoint) -- same 9 panels, same props, just two different parent
  // structures. Position no longer comes from a `className="grid-*"`
  // prop (that was CSS Grid's own mechanism) -- these panels don't need
  // one anymore, order in the tree below is what places them now.
  const momentumPanel = (
    <MomentumPanel confirmations={momentumConfirmations} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} />
  );
  const chartPanel = (
    <ChartPanel
      barsBySymbol={barsBySymbol}
      subMinuteBarsBySymbol={subMinuteBarsBySymbol}
      momentumBySymbol={momentumBySymbol}
      catalystsBySymbol={catalystsBySymbol}
      selectedSymbol={selectedSymbol}
      onSelectedSymbolChange={setSelectedSymbol}
    />
  );
  const catalystsPanel = <CatalystsPanel rows={catalysts} momentumBySymbol={momentumBySymbol} onSelectSymbol={setSelectedSymbol} />;
  const funnelPanel = <FunnelPanel signals={funnelSignals} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} />;
  const ignitionPanel = <IgnitionPanel items={ignitionFeed} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} />;
  const topGainersPanel = <TopGainersPanel today={todayMovers} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} />;
  const highlyTradingPanel = (
    <HighlyTradingPanel
      rows={todayMovers.mostActive}
      lastUpdated={todayMovers.lastUpdated}
      catalystsBySymbol={catalystsBySymbol}
      saved={saved}
      onToggleSaved={toggleSaved}
      onSelectSymbol={setSelectedSymbol}
    />
  );
  const haltPanel = <HaltPanel readings={haltReadings} catalystsBySymbol={catalystsBySymbol} saved={saved} onToggleSaved={toggleSaved} onSelectSymbol={setSelectedSymbol} />;
  const marketsTodayPanel = <MarketsTodayPanel readings={marketsToday.readings} sparklines={marketsToday.sparklines} />;

  return (
    <div className="app">
      <MicropullbackToast toasts={toasts} onDismiss={dismissToast} onSelectSymbol={setSelectedSymbol} />
      <IgnitionAlertToast toasts={ignitionToasts} onDismiss={dismissIgnitionToast} onSelectSymbol={setSelectedSymbol} />
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
          <ResetLayoutButton onReset={() => setLayoutResetKey((n) => n + 1)} />
        </nav>

        {isNarrow ? (
          // Same intent the old CSS-only fallback documented ("a desktop-
          // viewport promise, not a claim this fits a phone") -- just a
          // plain scrolling stack, no resize handles, below the breakpoint.
          <main className="dashboard-stack">
            {momentumPanel}
            {chartPanel}
            {catalystsPanel}
            {funnelPanel}
            {ignitionPanel}
            {topGainersPanel}
            {highlyTradingPanel}
            {haltPanel}
            {marketsTodayPanel}
          </main>
        ) : (
          // Sizes are percentage STRINGS, not numbers -- react-resizable-
          // panels interprets a bare number as PIXELS, only a string
          // without units as a percentage of the parent Group (see its
          // own PanelProps doc comment). Explicit `id` on every Panel:
          // useDefaultLayout's persisted layout is a map keyed by panel
          // id, so a stable id (not the useId() fallback) is required for
          // a saved layout to reapply to the right panel after a reload.
          <Group
            key={layoutResetKey}
            id="stockspotter.dashboardLayout.outer.v1"
            orientation="vertical"
            className="dashboard-panelgroup"
            defaultLayout={outerLayout.defaultLayout}
            onLayoutChanged={outerLayout.onLayoutChanged}
          >
            <Panel id="row1" defaultSize="55" minSize="30">
              <Group id="stockspotter.dashboardLayout.row1.v1" orientation="horizontal" defaultLayout={row1Layout.defaultLayout} onLayoutChanged={row1Layout.onLayoutChanged}>
                <Panel id="momentum" defaultSize="20" minSize="10">{momentumPanel}</Panel>
                <Separator className="dashboard-resize-handle" />
                <Panel id="chart" defaultSize="60" minSize="30">{chartPanel}</Panel>
                <Separator className="dashboard-resize-handle" />
                <Panel id="catalysts" defaultSize="20" minSize="10">{catalystsPanel}</Panel>
              </Group>
            </Panel>
            <Separator className="dashboard-resize-handle" />
            <Panel id="row2" defaultSize="32" minSize="15">
              <Group id="stockspotter.dashboardLayout.row2.v1" orientation="horizontal" defaultLayout={row2Layout.defaultLayout} onLayoutChanged={row2Layout.onLayoutChanged}>
                <Panel id="gapgo" defaultSize="20" minSize="10">{funnelPanel}</Panel>
                <Separator className="dashboard-resize-handle" />
                <Panel id="ignition" defaultSize="20" minSize="10">{ignitionPanel}</Panel>
                <Separator className="dashboard-resize-handle" />
                <Panel id="topgainers" defaultSize="20" minSize="10">{topGainersPanel}</Panel>
                <Separator className="dashboard-resize-handle" />
                <Panel id="highlytrading" defaultSize="20" minSize="10">{highlyTradingPanel}</Panel>
                <Separator className="dashboard-resize-handle" />
                <Panel id="alerts" defaultSize="20" minSize="10">{haltPanel}</Panel>
              </Group>
            </Panel>
            <Separator className="dashboard-resize-handle" />
            <Panel id="markets" defaultSize="13" minSize="8">{marketsTodayPanel}</Panel>
          </Group>
        )}
      </div>
    </div>
  );
}

export default App;
