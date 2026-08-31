import type { ReactNode } from "react";

export function PanelShell(props: {
  title: string;
  subtitle?: string;
  count?: number;
  children: ReactNode;
}) {
  return (
    <section className="panel">
      <header className="panel-header">
        <div>
          <h2>{props.title}</h2>
          {props.subtitle && <p className="panel-subtitle">{props.subtitle}</p>}
        </div>
        {typeof props.count === "number" && <span className="panel-count">{props.count}</span>}
      </header>
      <div className="panel-body">{props.children}</div>
    </section>
  );
}

export function EmptyState(props: { children: ReactNode }) {
  return <div className="empty-state">{props.children}</div>;
}
