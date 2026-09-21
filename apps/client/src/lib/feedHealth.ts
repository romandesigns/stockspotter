// Re-exported from the shared package so the web app and the native
// WebView chart cannot drift apart on what "stale", "quiet" or "gap" means
// -- same pattern as reconcileBars.ts next door, and the same reasoning
// the motion specs use: one vocabulary, defined once, imported by
// everything that speaks it.
export {
  recordGap,
  resolveChartFreshness,
  resolveChartStatus,
  resolveChartDisplay,
  seriesSpansGap,
  formatAge,
  FRESHNESS_LABEL,
  STATUS_LABEL,
} from "@stockspotter/shared-types";
export type {
  ChartFreshness,
  ChartStatus,
  ChartStatusInput,
  ChartDisplayState,
  ChartDisplayInput,
  FeedGap,
  FreshnessInput,
} from "@stockspotter/shared-types";

// Layer 2, the per-symbol view. Re-exported here so chart components have a
// single import site for both layers.
export {
  emptyCadence,
  observeUpdate,
  cadenceThresholds,
  resolveSymbolFreshness,
  CADENCE_WINDOW,
  MIN_GAPS_FOR_CADENCE,
  QUIET_FLOOR_SECS,
  QUIET_CEIL_SECS,
  STALE_FLOOR_SECS,
  STALE_CEIL_SECS,
} from "@stockspotter/shared-types";
export type {
  SymbolFreshness,
  SymbolCadenceState,
  CadenceThresholds,
  SymbolFreshnessResult,
} from "@stockspotter/shared-types";
