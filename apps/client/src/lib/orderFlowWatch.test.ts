import { afterEach, expect, test } from "bun:test";
import { __resetOrderFlowRegistry, attachOrderFlowSocket, dispatchOrderFlowFrame, watchOrderFlow, type OrderFlowState } from "./orderFlowWatch";
import type { OrderFlowResponse, OrderFlowSummary } from "./orderFlow";

afterEach(() => {
  __resetOrderFlowRegistry();
});

function summary(asOfSecs: number, full: boolean): OrderFlowSummary {
  const bucket = { startSecs: 0, buyVolume: 10, sellVolume: 4, unknownVolume: 0, open: 1, close: 1, bidSize: 5, askSize: 6 };
  return {
    asOfSecs,
    hasQuotes: true,
    windows: [{ windowSecs: 60, buyVolume: 10, sellVolume: 4, unknownVolume: 0, delta: 6, deltaRatio: 0.4, trades: 2, shares: 14, dollars: 28, tradesPerSec: 0.03, sharesPerSec: 0.2, dollarsPerSec: 0.5, shareVelocityAcceleration: null }],
    cumulativeDeltaPremarket: 0,
    cumulativeDeltaRegular: 6,
    cumulativeDeltaAfterHours: 0,
    large: { threshold: null, count: 0, volume: 0, buyVolume: 0, sellVolume: 0, delta: 0, intensity: null },
    absorption: { level: "none", side: null, intensity: null },
    absorptionLevels: [],
    netImpactEfficiency: null,
    exhaustion: { level: "none", velocityFading: false, deltaFading: false, largeTradesFading: false },
    sweeps: { state: "none", count: 0, buyCount: 0, sellCount: 0, flaggedBuyVolume: 0, flaggedSellVolume: 0, last: null },
    book: {
      ask: { removedSize: 0, addedSize: 0, levelsCleared: 0, refillsAtTouch: 0, possiblePersistentLiquidity: false },
      bid: { removedSize: 0, addedSize: 0, levelsCleared: 0, refillsAtTouch: 0, possiblePersistentLiquidity: false },
    },
    profile: { poc: 2, valueAreaLow: 1, valueAreaHigh: 3, hvn: [], lvn: [], bins: full ? [{ price: 2, volume: 14, buyVolume: 10, sellVolume: 4 }] : [], totalVolume: 14, sinceSecs: 500 },
    interpretation: { pressureSide: "buy", pressureLevel: "moderate", largeTrades: "none", flow: "confirming" },
    deltaBuckets: full ? [bucket] : [],
    largePrints: [],
    ...(full ? { chartArraysAsOfSecs: asOfSecs } : {}),
  };
}

function ok(body: OrderFlowResponse): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
}

/** Collects every state a watch emits, with a fetch that records its URLs. */
function harness(responses: (url: string) => Response | Promise<Response>) {
  const states: OrderFlowState[] = [];
  const urls: string[] = [];
  return {
    states,
    urls,
    options: {
      baseUrl: () => "http://api",
      onState: (state: OrderFlowState) => states.push(state),
      fetch: async (url: string) => {
        urls.push(url);
        return responses(url);
      },
    },
  };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("the first poll asks for the full shape and reports coverage and tape", async () => {
  const h = harness(() => ok({ enabled: true, symbol: "AAA", backfill: "done", coverage: "session", tape: { provenance: "uncorrected", correctionsSeen: 0, cancelsSeen: 0 }, summary: summary(1000, true) }));
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options });
  await settle();
  stop();
  expect(h.urls).toEqual(["http://api/orderflow/AAA/full"]);
  const state = h.states.at(-1)!;
  expect(state.status).toBe("ready");
  if (state.status !== "ready") throw new Error("unreachable");
  expect(state.coverage).toBe("session");
  expect(state.tape?.provenance).toBe("uncorrected");
  expect(state.summary.deltaBuckets).toHaveLength(1);
});

