// In-app toast for a real, high-confidence micropullback detection
// (2026-09-03) -- see useMicropullbackAlerts.ts's own header comment for
// the full reasoning on why this exists alongside a real browser
// Notification + audio chime, not instead of them. Click-to-jump-to-
// symbol. OS notifications display a summary; chart navigation is here.
import type { MicropullbackToastEntry } from "../lib/useMicropullbackAlerts";
import { useLayoutEffect, useRef } from "react";

export function MicropullbackToast(props: {
  toasts: MicropullbackToastEntry[];
  overflow?: number;
  onDismiss: (id: string) => void;
  onSelectSymbol: (symbol: string) => void;
}) {
  const removedFocus = useRef<HTMLElement | null>(null);
  const stackRef = useRef<HTMLDivElement | null>(null);
  useLayoutEffect(() => {
    if (removedFocus.current && !removedFocus.current.isConnected && document.hasFocus() && (!document.activeElement || document.activeElement === document.body)) {
      const next = stackRef.current?.querySelector<HTMLElement>(".micropullback-toast-close");
      (next ?? document.getElementById("ignition-panel"))?.focus({ preventScroll: !!next });
    }
    if (removedFocus.current && !removedFocus.current.isConnected) removedFocus.current = null;
  }, [props.toasts]);
  if (props.toasts.length === 0 && !props.overflow) return null;
  return (
    <div ref={stackRef} className="micropullback-toast-stack" onFocusCapture={(event) => {
      if (event.target instanceof HTMLElement) removedFocus.current = event.target;
    }} onBlurCapture={(event) => {
      // A deliberate focus departure must not be mistaken for DOM removal.
      if (event.target === removedFocus.current && removedFocus.current?.isConnected) removedFocus.current = null;
    }}>
      {props.toasts.map((t) => (
        <div key={t.id} className="micropullback-toast">
        <button
          type="button"
          className="micropullback-toast-open"
          onClick={() => {
            props.onSelectSymbol(t.symbol);
            document.querySelector<HTMLElement>(".chart-symbol-select-trigger")?.focus();
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
      {!!props.overflow && <div className="micropullback-toast-summary">{props.overflow} more notices in this batch</div>}
    </div>
  );
}
