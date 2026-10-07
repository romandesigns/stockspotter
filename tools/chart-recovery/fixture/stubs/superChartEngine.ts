// Recording stand-in for apps/client/src/lib/superChartEngine.ts -- the
// ONLY application module this harness replaces (plus two inert hooks,
// see the sibling files). Everything above it in the tree is the real
// thing: real ChartPanel, real SuperChart, real useHistoricalBackfill,
// real derive/chartIndicators math, real Radix toolbar/popover.
//
// What it deliberately does NOT do: create a chart, a canvas context,
// a price scale or a series. A passing test here says "React committed
// the right lifecycle and handed the engine boundary the right input",
// and says nothing whatsoever about rendering fidelity, pixels or
// lightweight-charts behaviour.
//
// What it does model, because the component's cleanup contract depends
// on it:
//  - two separate DOM artefacts, one removed by chart.remove() (the
//    "canvas") and one by destroy() (the backdrop/handle nodes the real
//    engine's destroy() is responsible for) -- so a half-done cleanup
//    leaves a visible orphan;
//  - a ResizeObserver on the container, like the real engine's
//    (superChartEngine.ts:540-546), so hidden->visible/resize can be
//    observed rather than assumed;
//  - series/price-scale applyOptions, so the settings the mount effect
//    re-applies to a freshly mounted instance are observable.

import { markDead, markLive, nextInstanceId, record } from "../recorder";
import { symbolsOfBars } from "../data";

export type ChartType = "candles" | "line";

