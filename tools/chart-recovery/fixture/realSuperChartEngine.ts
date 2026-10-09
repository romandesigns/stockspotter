// Adapter only: all drawing, series options and viewport operations run
// through the application's real Lightweight Charts engine.
import { mountSuperChart as mountReal } from "app-src/lib/superChartEngine";
export * from "app-src/lib/superChartEngine";
export function mountSuperChart(...args: Parameters<typeof mountReal>) {
  const api = mountReal(...args);
  const state = { api, barCount: args[2].bars.length };
  const setBars = api.setBars;
  api.setBars = (bars) => { state.barCount = bars.length; setBars(bars); };
  (window as unknown as { realChartState: typeof state }).realChartState = state;
  return api;
}
