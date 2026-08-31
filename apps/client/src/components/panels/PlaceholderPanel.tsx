// Honest "not built yet" panel for the target-layout slots that have no
// backend data source at all (stockspotter-ui-target-layout memory:
// Catalysts, Top Gainers, Highly Trading, Markets Today). Shown in the
// correct grid position/size so the overall dashboard shape matches the
// reference even before these are wired up -- not faked data, not
// silently omitted (which would visually break the layout proportions).

import { EmptyState, PanelShell } from "../PanelShell";

export function PlaceholderPanel(props: { title: string; note: string; className?: string }) {
  return (
    <PanelShell title={props.title} className={props.className}>
      <EmptyState>{props.note}</EmptyState>
    </PanelShell>
  );
}
