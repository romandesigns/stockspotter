// Super Chart panel: real live candlestick+volume+indicators data for one
// tracked symbol at a time, picked via a real shadcn Select. This is the
// "Single or Multiview Panel" slot in stockspotter-ui-target-layout
// (App.tsx positions it there) -- "single" was the only mode built until
// 2026-09-03, when Roman asked for real multi-view: 2-4 independent
// panels, each its own symbol AND its own timeframe (down to a real
// live-only 30s granularity, see SuperChart.tsx).
//
// Selection for the FIRST slot stays controlled from App.tsx
// (selectedSymbol/onSelectedSymbolChange) exactly as before -- so a
// CatalystBadge click in any other panel still drives what that slot
// shows. Extra slots (2nd/3rd/4th, when multi-view is active) are
// deliberately NOT lifted to App.tsx -- there's only ever one
// app-wide "externally requested" symbol concept, and forcing every
// multi-view slot through it would mean a catalyst click anywhere
// unpredictably reassigns whichever slot instead of the one Roman
// actually has in mind. Each extra slot owns its own local symbol
// state instead, same as picking a genuinely independent chart.

import { memo, useCallback, useEffect, useMemo, useState } from "react";
import type { BarUpdate, CatalystUpdate, MomentumUpdate, ServerFeedStatus } from "@stockspotter/shared-types";
import type { FeedGap } from "../../lib/feedHealth";
import type { ConnectionStatus } from "../../lib/useRealtimeFeed";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { CatalystBadge } from "../CatalystBadge";
import { listChartableSymbols, mergeHistoryAndLive, toChartBars } from "../../lib/derive";
import { useHistoricalBackfill } from "../../lib/useHistoricalBackfill";
import { registerChartSymbol } from "../../lib/chartSubscription";
import { EmptyState, PanelShell } from "../PanelShell";
import { SuperChart } from "../SuperChart";
import { OrderFlowStrip } from "../OrderFlowStrip";
import { orderFlowEnabled, useOrderFlow } from "../../lib/useOrderFlow";
import type { ChartReferenceCache, ChartReferenceQuote } from "../../lib/chartChange";

type PanelCount = 1 | 2 | 3 | 4;
const PANEL_COUNT_OPTIONS: PanelCount[] = [1, 2, 3, 4];
const NO_SYMBOLS: string[] = [];

