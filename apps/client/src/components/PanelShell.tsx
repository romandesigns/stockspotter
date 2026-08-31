import type { ReactNode } from "react";

export function PanelShell(props: {
  /** Omit entirely to skip the header row -- for a panel (like Super
   * Chart) whose content already renders its own, more specific header
   * right below, where a generic title above it would just be a
   * redundant duplicate label. */
  title?: string;
  subtitle?: string;
  count?: number;
  /** Positions this panel within the dashboard grid (App.tsx) -- one of
   * the `.grid-*` classes in index.css. Applied directly to the `.panel`
   * element itself, since that's the actual CSS grid item. */
  className?: string;
  children: ReactNode;
}) {
  return (
    <section className={props.className ? `panel ${props.className}` : "panel"}>
      {props.title && (
        <header className="panel-header">
          <div>
            <h2>{props.title}</h2>
            {props.subtitle && <p className="panel-subtitle">{props.subtitle}</p>}
          </div>
          {typeof props.count === "number" && <span className="panel-count">{props.count}</span>}
        </header>
      )}
      <div className="panel-body">{props.children}</div>
    </section>
  );
}

export function EmptyState(props: { children: ReactNode }) {
  return <div className="empty-state">{props.children}</div>;
}
