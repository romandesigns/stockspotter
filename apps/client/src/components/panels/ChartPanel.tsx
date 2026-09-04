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

import { useMemo, useState } from "react";
import type { BarUpdate, CatalystUpdate, MomentumUpdate } from "@stockspotter/shared-types";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { CatalystBadge } from "../CatalystBadge";
import { listChartableSymbols, mergeBars, toChartBars } from "../../lib/derive";
import { useHistoricalBackfill } from "../../lib/useHistoricalBackfill";
import { EmptyState, PanelShell } from "../PanelShell";
import { SuperChart } from "../SuperChart";

type PanelCount = 1 | 2 | 3 | 4;
const PANEL_COUNT_OPTIONS: PanelCount[] = [1, 2, 3, 4];

export function ChartPanel(props: {
  barsBySymbol: Map<string, BarUpdate[]>;
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  selectedSymbol: string | null;
  onSelectedSymbolChange: (symbol: string) => void;
  className?: string;
}) {
  const symbols = listChartableSymbols(props.barsBySymbol);
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

  function setSlotSymbol(index: number, symbol: string) {
    if (index === 0) {
      props.onSelectedSymbolChange(symbol);
    } else {
      setExtraSymbols((prev) => {
        const next: [string | null, string | null, string | null] = [...prev];
        next[index - 1] = symbol;
        return next;
      });
    }
  }

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
            <ChartSlot
              symbol={slots[0]}
              symbols={symbols}
              onSelectSymbol={(s) => setSlotSymbol(0, s)}
              barsBySymbol={props.barsBySymbol}
              subMinuteBarsBySymbol={props.subMinuteBarsBySymbol}
              momentumBySymbol={props.momentumBySymbol}
              catalystsBySymbol={props.catalystsBySymbol}
            />
          ) : (
            <div className="chart-multiview-grid" style={{ gridTemplateColumns: `repeat(${panelCount}, minmax(0, 1fr))` }}>
              {slots.map((sym, i) => (
                <ChartSlot
                  key={i}
                  symbol={sym}
                  symbols={symbols}
                  onSelectSymbol={(s) => setSlotSymbol(i, s)}
                  barsBySymbol={props.barsBySymbol}
                  subMinuteBarsBySymbol={props.subMinuteBarsBySymbol}
                  momentumBySymbol={props.momentumBySymbol}
                  catalystsBySymbol={props.catalystsBySymbol}
                  compact
                />
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
 * separate multi-view one. `toChartBars`/`mergeBars`/
 * `useHistoricalBackfill` are all confirmed pure/self-contained --
 * calling them once per slot (up to 4 times) is safe, no shared state to
 * corrupt (each is its own independent, uncoordinated fetch/computation).
 */
function ChartSlot(props: {
  symbol: string;
  symbols: string[];
  onSelectSymbol: (symbol: string) => void;
  barsBySymbol: Map<string, BarUpdate[]>;
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  compact?: boolean;
}) {
  const selected = props.symbol || null;
  const liveBars = useMemo(() => (selected ? toChartBars(props.barsBySymbol.get(selected) ?? []) : []), [selected, props.barsBySymbol]);
  const historicalBars = useHistoricalBackfill(selected);
  const bars = useMemo(() => mergeBars(historicalBars, liveBars), [historicalBars, liveBars]);
  // No historical merge for sub-minute -- there's nothing to merge with
  // (no backfill exists below 1 minute, see SuperChart.tsx's own doc
  // comment on the real Alpaca API constraint confirmed live).
  const subMinuteBars = useMemo(() => (selected ? toChartBars(props.subMinuteBarsBySymbol.get(selected) ?? []) : []), [selected, props.subMinuteBarsBySymbol]);
  const momentum = selected ? (props.momentumBySymbol.get(selected) ?? null) : null;

  return (
    <div className={props.compact ? "chart-slot chart-slot-compact" : "chart-slot"}>
      <div className="chart-symbol-picker">
        <Select value={selected ?? undefined} onValueChange={props.onSelectSymbol}>
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
        {selected && <CatalystBadge symbol={selected} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />}
      </div>
      {selected && <SuperChart symbol={selected} bars={bars} subMinuteBars={subMinuteBars} momentum={momentum} />}
    </div>
  );
}
