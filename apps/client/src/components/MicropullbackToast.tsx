// In-app toast for a real, high-confidence micropullback detection
// (2026-09-03) -- see useMicropullbackAlerts.ts's own header comment for
// the full reasoning on why this exists alongside a real browser
// Notification + audio chime, not instead of them. Click-to-jump-to-
// symbol, same real target the Notification's own click would go to.
import type { MicropullbackToastEntry } from "../lib/useMicropullbackAlerts";

export function MicropullbackToast(props: {
  toasts: MicropullbackToastEntry[];
  onDismiss: (id: string) => void;
  onSelectSymbol: (symbol: string) => void;
}) {
  if (props.toasts.length === 0) return null;
  return (
    <div className="micropullback-toast-stack">
      {props.toasts.map((t) => (
        <button
          key={t.id}
          className="micropullback-toast"
          onClick={() => {
            props.onSelectSymbol(t.symbol);
            props.onDismiss(t.id);
          }}
        >
          <span className="micropullback-toast-dot" />
          <span className="micropullback-toast-body">
            <span className="micropullback-toast-title">{t.symbol} micropullback forming</span>
            <span className="micropullback-toast-detail">
              ${t.price.toFixed(t.price < 1 ? 4 : 2)} · real momentum confirms — tap to open chart
            </span>
          </span>
          <span
            className="micropullback-toast-close"
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