function ChartPanelImpl(props: {
  barsBySymbol: Map<string, BarUpdate[]>;
  /** Sorted symbols with 1-minute bars (useRealtimeFeed's chartSymbols),
   * stable until a new symbol appears. Optional only for callers without
   * the feed (the chart lifecycle harness); the app always passes it. */
  chartSymbols?: string[];
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  selectedSymbol: string | null;
  onSelectedSymbolChange: (symbol: string) => void;
  status: ConnectionStatus;
  feedGap: FeedGap | null;
  resyncNonce: number;
  /** The server's upstream market-data state (`feed_status`), or null
   * from a server that does not send it. */
  upstream?: ServerFeedStatus | null;
  referenceQuotesBySymbol?: Map<string, ChartReferenceQuote[]>;
  referenceCache?: ChartReferenceCache;
  className?: string;
}) {
  // Not derived here: sorting the whole key list on every bar was its own
  // per-message cost. The feed keeps it, changing only when a symbol is
  // added (feedBatch.ts).
  const derivedSymbols = useMemo(
    () => (props.chartSymbols ? null : listChartableSymbols(props.barsBySymbol)),
    [props.chartSymbols, props.barsBySymbol],
  );
  const symbols = props.chartSymbols ?? derivedSymbols ?? NO_SYMBOLS;
  const selected = props.selectedSymbol ?? symbols[0] ?? null;

  const [panelCount, setPanelCount] = useState<PanelCount>(1);
  // Local, independent per-extra-slot symbol state -- see the module doc
  // comment on why these deliberately aren't lifted to App.tsx the way
  // the first slot's selection is. Defaults picked so switching into
  // multi-view for the first time isn't 3 empty pickers -- falls back to
  // whatever's next in the chartable-symbols list, still just a starting
  // point the user can freely change.
  const [extraSymbols, setExtraSymbols] = useState<[string | null, string | null, string | null]>([null, null, null]);

  const slots = useMemo(() => {
    const rest: string[] = [];
    for (const s of symbols) {
      if (s !== selected && !extraSymbols.includes(s) && rest.length < 3) rest.push(s);
    }
    const out: string[] = [selected ?? ""];
    for (let i = 0; i < panelCount - 1; i++) {
      out.push(extraSymbols[i] ?? rest[i] ?? selected ?? "");
    }
    return out;
  }, [selected, extraSymbols, symbols, panelCount]);

  // Stable, so the memoised slots below do not re-render for a new
  // callback identity on every feed batch.
  const onSelectedSymbolChange = props.onSelectedSymbolChange;
  const setSlotSymbol = useCallback((index: number, symbol: string) => {
    if (index === 0) {
      onSelectedSymbolChange(symbol);
    } else {
      setExtraSymbols((prev) => {
        const next: [string | null, string | null, string | null] = [...prev];
        next[index - 1] = symbol;
        return next;
      });
    }
  }, [onSelectedSymbolChange]);
  // Each slot gets only ITS symbol's data, not the maps, so a bar for an
  // unrelated symbol does not re-render it.
  const slotProps = (sym: string, index: number) => ({
    index,
    symbol: sym,
    symbols,
    onSelectSlot: setSlotSymbol,
    liveSeries: sym ? props.barsBySymbol.get(sym) : undefined,
    subMinuteSeries: sym ? props.subMinuteBarsBySymbol.get(sym) : undefined,
    momentum: sym ? (props.momentumBySymbol.get(sym) ?? null) : null,
    catalystsBySymbol: props.catalystsBySymbol,
    status: props.status,
    feedGap: props.feedGap,
    resyncNonce: props.resyncNonce,
    upstream: props.upstream ?? null,
    referenceQuotes: sym ? props.referenceQuotesBySymbol?.get(sym) : undefined,
    referenceCache: props.referenceCache,
  });

  return (
    <PanelShell className={props.className} scrollable={false}>
      {symbols.length === 0 && !selected ? (
        <EmptyState>No bars yet — waiting for a symbol to start tracking…</EmptyState>
      ) : (
        <>
          {/* Panel-count picker lives here, not PanelShell's header --
              ChartPanel deliberately has no `title` (each slot's own
              symbol/price header already serves that role), and
              PanelShell only renders headerExtra alongside a real
              title. */}
          <div className="chart-multiview-picker">
            <ToggleGroup type="single" size="sm" value={String(panelCount)} onValueChange={(v) => v && setPanelCount(Number(v) as PanelCount)}>
              {PANEL_COUNT_OPTIONS.map((n) => (
                <ToggleGroupItem key={n} value={String(n)} title={n === 1 ? "Single view" : `${n}-panel multi-view`}>
                  {n}
                </ToggleGroupItem>
              ))}
            </ToggleGroup>
          </div>
          {panelCount === 1 ? (
            <ChartSlot {...slotProps(slots[0], 0)} />
          ) : (
            <div className="chart-multiview-grid" style={{ gridTemplateColumns: `repeat(${panelCount}, minmax(0, 1fr))` }}>
              {slots.map((sym, i) => (
                <ChartSlot key={i} {...slotProps(sym, i)} compact />
              ))}
            </div>
          )}
        </>
      )}
    </PanelShell>
  );
}

/**
 * One independent chart instance -- symbol picker + CatalystBadge +
 * SuperChart, computing its own bars/subMinuteBars/momentum exactly the
 * way the single-view body always has. Reused for every slot, single or
 * multi-view, so there's exactly one real implementation of "what a
 * chart slot is" rather than a duplicated single-view case plus a
 * separate multi-view one. `toChartBars`/`mergeHistoryAndLive`/
 * `useHistoricalBackfill` are all confirmed pure/self-contained --
 * calling them once per slot (up to 4 times) is safe, no shared state to
 * corrupt (each is its own independent, uncoordinated fetch/computation).
 */
