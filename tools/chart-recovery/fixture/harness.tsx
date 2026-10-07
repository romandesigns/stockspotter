// Browser fixture: the REAL ChartPanel (and therefore the real
// ChartSlot, SuperChart, useHistoricalBackfill, derive/mergeBars/
// toChartBars, chartIndicators.resample and the real Radix toolbar) in
// a real React 19 root, driven from outside by Playwright.
//
// Two things make it a lifecycle test rather than a helper test:
//  - state reaches the components exactly the way useRealtimeFeed
//    delivers it (real reconcileBars, real copy-the-Map-set-one-symbol
//    update shape), so array identity behaves as it does in the app;
//  - every mutation is committed with flushSync, so each step is its
//    own React commit with its effects flushed -- including the single
//    intermediate commit right after a symbol switch, which is exactly
//    where the F5 carryover is observable.
//
// window.harness is the whole control surface. It never reaches inside
// the components: timeframes, indicator toggles and chart type are
// changed by the runner clicking the real UI.

import { StrictMode, useSyncExternalStore } from "react";
import { flushSync } from "react-dom";
import { createRoot, type Root } from "react-dom/client";
import { reconcileBars, type BarUpdate, type CatalystUpdate, type MomentumUpdate } from "@stockspotter/shared-types";
import { ChartPanel } from "app-src/components/panels/ChartPanel";
import { clearEngineLog, engineLog, liveInstanceIds, type EngineEvent } from "./recorder";
import { liveBarTime, makeHistoryBars, makeLiveBar, type FixtureCandle } from "./data";
import { wakeLockStates } from "./stubs/useWakeLock";

/** Same per-symbol cap useRealtimeFeed applies. */
const MAX_BARS_PER_SYMBOL = 500;

interface FeedState {
  barsBySymbol: Map<string, BarUpdate[]>;
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  selectedSymbol: string | null;
}

let state: FeedState = {
  barsBySymbol: new Map(),
  subMinuteBarsBySymbol: new Map(),
  momentumBySymbol: new Map(),
  catalystsBySymbol: new Map(),
  selectedSymbol: null,
};

const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function getState(): FeedState {
  return state;
}

/** One external mutation === one React commit with its passive effects
 * flushed before control returns. */
function commit(next: FeedState): void {
  flushSync(() => {
    state = next;
    for (const listener of [...listeners]) listener();
  });
}

/**
 * Byte-for-byte the shape useRealtimeFeed's bar_update case uses:
 * reconcile into the symbol's own array, then set that array into a
 * COPY of the map. Every other symbol's array keeps its identity, which
 * is the exact contract ChartPanel's selected-array memoization (F8)
 * relies on -- so if that contract were wrong, these tests would fail
 * rather than quietly agree with the component.
 */
function applyBar(bar: BarUpdate): void {
  const minute = bar.intervalSecs !== 30;
  const previous = minute ? state.barsBySymbol : state.subMinuteBarsBySymbol;
  const existing = previous.get(bar.symbol) ?? [];
  const next = new Map(previous);
  next.set(bar.symbol, reconcileBars(existing, bar, MAX_BARS_PER_SYMBOL));
  commit(minute ? { ...state, barsBySymbol: next } : { ...state, subMinuteBarsBySymbol: next });
}

// ---------------------------------------------------------------------
// Controlled fetch. authenticatedFetch (the real one, from
// @stockspotter/shared-types) is left completely alone -- only the
// global fetch it calls is replaced, so the real request construction,
// header handling, status check and json() parsing all still run.

interface PendingFetch {
  id: number;
  url: string;
  symbol: string;
  settle: (response: Response) => void;
}

const pendingFetches = new Map<number, PendingFetch>();
const fetchCalls: { id: number; url: string; symbol: string }[] = [];
let nextFetchId = 1;

function symbolOfBarsUrl(url: string): string {
  const after = url.split("/bars/")[1];
  if (!after) return "";
  return decodeURIComponent(after.split("?")[0]);
}

globalThis.fetch = ((input: RequestInfo | URL, _init?: RequestInit) => {
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  const id = nextFetchId++;
  const symbol = symbolOfBarsUrl(url);
  fetchCalls.push({ id, url, symbol });
  return new Promise<Response>((resolve) => {
    pendingFetches.set(id, { id, url, symbol, settle: resolve });
  });
}) as typeof fetch;

// ---------------------------------------------------------------------

function HarnessApp() {
  const snapshot = useSyncExternalStore(subscribe, getState);
  return (
    <ChartPanel
      barsBySymbol={snapshot.barsBySymbol}
      subMinuteBarsBySymbol={snapshot.subMinuteBarsBySymbol}
      momentumBySymbol={snapshot.momentumBySymbol}
      catalystsBySymbol={snapshot.catalystsBySymbol}
      selectedSymbol={snapshot.selectedSymbol}
      onSelectedSymbolChange={(symbol) => commit({ ...state, selectedSymbol: symbol })}
    />
  );
}

let root: Root | null = null;

function mountRoot(strict: boolean): void {
  const host = document.getElementById("root");
  if (!host) throw new Error("harness: #root missing");
  root = createRoot(host);
  const tree = strict ? (
    <StrictMode>
      <HarnessApp />
    </StrictMode>
  ) : (
    <HarnessApp />
  );
  flushSync(() => root?.render(tree));
}

/** Let React's async work, the ResizeObserver and any resolved fetch
 * chain finish before the runner reads anything back. */
