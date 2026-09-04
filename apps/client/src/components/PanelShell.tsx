import type { ReactNode } from "react";
import { ScrollArea } from "@/components/ui/scroll-area";

export function PanelShell(props: {
  /** Omit entirely to skip the header row -- for a panel (like Super
   * Chart) whose content already renders its own, more specific header
   * right below, where a generic title above it would just be a
   * redundant duplicate label. */
  title?: string;
  subtitle?: string;
  count?: number;
  /** Extra control rendered in the header, to the left of the count (e.g.
   * Top Gainers' date toggle) -- for a per-panel interactive control that
   * belongs in the header row, not the body. */
  headerExtra?: ReactNode;
  /** Extra class(es) applied directly to the `.panel` element itself.
   * Panel POSITION within the dashboard no longer comes from here (that
   * was CSS Grid's own mechanism, `.grid-*` classes in index.css) --
   * since 2026-09-04 the dashboard is a react-resizable-panels tree
   * (App.tsx) and position/size come from tree order + drag state
   * instead. `.panel` still needs `height: 100%` (index.css) to fill
   * whatever `<Panel>` wrapper hands it, same flex/min-height:0 chain as
   * before, just a flex item of a `PanelGroup` now instead of a CSS Grid
   * track. */
  className?: string;
  /** false for a panel whose content fills the available height itself
   * rather than scrolling within it (Super Chart -- its own internal
   * layout, resize handles, and drag math all assume a definite,
   * non-scrolling height). Radix's ScrollArea Viewport isn't a flex
   * container, so wrapping height-filling content in it would break the
   * .panel-body -> .super-chart-panel flex chain SuperChart.tsx depends
   * on. Defaults to true -- every list-style panel wants a real
   * scrollbar here, not the browser's native one. */
  scrollable?: boolean;
  children: ReactNode;
}) {
  const scrollable = props.scrollable ?? true;
  return (
    <section className={props.className ? `panel ${props.className}` : "panel"}>
      {props.title && (
        <header className="panel-header">
          <div>
            <h2>{props.title}</h2>
            {props.subtitle && <p className="panel-subtitle">{props.subtitle}</p>}
          </div>
          <div className="panel-header-actions">
            {props.headerExtra}
            {typeof props.count === "number" && <span className="panel-count">{props.count}</span>}
          </div>
        </header>
      )}
      {scrollable ? (
        <ScrollArea className="panel-body">{props.children}</ScrollArea>
      ) : (
        <div className="panel-body">{props.children}</div>
      )}
    </section>
  );
}

export function EmptyState(props: { children: ReactNode }) {
  return <div className="empty-state">{props.children}</div>;
}
