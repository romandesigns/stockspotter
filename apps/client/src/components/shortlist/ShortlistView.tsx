// The Shortlist workspace (App.tsx's default view): ranked list on the
// left, the decision card and the existing Super Chart for the symbol in
// focus on the right. Stacks vertically below the same 1400px breakpoint
// the Classic dashboard uses (pure CSS here -- unlike the resizable
// Classic tree, nothing about this layout needs a different React tree).
//
// Focus = the app's selectedSymbol, falling back to the #1 candidate when
// nothing is selected yet -- derived during render rather than written
// back into App state, so the list never fights the user's own pick. A
// symbol selected elsewhere (a toast, the watchlist) stays in focus even
// when it isn't on the list; the card says so instead of hiding it.

import type { CatalystUpdate, FunnelSignal } from "@stockspotter/shared-types";
import type { ReactNode } from "react";
import type { ShortlistState } from "../../lib/useShortlist";
import { DecisionCard } from "./DecisionCard";
import { ShortlistList } from "./ShortlistList";

export function ShortlistView(props: {
  shortlist: ShortlistState;
  selectedSymbol: string | null;
  onSelectSymbol: (symbol: string) => void;
  funnelBySymbol: Map<string, FunnelSignal>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  /** App's own ChartPanel, rendered for the symbol in focus. */
  renderChart: (symbol: string | null) => ReactNode;
}) {
  const candidates = props.shortlist.body?.candidates ?? [];
  const focusSymbol = props.selectedSymbol ?? candidates[0]?.symbol ?? null;
  const candidate = candidates.find((c) => c.symbol === focusSymbol) ?? null;

  return (
    <main className="shortlist-view">
      <ShortlistList
        shortlist={props.shortlist}
        selectedSymbol={focusSymbol}
        onSelectSymbol={props.onSelectSymbol}
        catalystsBySymbol={props.catalystsBySymbol}
        saved={props.saved}
        onToggleSaved={props.onToggleSaved}
      />
      <DecisionCard
        symbol={focusSymbol}
        candidate={candidate}
        generatedAt={props.shortlist.body?.generatedAt ?? null}
        liveFunnel={focusSymbol ? props.funnelBySymbol.get(focusSymbol) : undefined}
        liveCatalyst={focusSymbol ? props.catalystsBySymbol.get(focusSymbol) : undefined}
        onSelectSymbol={props.onSelectSymbol}
        saved={props.saved}
        onToggleSaved={props.onToggleSaved}
      />
      <div className="shortlist-chart">{props.renderChart(focusSymbol)}</div>
    </main>
  );
}
