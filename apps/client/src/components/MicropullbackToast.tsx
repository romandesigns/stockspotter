// In-app toast for a real, high-confidence micropullback detection
// (2026-09-03) -- see useMicropullbackAlerts.ts's own header comment for
// the full reasoning on why this exists alongside a real browser
// Notification + audio chime, not instead of them. Click-to-jump-to-
// symbol, same real target the Notification's own click would go to.
import type { MicropullbackToastEntry } from "../lib/useMicropullbackAlerts";
import { useLayoutEffect, useRef } from "react";

export function MicropullbackToast(props: {
  toasts: MicropullbackToastEntry[];
  onDismiss: (id: string) => void;
  onSelectSymbol: (symbol: string) => void;
}) {
  const removedFocus = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    if (removedFocus.current && !removedFocus.current.isConnected && document.activeElement === document.body) {
      document.getElementById("ignition-panel")?.focus();
    }
    if (removedFocus.current && !removedFocus.current.isConnected) removedFocus.current = null;
  }, [props.toasts]);
  if (props.toasts.length === 0) return null;
  return (
    <div className="micropullback-toast-stack" onFocusCapture={(event) => {
      if (event.target instanceof HTMLElement) removedFocus.current = event.target;
    }}>
      {props.toasts.map((t) => (
        <div key={t.id} className="micropullback-toast">
        <button
          type="button"
          className="micropullback-toast-open"
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
        </button>
          <button
            type="button"
            className="micropullback-toast-close"
            onClick={() => props.onDismiss(t.id)}
            aria-label={`Dismiss ${t.symbol} micropullback alert`}
          >
            ×
          </button>
        </div>
      ))}
    </div>
  );
}