test("a server that predates coverage is read as partial unless it says done", async () => {
  for (const [backfill, expected] of [
    ["done", "session"],
    ["running", "session_pending"],
    ["failed", "partial"],
    [undefined, "partial"],
  ] as const) {
    const h = harness(() => ok({ enabled: true, symbol: "AAA", backfill, summary: summary(1000, true) }));
    const stop = watchOrderFlow({ symbol: "AAA", ...h.options });
    await settle();
    stop();
    const state = h.states.at(-1)!;
    expect(state.status).toBe("ready");
    if (state.status !== "ready") throw new Error("unreachable");
    expect(state.coverage).toBe(expected);
  }
});

test("a disabled server and a missing endpoint are different states", async () => {
  const disabled = harness(() => ok({ enabled: false, symbol: "AAA", summary: null }));
  let stop = watchOrderFlow({ symbol: "AAA", ...disabled.options });
  await settle();
  stop();
  expect(disabled.states.at(-1)!.status).toBe("disabled");

  const missing = harness(() => new Response("not found", { status: 404 }));
  stop = watchOrderFlow({ symbol: "AAA", ...missing.options });
  await settle();
  stop();
  expect(missing.states.at(-1)!.status).toBe("unavailable");

  const broken = harness(() => {
    throw new Error("network down");
  });
  stop = watchOrderFlow({ symbol: "AAA", ...broken.options });
  await settle();
  stop();
  expect(broken.states.at(-1)!.status).toBe("unavailable");
});

test("enabled with nothing traded yet is 'waiting', not 'ready' with an empty summary", async () => {
  const h = harness(() => ok({ enabled: true, symbol: "AAA", coverage: "partial", summary: null }));
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options });
  await settle();
  stop();
  expect(h.states.at(-1)!.status).toBe("waiting");
});

test("a throttled poll keeps the last good summary instead of blanking the strip", async () => {
  // Two watches share one onState and one fetch: the first call is
  // answered, the second is throttled. Waiting for the real five-second
  // poll interval is not worth it when what matters is that a 429 emits
  // nothing at all, so nothing can overwrite what is on screen.
  const h = harness((url) => (url.endsWith("/AAA/full") ? ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }) : new Response("slow down", { status: 429 })));
  const stopAnswered = watchOrderFlow({ symbol: "AAA", ...h.options });
  await settle();
  const afterAnswered = h.states.length;
  const stopThrottled = watchOrderFlow({ symbol: "BBB", ...h.options });
  await settle();
  stopAnswered();
  stopThrottled();
  expect(h.urls).toEqual(["http://api/orderflow/AAA/full", "http://api/orderflow/BBB/full"]);
  expect(h.states.length).toBe(afterAnswered);
  expect(h.states.at(-1)!.status).toBe("ready");
});

test("a pushed frame updates the state and carries the last full arrays forward", async () => {
  const h = harness(() => ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options, now: () => 5_000 });
  await settle();

  const handled = dispatchOrderFlowFrame({
    type: "order_flow",
    symbol: "AAA",
    enabled: true,
    coverage: "session",
    tape: { provenance: "corrections_pending", correctionsSeen: 2, cancelsSeen: 0 },
    summary: summary(1180, false),
  });
  expect(handled).toBe(true);
  const state = h.states.at(-1)!;
  expect(state.status).toBe("ready");
  if (state.status !== "ready") throw new Error("unreachable");
  expect(state.summary.asOfSecs).toBe(1180);
  // The arrays came from the full fetch and say so.
  expect(state.summary.deltaBuckets).toHaveLength(1);
  expect(state.summary.chartArraysAsOfSecs).toBe(1000);
  expect(state.tape?.correctionsSeen).toBe(2);
  stop();
});

