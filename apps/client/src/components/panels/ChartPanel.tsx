// Super Chart panel: real live candlestick+volume data for one tracked
// symbol at a time, picked from a plain <select> for now — this is the
// plumbing pass (real data, real chart, real component), not the target
// "Single or Multiview Panel" layout from stockspotter-ui-target-layout.
// That's a real design pass still to come, done with Roman looking at it.

import { useState } from "react";
import type { BarUpdate } from "@stockspotter/shared-types";
import { listChartableSymbols, toChartBars } from "../../lib/derive";
import { EmptyState, PanelShell } from "../PanelShell";
import { SuperChart } from "../SuperChart";

export function ChartPanel(props: { barsBySymbol: Map<string, BarUpdate[]> }) {
  const symbols = listChartableSymbols(props.barsBySymbol);
  // Only ever set explicitly, from the picker's onChange — the "auto-pick
  // a symbol once bars start arriving" behavior is derived below instead
  // of synced via an effect, so there's no extra render/setState round
  // trip just to reflect a value computable from this render's own props.
  const [userSelected, setUserSelected] = useState<string | null>(null);
  const selected = userSelected && symbols.includes(userSelected) ? userSelected : (symbols[0] ?? null);

  const bars = selected ? toChartBars(props.barsBySymbol.get(selected) ?? []) : [];

  return (
    <PanelShell title="Super Chart" subtitle="live 1-minute bars, real Alpaca data">
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
          {selected && <SuperChart symbol={selected} bars={bars} />}
        </>
      )}
    </PanelShell>
  );
}
