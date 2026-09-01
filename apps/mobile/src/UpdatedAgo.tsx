// RN port of the web app's own UpdatedAgo (apps/client/src/components/
// UpdatedAgo.tsx) -- same relative-time formatting, same reasoning (a
// live-polled panel's ranking can legitimately hold still for several
// scans in a row, e.g. one extreme premarket gapper staying pinned at
// #1; without this, that's indistinguishable from "stopped updating").
import { useEffect, useState } from "react";
import { Text } from "react-native";
import { colors, monoFont } from "./theme";

function relativeLabel(lastUpdated: Date | null, now: number): string {
  if (!lastUpdated) return "updating…";
  const seconds = Math.max(0, Math.round((now - lastUpdated.getTime()) / 1000));
  if (seconds < 5) return "updated just now";
  if (seconds < 60) return `updated ${seconds}s ago`;
  return `updated ${Math.round(seconds / 60)}m ago`;
}

export function UpdatedAgo({ lastUpdated }: { lastUpdated: Date | null }) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  return <Text style={{ color: colors.muted, fontFamily: monoFont, fontSize: 10 }}>{relativeLabel(lastUpdated, now)}</Text>;
}