/** A fetch that does not answer until the test says so. */
function deferred(): { promise: Promise<Response>; answer: (res: Response) => void } {
  let answer!: (res: Response) => void;
  const promise = new Promise<Response>((resolve) => {
    answer = resolve;
  });
  return { promise, answer };
}

test("a full response that arrives after a fresher push installs its arrays without rolling the header back", async () => {
  // The two sources do not answer in the order they were asked: the push
  // is live, and the full fetch takes as long as it takes. Before this,
  // the late body simply overwrote the state and the strip's header
  // stepped backwards two minutes, then forwards again on the next push.
  const slow = deferred();
  const h = harness(() => slow.promise);
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options, now: () => 5_000 });
  await settle();

  dispatchOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, coverage: "session", summary: summary(1180, false) });
  const pushed = h.states.at(-1)!;
  if (pushed.status !== "ready") throw new Error("unreachable");
  expect(pushed.summary.asOfSecs).toBe(1180);
  const afterPush = h.states.length;

  slow.answer(ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  await settle();
  const state = h.states.at(-1)!;
  if (state.status !== "ready") throw new Error("unreachable");
  // The header stays where the push put it...
  expect(state.summary.asOfSecs).toBe(1180);
  // ...and the arrays the late body carried are shown with their own age,
  // because a full fetch is the only place they come from.
  expect(state.summary.deltaBuckets).toHaveLength(1);
  expect(state.summary.chartArraysAsOfSecs).toBe(1000);
  expect(h.states.length).toBe(afterPush + 1);
  stop();
});

test("a late response with no arrays to add emits nothing at all", async () => {
  // Same race, but the late body is the lite shape (a server that answers
  // the full route with it). There is nothing in it the strip does not
  // already have more recently, so it must not emit.
  const slow = deferred();
  const h = harness(() => slow.promise);
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options, now: () => 5_000 });
  await settle();
  dispatchOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, coverage: "session", summary: summary(1180, false) });
  const afterPush = h.states.length;
  slow.answer(ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, false) }));
  await settle();
  expect(h.states.length).toBe(afterPush);
  const state = h.states.at(-1)!;
  if (state.status !== "ready") throw new Error("unreachable");
  expect(state.summary.asOfSecs).toBe(1180);
  stop();
});

test("a late body carrying older arrays does not replace newer ones", async () => {
  // Two answers that both carry arrays, the older arriving last.
  // Adopting it would swap a current delta pane for an older one.
  const slow = deferred();
  const h = harness(() => slow.promise);
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options, now: () => 5_000 });
  await settle();
  dispatchOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, coverage: "session", summary: summary(2000, true) });
  const fresh = h.states.at(-1)!;
  if (fresh.status !== "ready") throw new Error("unreachable");
  expect(fresh.summary.chartArraysAsOfSecs).toBe(2000);
  const afterPush = h.states.length;

  slow.answer(ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  await settle();
  // Nothing was emitted, and what is on screen still has the newer
  // arrays with their own (current) age.
  expect(h.states.length).toBe(afterPush);
  const state = h.states.at(-1)!;
  if (state.status !== "ready") throw new Error("unreachable");
  expect(state.summary.chartArraysAsOfSecs).toBe(2000);
  expect(state.summary.asOfSecs).toBe(2000);
  stop();
});

test("a malformed body, a body for another symbol and a non-JSON answer are all 'unavailable'", async () => {
  // The HTTP path used to cast the body. Each of these produced a state
  // the strip then dereferenced: `exhaustion.level` of an object that has
  // no `exhaustion`, or another symbol's numbers under the open chart.
  const cases: Array<[string, () => Response]> = [
    ["a summary missing the fields the strip reads", () => ok({ enabled: true, symbol: "AAA", summary: { asOfSecs: 1, windows: [], interpretation: {} } } as never)],
    ["another symbol's body", () => ok({ enabled: true, symbol: "ZZZ", summary: summary(1000, true) })],
    ["not JSON at all", () => new Response("<html>bad gateway</html>", { status: 200, headers: { "content-type": "text/html" } })],
  ];
  for (const [what, respond] of cases) {
    const h = harness(respond);
    const stop = watchOrderFlow({ symbol: "AAA", ...h.options });
    await settle();
    stop();
    expect(h.states.at(-1)!.status, what).toBe("unavailable");
  }
});

