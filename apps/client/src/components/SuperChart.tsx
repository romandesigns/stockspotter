// Thin React wrapper around the actual Super Chart engine
// (../lib/superChartEngine.ts) — that file IS the ported
// mountSuperChart()/wireChartTooltip() from the Artifact prototype
// (stockspotter-super-chart-prototype memory), not a reimplementation of
// it. This component's job is exactly what the prototype's own
// page-level "Scenario 1 wiring" did: call mountSuperChart() into a
// container, call wireChartTooltip() on the result, and wire toolbar/
// popover controls that call directly into the returned api — nothing in
// here re-derives the chart's own rendering logic.
//
// Real per-piece notes:
// - The chart itself (candles/volume/MA9/MA20/VWAP/MACD pane/backdrop/
//   resize handle/tooltip) is superChartEngine.ts, called here, not
//   rebuilt here.
// - 1m/5m/15m timeframe pills use resample() (chartIndicators.ts, a
//   verbatim port of the prototype's own bucket-by-wall-clock-time fix).
//   1D is present but disabled: needs a new daily-bar data source, real
//   backend scope, not silently omitted.
// - Settings popover (autoScale/scaleMode/fitIndicators, now also chart
//   type) and the Indicators popover are the prototype's own handler
//   bodies, calling into the same api the engine returns. The
//   prototype-era MACD-toggle margin hack (fallback price/vol margins
//   hardcoded per on/off state, bypassing paneMargins()) is GONE now,
//   not carried over: it assumed MACD was the only possible bottom
//   oscillator, an assumption RSI breaks (toggling MACD off can't
//   reclaim the whole bottom zone for price/volume anymore when RSI is
//   still occupying half of it). Every bottom-oscillator toggle (MACD,
//   RSI) is now a plain visibility flip with no margin recompute at
//   all -- toggling one off leaves its half of the zone blank rather
//   than the other growing to fill it, the same real trade-off the old
//   hack already had for whichever indicator WASN'T what it hardcoded
//   for, just applied uniformly now instead of only to MACD.
// - Symbol switches remount the chart (mountSuperChart is called fresh),
//   matching the prototype's own model of one instance per mounted
//   context. Toolbar/settings state now SURVIVES a symbol switch
//   (2026-09-03, Roman's explicit ask) rather than resetting to
//   defaults -- the mount effect re-applies the current React state to
//   each freshly-mounted engine instance instead of resetting the state
//   itself to match a fresh engine's own defaults.
// - Momentum panel: real momentum_scorer::MomentumScore data, not
//   ported from the prototype (its "84 / Strong Bullish" text was static
//   demo copy, never computed from a formula — confirmed by reading its
//   source). Our own thresholds in momentumLabel.ts.
// - Toolbar/popover controls (buttons, popovers, switches, radio group,
//   timeframe pills) are real shadcn/Radix components now, not hand-
//   rolled button/div lookalikes — real focus trapping, Escape-to-close,
//   and outside-click dismissal come from Radix's Popover itself, which
//   is also why the old manual document-mousedown listener is gone.
// - Still genuinely deferred: symbol markers, extended-hours filtering,
//   session-highlight shading, and the backtest/watchlist CHART_PRESETS
//   contexts (only `scanner` is wired to real data so far).
import { useEffect, useMemo, useRef, useState } from "react";
import { PriceScaleMode } from "lightweight-charts";
import type { MomentumUpdate } from "@stockspotter/shared-types";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { ChartIcon } from "./ChartIcon";
import type { CandleBar } from "../lib/derive";
import { resample, sma } from "../lib/chartIndicators";
import { mountSuperChart, wireChartTooltip, type ChartType, type SuperChartApi } from "../lib/superChartEngine";
import { factorGood, momentumLabel } from "../lib/momentumLabel";
import { maSlopeDetail, structureDetail, volumeConfirmationDetail, wickRejectionDetail } from "../lib/momentumNarrative";
import { useAssessment } from "../lib/useAssessment";
import { useWakeLock } from "../lib/useWakeLock";

const TIMEFRAMES = [1, 5, 15] as const;
/** "30s" is a real, distinct case, not a fifth entry in TIMEFRAMES' own
 * minute-multiplier list — see displayBars' own comment for why it can't
 * go through resample() the same way the numeric ones do. */
type Timeframe = (typeof TIMEFRAMES)[number] | "30s";
type ScaleMode = "linear" | "percent" | "log";
type IndicatorKey = "ma9" | "ma20" | "vwap" | "macd" | "rsi" | "bollinger";

