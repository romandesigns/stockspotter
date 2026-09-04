// Replaces the old CSS-only `@media (max-width: 1400px)` dashboard
// fallback (index.css used to just override .dashboard-grid's grid
// properties at that width) -- that trick doesn't carry over to the new
// resizable-panel layout, since PanelGroup's `direction` is a React prop,
// not something a media query can flip the way grid-template-columns
// could. Same breakpoint, same intent ("Roman's own reference is
// explicitly 'Web 1920' desktop-first; below that this degrades to a
// scrolling stack, a desktop-viewport promise, not a claim this fits a
// phone"), just decided in JS now so App.tsx can render an entirely
// different (stacked, no resize handles) tree below the breakpoint.
import { useEffect, useState } from "react";

const BREAKPOINT_QUERY = "(max-width: 1400px)";

export function useIsNarrowViewport(): boolean {
  const [narrow, setNarrow] = useState(() => (typeof window !== "undefined" ? window.matchMedia(BREAKPOINT_QUERY).matches : false));

  useEffect(() => {
    const mql = window.matchMedia(BREAKPOINT_QUERY);
    const onChange = () => setNarrow(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, []);

  return narrow;
}
