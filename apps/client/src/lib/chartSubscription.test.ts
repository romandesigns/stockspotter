import { afterEach, expect, test } from "bun:test";
import { CHART_SUBSCRIPTION_LIMIT, type ClientSubscribeChart } from "@stockspotter/shared-types";
import { __resetChartSubscriptions, attachChartSocket, registerChartSymbol } from "./chartSubscription";

afterEach(() => __resetChartSubscriptions());
const tick = () => new Promise<void>((r) => queueMicrotask(r));

function socket() {
  const sent: string[][] = [];
  return { sent, send: (m: ClientSubscribeChart) => { expect(m.type).toBe("subscribe_chart"); sent.push(m.symbols); } };
}

test("nothing is sent with no chart open, even on attach", async () => {
  const s = socket();
  attachChartSocket(s.send);
  await tick();
  expect(s.sent).toEqual([]);
});

test("a symbol switch in one commit sends one message with the new set", async () => {
  const s = socket();
  attachChartSocket(s.send);
  const releaseA = registerChartSymbol("AAA");
  await tick();
  releaseA();
  registerChartSymbol("BBB");
  await tick();
  expect(s.sent).toEqual([["AAA"], ["BBB"]]);
});

test("two charts on one symbol are one entry until both close; closing the last sends an empty set", async () => {
  const s = socket();
  attachChartSocket(s.send);
  const r1 = registerChartSymbol("AAA");
  const r2 = registerChartSymbol("aaa");
  await tick();
  r1();
  r1(); // releasing twice is harmless
  await tick();
  r2();
  await tick();
  expect(s.sent).toEqual([["AAA"], []]);
});

test("re-sent to every new socket; registered before any socket is sent on attach", async () => {
  registerChartSymbol("AAA");
  registerChartSymbol("BBB");
  await tick();
  const first = socket();
  attachChartSocket(first.send);
  attachChartSocket(null);
  const second = socket();
  attachChartSocket(second.send);
  expect(first.sent).toEqual([["AAA", "BBB"]]);
  expect(second.sent).toEqual([["AAA", "BBB"]]);
});

test("capped at the server's limit, in registration order", async () => {
  const s = socket();
  attachChartSocket(s.send);
  for (let i = 0; i < CHART_SUBSCRIPTION_LIMIT + 2; i++) registerChartSymbol(`S${i}`);
  await tick();
  expect(s.sent.at(-1)).toHaveLength(CHART_SUBSCRIPTION_LIMIT);
  expect(s.sent.at(-1)![0]).toBe("S0");
});
