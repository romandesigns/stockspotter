// Super Chart panel: real live candlestick+volume+indicators data for one
// tracked symbol at a time, picked via a real shadcn Select now (was a
// plain native <select>). This is the "Single or Multiview Panel" slot in
// stockspotter-ui-target-layout (App.tsx positions it there) -- "single"
// is real (this component), "multiview" (several charts at once) isn't
// built yet.
//
// Selection is controlled from App.tsx (selectedSymbol/
// onSelectedSymbolChange), not local state -- so a CatalystBadge click in
// any other panel, or a CatalystsPanel row, can drive what shows here
// (Roman's "actionable, practical" catalyst request). A requested symbol
// doesn't have to be one with live bars yet: useHistoricalBackfill covers
// any real ticker, same resilience the Backtest Replay dialog's free-text
// symbol input already relies on.

import { useMemo } from "react";
import type { BarUpdate, CatalystUpdate, MomentumUpdate } from "@stockspotter/shared-types";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CatalystBadge } from "../CatalystBadge";
import { listChartableSymbols, mergeBars, toChartBars } from "../../lib/derive";
import { useHistoricalBackfill } from "../../lib/useHistoricalBackfill";
import { EmptyState, PanelShell } from "../PanelShell";
import { SuperChart } from "../SuperChart";

export function ChartPanel(props: {
  barsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  selectedSymbol: string | null;
  onSelectedSymbolChange: (symbol: string) => void;
  className?: string;
}) {
  const symbols = listChartableSymbols(props.barsBySymbol);
  // Honors an externally-requested symbol (a catalyst badge/row click)
  // even if it isn't one of the live-tracked `symbols` -- falls back to
  // the first chartable symbol only when nothing's been requested yet.
  const selected = props.selectedSymbol ?? symbols[0] ?? null;

  const liveBars = useMemo(() => (selected ? toChartBars(props.barsBySymbol.get(selected) ?? []) : []), [selected, props.barsBySymbol]);
  const historicalBars = useHistoricalBackfill(selected);
  const bars = useMemo(() => mergeBars(historicalBars, liveBars), [historicalBars, liveBars]);
  const momentum = selected ? (props.momentumBySymbol.get(selected) ?? null) : null;

  return (
    <PanelShell className={props.className} scrollable={false}>
      {symbols.length === 0 && !selected ? (
        <EmptyState>No bars yet — waiting for a symbol to start tracking…</EmptyState>
      ) : (
        <>
          <div className="chart-symbol-picker">
            <Select value={selected ?? undefined} onValueChange={props.onSelectedSymbolChange}>
              <SelectTrigger size="sm" className="chart-symbol-select-trigger">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {symbols.map((s) => (
                  <SelectItem key={s} value={s}>
                    {s}
                  </SelectItem>
                ))}
                {/* A catalyst-driven selection can point at a real symbol
                    that isn't in the live-tracked list -- still shown as
                    a selectable option so the trigger's own value stays
                    meaningful instead of silently mismatching. */}
                {selected && !symbols.includes(selected) && <SelectItem value={selected}>{selected}</SelectItem>}
              </SelectContent>
            </Select>
            {selected && <CatalystBadge symbol={selected} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectedSymbolChange} />}
          </div>
          {selected && <SuperChart symbol={selected} bars={bars} momentum={momentum} />}
        </>
      )}
    </PanelShell>
  );
}
