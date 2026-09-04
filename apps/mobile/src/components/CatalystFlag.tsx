// Moved out of App.tsx (2026-09-04) so HaltMiniCard.tsx can reuse it too
// (ChartScreen's new halt-risk row wants the same real component the
// Radar tab uses, not a re-derived copy) -- without a circular import
// back into App.tsx.
import type { CatalystUpdate } from "@stockspotter/shared-types";
import { Text } from "./ui/text";

/** Small inline flag next to a ticker, real not decorative -- renders
 * nothing for a symbol with no catalyst record, same rule the web app's
 * CatalystBadge follows. */
export function CatalystFlag({ symbol, catalysts }: { symbol: string; catalysts: Map<string, CatalystUpdate> }) {
  if (!catalysts.has(symbol)) return null;
  return (
    <Text variant="accent" className="mr-2 text-[11px]" accessibilityLabel={`${symbol} has a catalyst`}>
      ⚑
    </Text>
  );
}