function settle(): Promise<void> {
  return new Promise<void>((resolve) => {
    setTimeout(() => {
      requestAnimationFrame(() => {
        setTimeout(() => resolve(), 0);
      });
    }, 0);
  });
}

interface DomSnapshot {
  emptyState: string | null;
  waiting: string | null;
  hasContainer: boolean;
  containerWidth: number | null;
  containerHeight: number | null;
  headerSymbol: string | null;
  headerPrice: string | null;
  headerChange: string | null;
  subMinuteNotice: string | null;
  activeTimeframe: string | null;
  canvasCount: number;
  overlayCount: number;
  liveInstances: number[];
}

function domSnapshot(): DomSnapshot {
  const container = document.querySelector<HTMLElement>(".super-chart");
  const activePill = document.querySelector<HTMLElement>('.chart-toolbar [data-state="on"]');
  return {
    emptyState: document.querySelector(".empty-state")?.textContent ?? null,
    waiting: document.querySelector(".super-chart-empty")?.textContent ?? null,
    hasContainer: container !== null,
    containerWidth: container?.clientWidth ?? null,
    containerHeight: container?.clientHeight ?? null,
    headerSymbol: document.querySelector(".chart-ticker-symbol")?.textContent ?? null,
    headerPrice: document.querySelector(".chart-ticker-price")?.textContent ?? null,
    headerChange: document.querySelector(".pct-up, .pct-down")?.textContent ?? null,
    subMinuteNotice: document.querySelector(".super-chart-submin-empty")?.textContent ?? null,
    activeTimeframe: activePill?.textContent ?? null,
    canvasCount: document.querySelectorAll(".stub-chart-canvas").length,
    overlayCount: document.querySelectorAll(".stub-chart-overlay").length,
    liveInstances: liveInstanceIds(),
  };
}

export interface HarnessApi {
  mount(options?: { strict?: boolean }): Promise<void>;
  unmount(): Promise<void>;
  select(symbol: string | null): Promise<void>;
  pushLiveBar(symbol: string, intervalSecs: 30 | 60, index: number, overrides?: { close?: number; isFinal?: boolean }): Promise<void>;
  pushLiveBars(symbol: string, intervalSecs: 30 | 60, count: number, startIndex?: number): Promise<void>;
  fetchCalls(): { id: number; url: string; symbol: string }[];
  pendingFetchIds(): number[];
  resolveFetch(id: number, historyCount: number): Promise<void>;
  failFetch(id: number, status?: number): Promise<void>;
  historyBars(symbol: string, count: number): FixtureCandle[];
  /** The exact timestamps pushLiveBar(s) produces, so a test can assert
   * against the input it actually sent rather than a shape guess. */
  liveBarTimes(symbol: string, intervalSecs: 30 | 60, count: number, startIndex?: number): number[];
  engineLog(): EngineEvent[];
  clearEngineLog(): Promise<void>;
  dom(): DomSnapshot;
  wakeLock(): boolean[];
  setPanelHeight(px: number): Promise<void>;
  settle(): Promise<void>;
}

const harness: HarnessApi = {
  async mount(options) {
    mountRoot(options?.strict === true);
    await settle();
  },
  async unmount() {
    root?.unmount();
    root = null;
    await settle();
  },
  async select(symbol) {
    commit({ ...state, selectedSymbol: symbol });
    await settle();
  },
  async pushLiveBar(symbol, intervalSecs, index, overrides) {
    applyBar(makeLiveBar(symbol, intervalSecs, index, overrides ?? {}));
    await settle();
  },
  async pushLiveBars(symbol, intervalSecs, count, startIndex = 0) {
    for (let i = 0; i < count; i++) applyBar(makeLiveBar(symbol, intervalSecs, startIndex + i));
    await settle();
  },
  fetchCalls: () => fetchCalls.map((c) => ({ ...c })),
  pendingFetchIds: () => [...pendingFetches.keys()].sort((a, b) => a - b),
  async resolveFetch(id, historyCount) {
    const pending = pendingFetches.get(id);
    if (!pending) throw new Error(`harness: fetch ${id} is not pending`);
    pendingFetches.delete(id);
    const body = JSON.stringify(makeHistoryBars(pending.symbol, historyCount));
    pending.settle(new Response(body, { status: 200, headers: { "Content-Type": "application/json" } }));
    await settle();
  },
  async failFetch(id, status = 503) {
    const pending = pendingFetches.get(id);
    if (!pending) throw new Error(`harness: fetch ${id} is not pending`);
    pendingFetches.delete(id);
    pending.settle(new Response("", { status }));
    await settle();
  },
  historyBars: (symbol, count) => makeHistoryBars(symbol, count),
  liveBarTimes: (symbol, intervalSecs, count, startIndex = 0) =>
    Array.from({ length: count }, (_, i) => liveBarTime(symbol, intervalSecs, startIndex + i)),
  engineLog,
  async clearEngineLog() {
    clearEngineLog();
    await settle();
  },
  dom: domSnapshot,
  wakeLock: wakeLockStates,
  async setPanelHeight(px) {
    document.documentElement.style.setProperty("--harness-height", `${px}px`);
    await settle();
  },
  settle,
};

declare global {
  interface Window {
    harness: HarnessApi;
  }
}

window.harness = harness;
// The runner waits on this rather than a timeout: the bundle is an ES
// module, so it evaluates after DOMContentLoaded.
document.documentElement.setAttribute("data-harness-ready", "true");