// Swatch dot colors in the Indicators popover — same fixed order/values
// as --series-1..7 in index.css (which the engine itself reads live via
// getComputedStyle for the actual chart rendering; these are only for
// the little UI dots in the menu, kept as plain constants rather than a
// second getComputedStyle call for a decorative element).
const MA9_COLOR = "#3987e5";
const MA20_COLOR = "#d95926";
const VWAP_COLOR = "#9085e9";
const MACD_LINE_COLOR = "#c98500";
const RSI_COLOR = "#2ec4b6";
const BOLLINGER_COLOR = "#7c93a8";

const CHART_TYPE_OPTIONS: { value: ChartType; label: string }[] = [
  { value: "candles", label: "Candlestick" },
  { value: "line", label: "Line" },
];

export function SuperChart(props: { symbol: string; bars: CandleBar[]; subMinuteBars: CandleBar[]; momentum: MomentumUpdate | null }) {
  const panelRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const apiRef = useRef<SuperChartApi | null>(null);
  const barsRef = useRef<CandleBar[]>(props.bars);
  barsRef.current = props.bars;
  // Real sub-minute (30s) live-only bars (2026-09-03) -- a genuinely
  // separate array from props.bars, not derivable from it (Alpaca has no
  // sub-minute historical data at all, confirmed live against its own
  // API; this only ever grows forward from whenever the symbol started
  // being tracked). Same ref-for-the-mount-effect pattern as barsRef.
  const subMinuteBarsRef = useRef<CandleBar[]>(props.subMinuteBars);
  subMinuteBarsRef.current = props.subMinuteBars;

  const [visible, setVisible] = useState<Record<IndicatorKey, boolean>>({ ma9: true, ma20: true, vwap: true, macd: true, rsi: true, bollinger: true });
  const [autoScale, setAutoScale] = useState(true);
  const [scaleMode, setScaleMode] = useState<ScaleMode>("linear");
  const [fitIndicators, setFitIndicators] = useState(true);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [timeframe, setTimeframe] = useState<Timeframe>(1);
  const [chartType, setChartTypeState] = useState<ChartType>("candles");

  // Read at mount-effect time without making these its reactive deps --
  // that effect must still only re-run on props.symbol (see its own
  // comment below), not on every settings change (which already has its
  // own dedicated effects for the *already-mounted* instance).
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const autoScaleRef = useRef(autoScale);
  autoScaleRef.current = autoScale;
  const scaleModeRef = useRef(scaleMode);
  scaleModeRef.current = scaleMode;
  const fitIndicatorsRef = useRef(fitIndicators);
  fitIndicatorsRef.current = fitIndicators;
  const chartTypeRef = useRef(chartType);
  chartTypeRef.current = chartType;

  // "30s" bypasses resample() entirely -- that function can only ever
  // COARSEN already-1-minute-granular data (confirmed: it has nothing
  // sub-minute to resample from), so the live-only subMinuteBars array
  // is used directly instead of being derived from props.bars.
  const displayBars = useMemo(
    () => (timeframe === "30s" ? props.subMinuteBars : resample(props.bars, timeframe)),
    [props.bars, props.subMinuteBars, timeframe],
  );
  // Tooltip lookup needs the currently DISPLAYED (possibly resampled)
  // bars, not the raw props.bars barsRef already tracks for getBaseOpen
  // -- param.time from the crosshair matches whatever's actually
  // plotted.
  const displayBarsRef = useRef<CandleBar[]>(displayBars);
  displayBarsRef.current = displayBars;

  // Mount fresh on every symbol change — same model as the prototype's
  // own per-tab instances, one mountSuperChart() call per chart identity,
  // not a single instance whose data gets destructively swapped across
  // unrelated symbols. New bars for the *same* symbol (ticks arriving,
  // timeframe pill changes) go through api.setBars() below instead.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    // Height explicitly passed rather than left to the scanner preset's
    // own fixed 380 -- this chart now lives in a real grid layout whose
    // cell height varies by viewport (see stockspotter-ui-target-layout
    // memory), not a fixed-height page section, so it needs to actually
    // fill whatever space CSS gives it rather than a constant.
    const initialBars = timeframe === "30s" ? subMinuteBarsRef.current : resample(barsRef.current, timeframe);
    // mountSuperChart's own internals index into bars[0]/bars[length-1]
    // unconditionally (real crash confirmed by reading superChartEngine.ts
    // before shipping this) -- an empty array is a real, expected state
    // for "30s" right when a symbol is first opened on that timeframe (no
    // sub-minute history exists at all, see subMinuteBarsRef's own
    // comment), not just for the pre-existing "no bars yet" case. Wait
    // for the first real bar rather than mounting with nothing.
    if (initialBars.length === 0) return;
    const api = mountSuperChart(container, "scanner", { bars: initialBars, height: container.clientHeight || undefined });
    apiRef.current = api;
    const unwireTooltip = wireChartTooltip(api, container, () => displayBarsRef.current, () => barsRef.current[0]?.open ?? 0);

    // Re-apply whatever settings were already chosen to this freshly-
    // mounted chart instance -- changed 2026-09-03 per Roman's explicit
    // ask ("the selection should be persistent... state should not
    // reset when changing from one stock to another"). This used to
    // reset every setting to its default here instead; that's gone now.
    // Real subtlety this can't skip: mountSuperChart() always starts a
    // brand-new chart engine at ITS OWN internal defaults (new series
    // objects, default visibility/autoScale/mode) regardless of what
    // React state says -- the autoScale/scaleMode/fitIndicators effects
    // further down only re-run when THEIR OWN dependency changes value,
    // which it won't across a symbol switch if it's already non-
    // default. So the current (persisted) values have to be pushed onto
    // this new `api` explicitly, right here, not left to those other
    // effects to eventually pick up.
    const v = visibleRef.current;
    api.series.ma9?.applyOptions({ visible: v.ma9 });
    api.series.ma20?.applyOptions({ visible: v.ma20 });
    api.series.vwap?.applyOptions({ visible: v.vwap });
    api.series.macdHist?.applyOptions({ visible: v.macd });
    api.series.macdLine?.applyOptions({ visible: v.macd });
    api.series.macdSignal?.applyOptions({ visible: v.macd });
    api.series.rsi?.applyOptions({ visible: v.rsi });
    api.series.bbUpper?.applyOptions({ visible: v.bollinger });
    api.series.bbLower?.applyOptions({ visible: v.bollinger });
    const mode = scaleModeRef.current === "log" ? PriceScaleMode.Logarithmic : scaleModeRef.current === "percent" ? PriceScaleMode.Percentage : PriceScaleMode.Normal;
    api.chart.priceScale("right").applyOptions({ autoScale: autoScaleRef.current, mode });
    for (const key of ["ma9", "ma20", "vwap", "bbUpper", "bbLower"] as const) {
      api.series[key]?.applyOptions({
        autoscaleInfoProvider: (original: () => unknown) => (fitIndicatorsRef.current ? original() : null),
      });
    }
    if (chartTypeRef.current !== "candles") api.setChartType(chartTypeRef.current);

    return () => {
      unwireTooltip();
      api.destroy();
      api.chart.remove();
      apiRef.current = null;
    };
    // Also re-runs the FALSE->TRUE transition of "on 30s with real data
    // now available" -- covers the real case above where the initial
    // mount was skipped because subMinuteBars started empty; once the
    // first sub-minute bar actually arrives this re-fires once (the
    // boolean only flips once) to mount the chart that was waiting on
    // it. Does NOT retrigger per-bar once already true/mounted.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.symbol, timeframe === "30s" && props.subMinuteBars.length > 0]);

  // New bars for the already-mounted instance (live ticks, or a
  // timeframe pill switching which resampled series is shown).
  useEffect(() => {
    apiRef.current?.setBars(displayBars);
  }, [displayBars]);

  useEffect(() => {
    apiRef.current?.chart.priceScale("right").applyOptions({ autoScale });
  }, [autoScale]);

  useEffect(() => {
    const mode = scaleMode === "log" ? PriceScaleMode.Logarithmic : scaleMode === "percent" ? PriceScaleMode.Percentage : PriceScaleMode.Normal;
    apiRef.current?.chart.priceScale("right").applyOptions({ mode });
  }, [scaleMode]);

  useEffect(() => {
    // "Fit all indicators" — ported verbatim from the prototype's own
    // applyFitIndicators(): autoscaleInfoProvider is a series *option*
    // (applyOptions), not a method — series.setAutoscaleInfoProvider()
    // isn't real on v4.1.3's API and throws, a real bug the prototype's
    // own history already found and fixed.
    const api = apiRef.current;
    if (!api) return;
    for (const key of ["ma9", "ma20", "vwap", "bbUpper", "bbLower"] as const) {
      const s = api.series[key];
      s?.applyOptions({
        autoscaleInfoProvider: (original: () => unknown) => (fitIndicators ? original() : null),
      });
    }
    if (autoScale) api.chart.priceScale("right").applyOptions({ autoScale: true });
  }, [fitIndicators, autoScale]);

  useEffect(() => {
    function onFullscreenChange() {
      setIsFullscreen(document.fullscreenElement === panelRef.current);
    }
    document.addEventListener("fullscreenchange", onFullscreenChange);
    return () => document.removeEventListener("fullscreenchange", onFullscreenChange);
  }, []);

  // Keep the screen awake while a real chart is actually showing --
  // 2026-09-03, ported from the same real ask on mobile (see
  // useWakeLock.ts's own doc comment).
  useWakeLock(props.bars.length > 0);

  function toggleFullscreen() {
    if (document.fullscreenElement) {
      document.exitFullscreen();
    } else {
      panelRef.current?.requestFullscreen();
    }
  }

  // Indicators popover toggle — ported verbatim from the prototype's own
  // click handler body (see its "if (key === 'macd')" branch), calling
  // directly into the engine's returned api.
  function toggleIndicator(key: IndicatorKey, nowOn: boolean) {
    const api = apiRef.current;
    if (!api) return;
    setVisible((prev) => ({ ...prev, [key]: nowOn }));

    if (key === "macd") {
      api.series.macdHist?.applyOptions({ visible: nowOn });
      api.series.macdLine?.applyOptions({ visible: nowOn });
      api.series.macdSignal?.applyOptions({ visible: nowOn });
    } else if (key === "bollinger") {
      api.series.bbUpper?.applyOptions({ visible: nowOn });
      api.series.bbLower?.applyOptions({ visible: nowOn });
    } else {
      api.series[key]?.applyOptions({ visible: nowOn });
    }
  }

  function setChartType(type: ChartType) {
    setChartTypeState(type);
    apiRef.current?.setChartType(type);
  }

  if (props.bars.length === 0) {
    return (
      <div className="super-chart-empty" style={{ height: 420 }}>
        Waiting for bars for {props.symbol}…
      </div>
    );
  }

  // Header price/change — from the full raw bar history, not whatever
  // timeframe pill is selected, so it doesn't jump around when switching
  // timeframes (matches the prototype's own convention).
  const firstBar = props.bars[0];
  const lastBar = props.bars[props.bars.length - 1];
  const headerPrice = lastBar.close;
  const headerChangePct = firstBar.open !== 0 ? ((lastBar.close - firstBar.open) / firstBar.open) * 100 : 0;
  const headerUp = headerChangePct >= 0;

  return (
    <div className="super-chart-panel" ref={panelRef}>
      <div className="chart-header">
        <div className="ticker-head">
          <span className="ticker chart-ticker-symbol">{props.symbol}</span>
        </div>
        <div className="chart-header-spacer" />
        <span className="price chart-ticker-price">${headerPrice.toFixed(headerPrice < 1 ? 4 : 2)}</span>
        <span className={headerUp ? "pct-up" : "pct-down"}>
          {headerUp ? "▲" : "▼"} {headerUp ? "+" : ""}
          {headerChangePct.toFixed(1)}%
        </span>
      </div>

      <div className="chart-toolbar">
        <ToggleGroup
          type="single"
          size="sm"
          value={String(timeframe)}
          onValueChange={(v) => v && setTimeframe(v === "30s" ? "30s" : (Number(v) as Timeframe))}
        >
          {/* Real sub-minute (2026-09-03) -- live-only, no history below 1
              minute (confirmed live against Alpaca's own API), so it's
              deliberately first/most-granular in the row rather than
              implying it's just another resampled bucket like the rest. */}
          <ToggleGroupItem value="30s" title="Live only — no history below 1 minute">
            30s
          </ToggleGroupItem>
          {TIMEFRAMES.map((tf) => (
            <ToggleGroupItem key={tf} value={String(tf)}>
              {tf}m
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
        <Button variant="ghost" size="xs" disabled title="Needs daily-bar data — the live feed only sends 1-minute bars, not built yet">
          1D
        </Button>

        <div className="chart-toolbar-spacer" />

        {/* Consolidated 2026-09-03 (Roman's explicit ask, ported from the
            mobile redesign this session): one menu instead of separate
            Indicators/Settings popovers -- same content, sectioned, not
            re-derived. The disabled "Create alert" bolt is dropped
            entirely rather than folded in: it was always just an inert
            placeholder here (unlike mobile's now-real one), and Roman's
            own ask was specifically to remove that icon, not relocate a
            still-nonfunctional one into the new menu. */}
        <Popover>
          <PopoverTrigger asChild>
            <Button variant="outline" size="icon-sm" aria-label="Chart menu" title="Indicators & display settings">
              <ChartIcon name="sliders" />
            </Button>
          </PopoverTrigger>
          <PopoverContent align="end" className="chart-popover-content chart-settings-popover">
            <div className="chart-popover-title">Indicators</div>
            <IndicatorSwitch label="MA9" color={MA9_COLOR} checked={visible.ma9} onToggle={(v) => toggleIndicator("ma9", v)} />
            <IndicatorSwitch label="MA20" color={MA20_COLOR} checked={visible.ma20} onToggle={(v) => toggleIndicator("ma20", v)} />
            <IndicatorSwitch label="VWAP" color={VWAP_COLOR} checked={visible.vwap} onToggle={(v) => toggleIndicator("vwap", v)} />
            <IndicatorSwitch label="MACD" color={MACD_LINE_COLOR} checked={visible.macd} onToggle={(v) => toggleIndicator("macd", v)} />
            <IndicatorSwitch label="RSI" color={RSI_COLOR} checked={visible.rsi} onToggle={(v) => toggleIndicator("rsi", v)} />
            <IndicatorSwitch label="Bollinger Bands" color={BOLLINGER_COLOR} checked={visible.bollinger} onToggle={(v) => toggleIndicator("bollinger", v)} />
            <div className="chart-popover-divider" />
            {/* Line/Candlestick lives here, same placement as Robinhood's
                own chart-settings gear icon (their first option) --
                borrowed per Roman's explicit ask, not a prototype
                carryover. */}
            <div className="chart-popover-title">Chart type</div>
            <RadioGroup value={chartType} onValueChange={(v) => setChartType(v as ChartType)} className="chart-scale-radio-group">
              {CHART_TYPE_OPTIONS.map((opt) => (
                <ScaleRadio key={opt.value} value={opt.value} label={opt.label} />
              ))}
            </RadioGroup>
            <div className="chart-popover-divider" />
            <div className="chart-popover-title">Auto-scale</div>
            <SettingSwitch label="Auto-scale price axis" checked={autoScale} onToggle={setAutoScale} />
            <div className="chart-popover-divider" />
            <div className="chart-popover-title">Fit to chart</div>
            <SettingSwitch label="Fit all indicators" checked={fitIndicators} onToggle={setFitIndicators} />
            <div className="chart-popover-divider" />
            <div className="chart-popover-title">Scaling</div>
            <RadioGroup value={scaleMode} onValueChange={(v) => setScaleMode(v as ScaleMode)} className="chart-scale-radio-group">
              <ScaleRadio value="linear" label="Linear (Price)" />
              <ScaleRadio value="percent" label="Linear (Percentage)" />
              <ScaleRadio value="log" label="Logarithmic (Price)" />
            </RadioGroup>
          </PopoverContent>
        </Popover>

        <Button
          variant="outline"
          size="icon-sm"
          aria-label={isFullscreen ? "Exit full screen" : "Full screen"}
          title={isFullscreen ? "Exit full screen" : "Full screen"}
          onClick={toggleFullscreen}
        >
          <ChartIcon name={isFullscreen ? "collapse" : "expand"} />
        </Button>
      </div>

      <div className="super-chart-mount">
        {timeframe === "30s" && displayBars.length === 0 && (
          <div className="super-chart-submin-empty">Live — building 30s candles now, no history below 1 minute</div>
        )}
        <div ref={containerRef} className="super-chart" />
      </div>

      <MomentumScoreRow symbol={props.symbol} momentum={props.momentum} bars={props.bars} />
    </div>
  );
}

function IndicatorSwitch(props: { label: string; color: string; checked: boolean; onToggle: (checked: boolean) => void }) {
  return (
    <label className="chart-switch-row">
      <span className="swatch" style={{ background: props.color }} />
      <span>{props.label}</span>
      <Switch size="sm" checked={props.checked} onCheckedChange={props.onToggle} className="chart-switch-row-control" />
    </label>
  );
}

function SettingSwitch(props: { label: string; checked: boolean; onToggle: (checked: boolean) => void }) {
  return (
    <label className="chart-switch-row">
      <span>{props.label}</span>
      <Switch size="sm" checked={props.checked} onCheckedChange={props.onToggle} className="chart-switch-row-control" />
    </label>
  );
}

function ScaleRadio(props: { value: string; label: string }) {
  return (
    <label className="chart-radio-row">
      <RadioGroupItem value={props.value} />
      {props.label}
    </label>
  );
}

/**
 * Real momentum_scorer::MomentumScore data (the MomentumUpdate ScanEvent),
 * NOT the prototype's version -- that was static demo copy ("84 / Strong
 * Bullish", "Structure intact since 9:41" were never computed from a
 * formula, confirmed by reading its source). Detail lines read like the
 * prototype's own sentences, but are real, computed from actual bar data
 * (momentumNarrative.ts) -- not copied placeholder text. See that file's
 * header comment for which lines are direct data restatements (volume,
 * MA slope) vs. grounded in the real backend score rather than an
 * independent client-side re-detection (structure, wick rejection).
 */
function MomentumScoreRow(props: { symbol: string; momentum: MomentumUpdate | null; bars: CandleBar[] }) {
  const m = props.momentum;
  const { assessment, loading, error, regenerate } = useAssessment(props.symbol, m);

  if (!m) {
    return (
      <div className="score-row">
        <div className="score-empty">No momentum reading yet for this symbol…</div>
      </div>
    );
  }
  const scoreValue = Math.round(m.overall * 100);
  const scoreColor = m.overall >= 0.6 ? "#0ca30c" : m.overall >= 0.4 ? "#fab219" : "#d03b3b";
  const bars = props.bars;
  const ma9Vals = sma(bars, 9).map((p) => p.value);
  const ma20Vals = sma(bars, 20).map((p) => p.value);
  const lastPrice = bars[bars.length - 1]?.close ?? 0;
  return (
    <div className="score-row">
      <div className="score-main">
        <div
          className="score-badge"
          title="Composite of volume confirmation, HH/HL structure, MA slope and wick rejection — weighted toward volume, the strongest signal"
        >
          <div className="score-value" style={{ color: scoreColor }}>
            {scoreValue}
          </div>
          <div className="score-caption">{momentumLabel(m.overall)}</div>
          <div className="score-sub">Momentum score</div>
        </div>
        <div className="factors-list">
          <FactorRow label="Volume confirmation" score={m.volumeConfirmation} detail={volumeConfirmationDetail(bars)} />
          <FactorRow label="Higher highs / higher lows" score={m.structure} detail={structureDetail(m.structure)} />
          <FactorRow label="MA slope" score={m.maSlope} detail={maSlopeDetail(ma9Vals, ma20Vals, lastPrice, factorGood(m.maSlope))} />
          <FactorRow label="Rejection wicks" score={m.wickRejection} detail={wickRejectionDetail(m.wickRejection)} />
        </div>
      </div>
      {/* AI assessment (2026-09-03) -- a real Claude call (with web
          search), fetched once per symbol selection and cached
          server-side, not on every bar tick (see useAssessment.ts's own
          doc comment). Kept inside this same card per Roman's own
          suggestion, not a separate panel. */}
      <div className="score-ai">
        <div className="score-ai-header">
          <span className="score-ai-label">AI read</span>
          <button className="score-ai-refresh" onClick={regenerate} disabled={loading} aria-label="Regenerate AI assessment">
            {loading ? "…" : "↻ Refresh"}
          </button>
        </div>
        {loading && !assessment && <div className="score-ai-status">Reading the tape…</div>}
        {error && !assessment && <div className="score-ai-status">Couldn't reach the assessment service.</div>}
        {assessment && (
          <ul className="score-ai-bullets">
            {assessment.summary.map((line, i) => (
              <li key={i}>{line}</li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function FactorRow(props: { label: string; score: number; detail: string }) {
  const good = factorGood(props.score);
  return (
    <div className={`factor-row ${good ? "factor-row-good" : "factor-row-warning"}`}>
      <span className="factor-row-icon">{good ? "✓" : "!"}</span>
      <div className="factor-row-body">
        <div className="factor-row-title">{props.label}</div>
        <div className="factor-row-detail">{props.detail}</div>
      </div>
    </div>
  );
}
