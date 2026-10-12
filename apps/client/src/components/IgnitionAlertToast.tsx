// In-app toast for a real, confirmed ignition (2026-09-04) -- see
// useIgnitionAlerts.ts's own header comment for the full reasoning on
// why this exists alongside a real browser Notification + audio chime,
// not instead of them. Click-to-jump-to-symbol, same real target the
// Notification's own click would go to. Deliberately its own component,
// not a shared one with MicropullbackToast.tsx -- distinct wording and a
// distinct accent color (see index.css) so the two read as genuinely
// different signals at a glance, not one generic "alert" box.
import type { IgnitionAlertToastEntry } from "../lib/useIgnitionAlerts";
import { useLayoutEffect, useRef } from "react";

export function IgnitionAlertToast(props: {
  toasts: IgnitionAlertToastEntry[];
  overflow?: number;
  onShowAll?: () => void;
  onDismiss: (id: string) => void;
  onSelectSymbol: (symbol: string) => void;
}) {
  const removedFocus = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    // Expiry, dismissal and cap eviction must not strand keyboard focus.
    if (removedFocus.current && !removedFocus.current.isConnected && document.activeElement === document.body) {
      document.getElementById("ignition-panel")?.focus();
    }
    if (removedFocus.current && !removedFocus.current.isConnected) removedFocus.current = null;
  }, [props.toasts, props.overflow]);
  if (props.toasts.length === 0 && !props.overflow) return null;
  return (
    <div className="ignition-alert-toast-stack" onFocusCapture={(event) => {
      if (event.target instanceof HTMLElement) removedFocus.current = event.target;
    }}>
      {props.toasts.map((t) => (
        <div key={t.id} className="ignition-alert-toast">
        <button
          type="button"
          className="ignition-alert-toast-open"
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
        </button>
          <button
            type="button"
            className="ignition-alert-toast-close"
            onClick={() => props.onDismiss(t.id)}
            aria-label={`Dismiss ${t.symbol} ignition alert`}
          >
            ×
          </button>
        </div>
      ))}
      {!!props.overflow && <button type="button" className="ignition-alert-summary" onClick={props.onShowAll}>{props.overflow} more confirmations — open Ignition</button>}
    </div>
  );
}
