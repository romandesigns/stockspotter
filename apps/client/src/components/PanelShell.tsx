import type { ReactNode } from "react";

export function PanelShell(props: {
  /** Omit entirely to skip the header row -- for a panel (like Super
   * Chart) whose content already renders its own, more specific header
   * right below, where a generic title above it would just be a
   * redundant duplicate label. */
  title?: string;
  subtitle?: string;
  count?: number;
  children: ReactNode;
}) {
  return (
    <section className="panel">
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
