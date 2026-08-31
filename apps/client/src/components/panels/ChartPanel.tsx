// Super Chart panel: real live candlestick+volume+indicators data for one
// tracked symbol at a time, picked from a plain <select> for now — not
// yet the target "Single or Multiview Panel" layout from
// stockspotter-ui-target-layout. That's real layout work still to come,
// done with Roman looking at it (SuperChart.tsx's own doc comment tracks
// what's ported vs. still deferred within the chart itself).

import { useMemo, useState } from "react";
import type { BarUpdate, MomentumUpdate } from "@stockspotter/shared-types";
import { listChartableSymbols, mergeBars, toChartBars } from "../../lib/derive";
import { useHistoricalBackfill } from "../../lib/useHistoricalBackfill";
import { EmptyState, PanelShell } from "../PanelShell";
import { SuperChart } from "../SuperChart";

export function ChartPanel(props: {
  barsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
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
    <PanelShell title="Super Chart" subtitle="live bars, real Alpaca data">
      {symbols.length === 0 ? (
        <EmptyState>No bars yet — waiting for a symbol to start tracking…</EmptyState>
      ) : (
        <>
          <div className="chart-symbol-picker">
            <select value={selected ?? ""} onChange={(e) => setUserSelected(e.target.value)}>
              {symbols.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </div>
          {selected && <SuperChart symbol={selected} bars={bars} momentum={momentum} />}
        </>
      )}
    </PanelShell>
  );
}
