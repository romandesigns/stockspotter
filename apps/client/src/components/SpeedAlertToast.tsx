// In-app pop-up for a speed alert from the Halt Early-Warning panel
// (2026-10-16, approved Alerts board). Same stack and lifecycle as
// IgnitionAlertToast.tsx (top-right attention-toasts column, 1 s burst
// coalescing, capped at three with an overflow line), but its own
// component: it carries three actions (Open chart / Save / Mute) and the
// gauge mini ring, so a generalised "alert box" would have had to grow
// every one of these as options. The ring is the same PressureGauge as
// the card, so the pop-up visibly is that card's gauge.
import { useLayoutEffect, useRef } from "react";
import type { SpeedToastEntry } from "../lib/useHaltSpeed";
import { alertDetail, formatClock } from "../lib/haltSpeedView";
import { formatPrice } from "../lib/format";
import { PressureGauge } from "./PressureGauge";

export function SpeedAlertToast(props: {
  toasts: SpeedToastEntry[];
  overflow?: number;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onMute: (symbol: string) => void;
  onDismiss: (id: string) => void;
  onSelectSymbol: (symbol: string) => void;
  onShowAll?: () => void;
}) {
  const removedFocus = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    // Expiry, dismissal and mute must not strand keyboard focus on a
    // removed node (same guard as the ignition toast).
    if (removedFocus.current && !removedFocus.current.isConnected && document.activeElement === document.body) {
      document.getElementById("halt-panel")?.focus();
    }
    if (removedFocus.current && !removedFocus.current.isConnected) removedFocus.current = null;
  }, [props.toasts, props.overflow]);
  if (props.toasts.length === 0 && !props.overflow) return null;
  return (
    <div
      className="speed-alert-toast-stack"
      onFocusCapture={(event) => {
        if (event.target instanceof HTMLElement) removedFocus.current = event.target;
      }}
    >
      {props.toasts.map((t) => {
        const saved = props.saved.has(t.symbol);
        return (
          <div key={t.id} role="status" className="speed-alert-toast">
            <PressureGauge proximityRatio={t.pct / 100} bullish={t.bullish} size="sm" />
            <div className="speed-alert-toast-body">
              <div className="speed-alert-toast-head">
                <span className="speed-alert-toast-symbol">{t.symbol}</span>
                <span className={t.bullish ? "price pct-up" : "price pct-down"}>
                  {t.bullish ? "▲" : "▼"} {formatPrice(t.price)}
                </span>
                <span className="speed-alert-toast-time">{formatClock(t.at)}</span>
                <button type="button" className="speed-alert-toast-close" onClick={() => props.onDismiss(t.id)} aria-label={`Dismiss ${t.symbol} speed alert`}>
                  ×
                </button>
              </div>
              <div className="speed-alert-toast-detail">{alertDetail(t)}</div>
              <div className="speed-alert-toast-actions">
                <button
                  type="button"
                  className="speed-alert-toast-primary"
                  onClick={() => {
                    props.onSelectSymbol(t.symbol);
                    props.onDismiss(t.id);
                  }}
                >
                  Open chart
                </button>
                <button type="button" className="speed-alert-toast-secondary" aria-pressed={saved} onClick={() => props.onToggleSaved(t.symbol)}>
                  {saved ? "Saved" : "Save"}
                </button>
                <button type="button" className="speed-alert-toast-ghost" onClick={() => props.onMute(t.symbol)}>
                  Mute {t.symbol} 30 min
                </button>
              </div>
            </div>
          </div>
        );
      })}
      {!!props.overflow && (
        <button type="button" className="speed-alert-summary" onClick={props.onShowAll}>
          {props.overflow} more speed alerts — open Halt Early-Warning
        </button>
      )}
    </div>
  );
}
