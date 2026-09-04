// In-app toast for a real, confirmed ignition (2026-09-04) -- see
// useIgnitionAlerts.ts's own header comment for the full reasoning on
// why this exists alongside a real browser Notification + audio chime,
// not instead of them. Click-to-jump-to-symbol, same real target the
// Notification's own click would go to. Deliberately its own component,
// not a shared one with MicropullbackToast.tsx -- distinct wording and a
// distinct accent color (see index.css) so the two read as genuinely
// different signals at a glance, not one generic "alert" box.
import type { IgnitionAlertToastEntry } from "../lib/useIgnitionAlerts";

export function IgnitionAlertToast(props: {
  toasts: IgnitionAlertToastEntry[];
  onDismiss: (id: string) => void;
  onSelectSymbol: (symbol: string) => void;
}) {
  if (props.toasts.length === 0) return null;
  return (
    <div className="ignition-alert-toast-stack">
      {props.toasts.map((t) => (
        <button
          key={t.id}
          className="ignition-alert-toast"
          onClick={() => {
            props.onSelectSymbol(t.symbol);
            props.onDismiss(t.id);
          }}
        >
          <span className="ignition-alert-toast-dot" />
          <span className="ignition-alert-toast-body">
            <span className="ignition-alert-toast-title">{t.symbol} ignition confirmed</span>
            <span className="ignition-alert-toast-detail">
              ${t.price.toFixed(t.price < 1 ? 4 : 2)} · real follow-through — tap to open chart
            </span>
          </span>
          <span
            className="ignition-alert-toast-close"
            onClick={(e) => {
              e.stopPropagation();
              props.onDismiss(t.id);
            }}
            role="button"
            aria-label="Dismiss"
          >
            ×
          </span>
        </button>
      ))}
    </div>
  );
}