function ChartSlotImpl(props: {
  index: number;
  symbol: string;
  symbols: string[];
  onSelectSlot: (index: number, symbol: string) => void;
  /** This slot's symbol's live series only -- see ChartPanel's slotProps. */
  liveSeries: BarUpdate[] | undefined;
  subMinuteSeries: BarUpdate[] | undefined;
  momentum: MomentumUpdate | null;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  status: ConnectionStatus;
  feedGap: FeedGap | null;
  resyncNonce: number;
  upstream: ServerFeedStatus | null;
  referenceQuotes?: ChartReferenceQuote[];
  referenceCache?: ChartReferenceCache;
  compact?: boolean;
}) {
  const selected = props.symbol || null;
  const { index, onSelectSlot } = props;
  const onSelectSymbol = useCallback((symbol: string) => onSelectSlot(index, symbol), [index, onSelectSlot]);
  // Ask the server to build live candles for this symbol while the slot
  // shows it (lib/chartSubscription.ts). Released on symbol change and
  // unmount; the registry coalesces a switch into one message.
  useEffect(() => (selected ? registerChartSymbol(selected) : undefined), [selected]);

  // The per-symbol arrays keep their identity unless that symbol's own
  // series changed (feedBatch.ts), so these memos recompute exactly when
  // the displayed data changed, not when any tracked symbol printed. Audit
  // §3: "Unrelated symbol updates can cause additional chart derivation."
  const liveSeries = props.liveSeries;
  const backfill = useHistoricalBackfill(selected, props.resyncNonce, (liveSeries?.length ?? 0) > 0);
  const bars = useMemo(() => mergeHistoryAndLive(backfill.bars, liveSeries ?? []), [backfill.bars, liveSeries]);
  // Every recorded gap has been re-fetched over for this symbol, so its
  // 1-minute series is whole again. The 30-second view is never repaired
  // this way (no backfill exists below 1 minute); SuperChart keeps its
  // gap flag there.
  const minuteGapRepaired = props.resyncNonce > 0 && backfill.repairedNonce >= props.resyncNonce;
  // No historical merge for sub-minute -- there's nothing to merge with
  // (no backfill exists below 1 minute, see SuperChart.tsx's own doc
  // comment on the real Alpaca API constraint confirmed live).
  const subMinuteSeries = props.subMinuteSeries;
  const subMinuteBars = useMemo(() => toChartBars(subMinuteSeries ?? []), [subMinuteSeries]);
  const momentum = props.momentum;

  // Observational order-flow research, opt-in and off by default: without
  // VITE_ORDER_FLOW this hook subscribes to nothing and makes no request,
  // and the strip is not rendered at all. See lib/useOrderFlow.ts.
  const showOrderFlow = orderFlowEnabled();
  const orderFlow = useOrderFlow(showOrderFlow ? selected : null);
  // Which summary window the strip describes. Owned here because
  // SuperChart owns the timeframe pill; it only notifies.
  const [bucketMinutes, setBucketMinutes] = useState(1);
  // Multi-view slots are short, so the strip starts collapsed there and
  // the header alone carries the state. Null until the user decides, so
  // switching into or out of multi-view follows the slot's own height
  // rather than keeping whatever the first render happened to pick.
  const [flowCollapsed, setFlowCollapsed] = useState<boolean | null>(null);
  const collapsed = flowCollapsed ?? Boolean(props.compact);

  return (
    <div className={props.compact ? "chart-slot chart-slot-compact" : "chart-slot"}>
      <div className="chart-symbol-picker">
        <Select value={selected ?? undefined} onValueChange={onSelectSymbol}>
          <SelectTrigger size="sm" className="chart-symbol-select-trigger">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {props.symbols.map((s) => (
              <SelectItem key={s} value={s}>
                {s}
              </SelectItem>
            ))}
            {selected && !props.symbols.includes(selected) && <SelectItem value={selected}>{selected}</SelectItem>}
          </SelectContent>
        </Select>
        {selected && <CatalystBadge symbol={selected} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={onSelectSymbol} />}
      </div>
      {selected && (
        <SuperChart
          symbol={selected}
          bars={bars}
          subMinuteBars={subMinuteBars}
          momentum={momentum}
          referenceQuotes={props.referenceQuotes}
          referenceCache={props.referenceCache}
          status={props.status}
          feedGap={props.feedGap}
          minuteGapRepaired={minuteGapRepaired}
          upstream={props.upstream}
          onTimeframeChange={showOrderFlow ? setBucketMinutes : undefined}
        />
      )}
      {selected && showOrderFlow && (
        <OrderFlowStrip state={orderFlow} chartBucketMinutes={bucketMinutes} collapsed={collapsed} onToggleCollapsed={() => setFlowCollapsed(!collapsed)} />
      )}
    </div>
  );
}

/** Memoised: ChartPanel re-renders on every feed batch that touches bars
 * for any symbol, and each slot's props are per-symbol (see slotProps), so
 * a slot whose own symbol did not change skips rendering entirely. */
const ChartSlot = memo(ChartSlotImpl);

/** Memoised so a batch that carries no bars (most of them, in the regular
 * session: halt warnings and ignition events) does not re-render the chart
 * panel at all. Every prop is either stable or changes only with data the
 * chart shows. */
export const ChartPanel = memo(ChartPanelImpl);
