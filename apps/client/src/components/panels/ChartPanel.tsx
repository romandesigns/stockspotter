// Super Chart panel: real live candlestick+volume+indicators data for one
// tracked symbol at a time, picked via a real shadcn Select now (was a
// plain native <select>). This is the "Single or Multiview Panel" slot in
// stockspotter-ui-target-layout (App.tsx positions it there) -- "single"
// is real (this component), "multiview" (several charts at once) isn't
// built yet.

import { useMemo, useState } from "react";
import type { BarUpdate, MomentumUpdate } from "@stockspotter/shared-types";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { listChartableSymbols, mergeBars, toChartBars } from "../../lib/derive";
import { useHistoricalBackfill } from "../../lib/useHistoricalBackfill";
import { EmptyState, PanelShell } from "../PanelShell";
import { SuperChart } from "../SuperChart";

export function ChartPanel(props: {
  barsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  className?: string;
}) {
  const symbols = listChartableSymbols(props.barsBySymbol);
  // Only ever set explicitly, from the picker's onChange — the "auto-pick
  // a symbol once bars start arriving" behavior is derived below instead
  // of synced via an effect, so there's no extra render/setState round
  // trip just to reflect a value computable from this render's own props.
  const [userSelected, setUserSelected] = useState<string | null>(null);
  const selected = userSelected && symbols.includes(userSelected) ? userSelected : (symbols[0] ?? null);

  const liveBars = useMemo(() => (selected ? toChartBars(props.barsBySymbol.get(selected) ?? []) : []), [selected, props.barsBySymbol]);
  const historicalBars = useHistoricalBackfill(selected);
  const bars = useMemo(() => mergeBars(historicalBars, liveBars), [historicalBars, liveBars]);
  const momentum = selected ? (props.momentumBySymbol.get(selected) ?? null) : null;

  return (
    <PanelShell className={props.className} scrollable={false}>
      {symbols.length === 0 ? (
        <EmptyState>No bars yet — waiting for a symbol to start tracking…</EmptyState>
      ) : (
        <>
          <div className="chart-symbol-picker">
            <Select value={selected ?? undefined} onValueChange={setUserSelected}>
              <SelectTrigger size="sm" className="chart-symbol-select-trigger">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {symbols.map((s) => (
                  <SelectItem key={s} value={s}>
                    {s}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {selected && <SuperChart symbol={selected} bars={bars} momentum={momentum} />}
        </>
      )}
    </PanelShell>
  );
}
