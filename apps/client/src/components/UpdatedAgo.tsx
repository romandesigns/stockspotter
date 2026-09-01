// Small "updated Xs ago" indicator for a panel backed by a live-polled
// REST endpoint (Top Gainers' live mode, Highly Trading) -- makes a
// background re-poll visible even when the ranking itself doesn't
// visibly reorder (thin premarket volume can keep the same symbol
// pinned at #1 across several 60s scans), which otherwise looks
// indistinguishable from "stopped updating" at a glance.

import { useEffect, useState } from "react";

function relativeLabel(lastUpdated: Date | null, now: number): string {
  if (!lastUpdated) return "updating…";
  const seconds = Math.max(0, Math.round((now - lastUpdated.getTime()) / 1000));
  if (seconds < 5) return "updated just now";
  if (seconds < 60) return `updated ${seconds}s ago`;
  return `updated ${Math.round(seconds / 60)}m ago`;
}

export function UpdatedAgo(props: { lastUpdated: Date | null }) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  return <span className="updated-ago dim">{relativeLabel(props.lastUpdated, now)}</span>;
}