export interface StubCandle {
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

export interface SignalMarker {
  time: number;
  strategy: string;
}

type OptionBag = Record<string, unknown>;

export interface SuperChartApi {
  chart: {
    timeScale: () => { fitContent: () => void };
    priceScale: (id: string) => { applyOptions: (options: OptionBag) => void };
    applyOptions: (options: OptionBag) => void;
    remove: () => void;
  };
  series: Record<string, { applyOptions: (options: OptionBag) => void } | undefined>;
  setBars: (bars: StubCandle[]) => void;
  setSignalMarkers: (markers: SignalMarker[]) => void;
  setChartType: (type: ChartType) => void;
  destroy: () => void;
}

/** Every series key SuperChart.tsx ever touches. Created up front (all
 * defined) so `api.series.ma9?.applyOptions(...)` records instead of
 * silently short-circuiting -- a missing key would make a settings
 * assertion pass for the wrong reason. */
const SERIES_KEYS = [
  "area",
  "volume",
  "candles",
  "ma9",
  "ma20",
  "vwap",
  "bbUpper",
  "bbLower",
  "macdHist",
  "macdLine",
  "macdSignal",
  "rsi",
] as const;

/** The chart header that owns this container, read out of the committed
 * DOM. Independent of the bar payload, so the two can be cross-checked. */
function symbolFromDom(el: HTMLElement): string | null {
  const panel = el.closest(".super-chart-panel");
  return panel?.querySelector(".chart-ticker-symbol")?.textContent?.trim() ?? null;
}

/**
 * Options carry functions (autoscaleInfoProvider) that cannot cross the
 * page/runner boundary. `autoscaleInfoProvider` is the exact mechanism
 * "Fit all indicators" uses, so it is probed rather than dropped: the
 * real provider returns `original()` when fitting is on and `null` when
 * it is off, which makes the current setting directly observable.
 */
function serializableOptions(options: OptionBag): OptionBag {
  const out: OptionBag = {};
  for (const [key, value] of Object.entries(options)) {
    if (typeof value !== "function") {
      out[key] = value;
      continue;
    }
    if (key === "autoscaleInfoProvider") {
      const probe = (value as (original: () => unknown) => unknown)(() => "ORIGINAL");
      out.fitIndicators = probe === "ORIGINAL";
      continue;
    }
    out[key] = "[function]";
  }
  return out;
}

function barFields(bars: StubCandle[]) {
  return {
    barCount: bars.length,
    barSymbols: symbolsOfBars(bars),
    times: bars.map((b) => b.time),
    firstTime: bars[0]?.time,
    lastTime: bars[bars.length - 1]?.time,
    lastClose: bars[bars.length - 1]?.close,
  };
}

export function mountSuperChart(
  el: HTMLElement,
  context: string,
  instanceOpts: { bars: StubCandle[]; height?: number },
): SuperChartApi {
  const instance = nextInstanceId();
  markLive(instance);
  el.setAttribute("data-engine-instance", String(instance));

  const canvas = document.createElement("div");
  canvas.className = "stub-chart-canvas";
  canvas.dataset.engineInstance = String(instance);
  el.appendChild(canvas);

  const overlay = document.createElement("div");
  overlay.className = "stub-chart-overlay";
  overlay.dataset.engineInstance = String(instance);
  el.appendChild(overlay);

  record({
    instance,
    event: "mount",
    symbolFromDom: symbolFromDom(el),
    width: el.clientWidth,
    height: el.clientHeight,
    detail: { context, heightOption: instanceOpts.height ?? null },
    ...barFields(instanceOpts.bars),
  });

  const resizeObserver = new ResizeObserver(() => {
    record({ instance, event: "resize", symbolFromDom: symbolFromDom(el), width: el.clientWidth, height: el.clientHeight });
  });
  resizeObserver.observe(el);

  const series: SuperChartApi["series"] = {};
  for (const key of SERIES_KEYS) {
    series[key] = {
      applyOptions: (options: OptionBag) => {
        record({ instance, event: "series.applyOptions", symbolFromDom: symbolFromDom(el), detail: { key, options: serializableOptions(options) } });
      },
    };
  }

  return {
    chart: {
      timeScale: () => ({
        fitContent: () => record({ instance, event: "fitContent", symbolFromDom: symbolFromDom(el) }),
      }),
      priceScale: (id: string) => ({
        applyOptions: (options: OptionBag) =>
          record({ instance, event: "priceScale.applyOptions", symbolFromDom: symbolFromDom(el), detail: { id, options: serializableOptions(options) } }),
      }),
      applyOptions: (options: OptionBag) =>
        record({ instance, event: "chart.applyOptions", symbolFromDom: symbolFromDom(el), detail: { options: serializableOptions(options) } }),
      remove: () => {
        canvas.remove();
        record({ instance, event: "chart.remove", symbolFromDom: symbolFromDom(el) });
      },
    },
    series,
    setBars: (bars: StubCandle[]) => {
      record({ instance, event: "setBars", symbolFromDom: symbolFromDom(el), ...barFields(bars) });
    },
    setSignalMarkers: (markers: SignalMarker[]) => {
      record({ instance, event: "setSignalMarkers", symbolFromDom: symbolFromDom(el), detail: { count: markers.length } });
    },
    setChartType: (type: ChartType) => {
      record({ instance, event: "setChartType", symbolFromDom: symbolFromDom(el), detail: { type } });
    },
    destroy: () => {
      resizeObserver.disconnect();
      overlay.remove();
      markDead(instance);
      record({ instance, event: "destroy", symbolFromDom: symbolFromDom(el) });
    },
  };
}

/**
 * Same signature the component uses. The two accessors are invoked once
 * at wire time on purpose: they are the tooltip's view of "which bars
 * is this instance actually showing", so a mismatch between them and
 * the mount payload is caught here too.
 */
export function wireChartTooltip(
  _api: SuperChartApi,
  el: HTMLElement,
  getBars: () => StubCandle[],
  getBaseOpen: () => number,
): () => void {
  const bars = getBars();
  const instance = Number(el.getAttribute("data-engine-instance") ?? 0);
  record({
    instance,
    event: "wireTooltip",
    symbolFromDom: symbolFromDom(el),
    detail: { baseOpen: getBaseOpen() },
    ...barFields(bars),
  });
  return () => {
    record({ instance, event: "unwireTooltip", symbolFromDom: symbolFromDom(el) });
  };
}