test("a new day's summary does not keep yesterday's delta pane under it", async () => {
  const h = harness(() => ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options, now: () => 5_000 });
  await settle();
  const first = h.states.at(-1)!;
  if (first.status !== "ready") throw new Error("unreachable");
  expect(first.summary.deltaBuckets).toHaveLength(1);

  // The chart was left open overnight and the engine is on a new session.
  dispatchOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, coverage: "partial", summary: summary(1000 + 24 * 3600, false) });
  const state = h.states.at(-1)!;
  if (state.status !== "ready") throw new Error("unreachable");
  expect(state.summary.deltaBuckets).toEqual([]);
  expect(state.summary.chartArraysAsOfSecs).toBeUndefined();
  stop();
});

test("frames for another symbol, and frames after the watch stops, are ignored", async () => {
  const h = harness(() => ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  const stop = watchOrderFlow({ symbol: "AAA", ...h.options });
  await settle();
  const before = h.states.length;
  expect(dispatchOrderFlowFrame({ type: "order_flow", symbol: "ZZZ", enabled: true, summary: summary(1180, false) })).toBe(true);
  expect(h.states.length).toBe(before);
  stop();
  dispatchOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, summary: summary(1180, false) });
  expect(h.states.length).toBe(before);
});

test("a message that is not an order-flow frame is reported as unhandled", () => {
  expect(dispatchOrderFlowFrame({ type: "bar_update", symbol: "AAA" })).toBe(false);
  expect(dispatchOrderFlowFrame(undefined)).toBe(false);
});

test("nothing is sent on the socket until a chart is open, and the list is re-sent on reconnect", async () => {
  const sent: string[][] = [];
  attachOrderFlowSocket((symbols) => sent.push(symbols));
  // Attaching with no watcher must not send a subscription at all.
  expect(sent).toEqual([]);

  const h = harness(() => ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  const stopA = watchOrderFlow({ symbol: "AAA", ...h.options });
  const stopB = watchOrderFlow({ symbol: "BBB", ...h.options });
  await settle();
  expect(sent).toEqual([["AAA"], ["AAA", "BBB"]]);

  // Socket drops, then a new one is welcomed: the current list is re-sent,
  // because a new socket starts with no subscription.
  attachOrderFlowSocket(null);
  const reconnected: string[][] = [];
  attachOrderFlowSocket((symbols) => reconnected.push(symbols));
  expect(reconnected).toEqual([["AAA", "BBB"]]);

  stopB();
  expect(reconnected.at(-1)).toEqual(["AAA"]);
  stopA();
  expect(reconnected.at(-1)).toEqual([]);
});

test("two charts on the same symbol subscribe once and both are fed", async () => {
  const sent: string[][] = [];
  attachOrderFlowSocket((symbols) => sent.push(symbols));
  const h1 = harness(() => ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  const h2 = harness(() => ok({ enabled: true, symbol: "AAA", coverage: "session", summary: summary(1000, true) }));
  const stop1 = watchOrderFlow({ symbol: "AAA", ...h1.options });
  const stop2 = watchOrderFlow({ symbol: "AAA", ...h2.options });
  await settle();
  expect(sent).toEqual([["AAA"]]);

  dispatchOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, coverage: "session", summary: summary(1180, false) });
  expect(h1.states.at(-1)!.status).toBe("ready");
  expect(h2.states.at(-1)!.status).toBe("ready");

  // Closing one chart must not unsubscribe the other.
  stop1();
  expect(sent).toEqual([["AAA"]]);
  stop2();
  expect(sent.at(-1)).toEqual([]);
});
