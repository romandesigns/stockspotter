// Re-exported from the shared package so the web app and the native
// WebView chart cannot drift apart on what "stale" or "gap" means --
// same pattern as reconcileBars.ts next door, and the same reasoning
// the motion specs use: one vocabulary, defined once, imported by
// everything that speaks it.
export { recordGap, resolveChartFreshness, seriesSpansGap, FRESHNESS_LABEL } from "@stockspotter/shared-types";
export type { ChartFreshness, FeedGap, FreshnessInput } from "@stockspotter/shared-types";
