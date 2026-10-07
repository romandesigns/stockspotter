#!/usr/bin/env node
"use strict";

// React lifecycle regression harness for the Stockspotter chart
// (Batch1: F5 symbol-history carryover, F6 mount readiness, F8
// selected-array memoization).
//
// Serves the Bun-built fixture from an ephemeral loopback port and
// drives the REAL components in Chromium through Playwright. Every
// assertion is about what a real React commit did: which engine
// instances were mounted/destroyed, what bar payload crossed the engine
// boundary, and what the committed DOM said at that moment.
//
// Scope, stated plainly: the chart engine is a recording stub. Nothing
// here proves rendering, pixels, series parity or latency -- the real-
// engine twelve-series parity report is a separate artefact and stays
// separate. What a green run proves is lifecycle and data-routing
// behaviour, which is exactly what F5/F6/F8 are.
//
// Requires (no new dependencies, nothing installed by this script):
//   PLAYWRIGHT_MODULE  path to the installed playwright-core module
//   CHROME_PATH        path to the Chromium executable
//
//   node tools/chart-recovery/run-tests.cjs
//   node tools/chart-recovery/run-tests.cjs --filter "unrelated"
//
// Build first (and re-build to switch source roots):
//   bun tools/chart-recovery/build.ts
//   bun tools/chart-recovery/build.ts --source-root <other-worktree>/apps/client/src

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert/strict");

const DIST = path.join(__dirname, "dist");
const FILTER = argValue("filter");
const HEADED = process.argv.includes("--headed");
/** @type {RegExp[]} see the console handler in main(). */
const ALLOWED_CONSOLE_ERRORS = [];

function argValue(name) {
  const flag = `--${name}`;
  const index = process.argv.indexOf(flag);
  if (index !== -1) return process.argv[index + 1];
  const inline = process.argv.find((a) => a.startsWith(`${flag}=`));
  return inline ? inline.slice(flag.length + 1) : undefined;
}

// ---------------------------------------------------------------------
// Fixture server: two files, loopback only, no caching.

const CONTENT_TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".json": "application/json" };

function startServer() {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    const name = url.pathname === "/" ? "index.html" : path.basename(url.pathname);
    const file = path.join(DIST, name);
    if (!fs.existsSync(file) || !file.startsWith(DIST)) {
      res.writeHead(404, { "Content-Type": "text/plain" });
      res.end("not found");
      return;
    }
    res.writeHead(200, { "Content-Type": CONTENT_TYPES[path.extname(file)] ?? "application/octet-stream", "Cache-Control": "no-store" });
    res.end(fs.readFileSync(file));
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve({ server, port: server.address().port }));
  });
}

// ---------------------------------------------------------------------
// Harness access helpers.

/** Call a window.harness method in the page and await its result. */
function call(page, method, args = []) {
  return page.evaluate(([m, a]) => window.harness[m](...a), [method, args]);
}

const only = (log, event) => log.filter((e) => e.event === event);
/** ResizeObserver noise is real but orthogonal -- filtered out of the
 * "nothing at all happened" assertions, and asserted on directly in the
 * resize test instead. */
const withoutResize = (log) => log.filter((e) => e.event !== "resize");

async function clickTimeframe(page, label) {
  await page.click(`.chart-toolbar button:text-is("${label}")`);
  await call(page, "settle");
}

/**
 * What identifies a 30s payload. Deliberately NOT `every(t => t % 60
 * === 30)`: consecutive 30s stamps alternate 30 and 0, so that holds
 * only for a one-bar series and is false for every real one. The
 * boundary the block starts on plus a strict 30s step is the actual
 * property, checked against `expected` -- the exact timestamps the
 * fixture pushed -- so the counts and times under test are stated, not
 * inferred.
 */
function assertSubMinuteSeries(times, expected, message) {
  assert.ok(expected.length > 0, `${message}: the fixture pushed no 30s bar`);
  assert.deepEqual(times, expected, `${message}: exactly the 30s bars that were pushed, unresampled`);
  assert.equal(times[0] % 60, 30, `${message}: starts on a half-minute boundary`);
  for (let i = 1; i < times.length; i++) {
    assert.equal(times[i] - times[i - 1], 30, `${message}: 30s step at index ${i}`);
  }
  assert.ok(times.some((t) => t % 60 !== 0), `${message}: carries off-minute stamps, so it is not a minute series`);
}

// ---------------------------------------------------------------------
// Tests. Each runs in a fresh page against a fresh React root.

const tests = [];
const test = (name, fn, options) => tests.push({ name, fn, options });

test("initially empty 1m: first bar mounts exactly once, ticks do not remount", async (page) => {
  let dom = await call(page, "dom");
  assert.match(dom.emptyState ?? "", /No bars yet/, "no symbols and no selection is the panel's empty state");
  assert.deepEqual(await call(page, "fetchCalls"), [], "nothing selected must not fetch history");

  await call(page, "select", ["AAA"]);
  dom = await call(page, "dom");
  assert.equal(dom.hasContainer, false, "the waiting state has no chart container at all");
  assert.match(dom.waiting ?? "", /Waiting for bars for AAA/);
  assert.deepEqual(only(await call(page, "engineLog"), "mount"), []);

  await call(page, "pushLiveBar", ["AAA", 60, 0]);
  let log = await call(page, "engineLog");
  const mounts = only(log, "mount");
  assert.equal(mounts.length, 1, "the first 1m bar must mount exactly one engine");
  assert.deepEqual(mounts[0].barSymbols, ["AAA"]);
  assert.equal(mounts[0].symbolFromDom, "AAA", "the mounted container belongs to the rendered symbol header");

  dom = await call(page, "dom");
  assert.equal(dom.hasContainer, true);
  assert.equal(dom.waiting, null);
  assert.equal(dom.canvasCount, 1);
  assert.equal(dom.liveInstances.length, 1);
  assert.match(dom.headerPrice ?? "", /^\$\d/, "the existing header renders from the first bar, still guarded by the empty state");
  assert.match(dom.headerChange ?? "", /%/);

  await call(page, "pushLiveBars", ["AAA", 60, 3, 1]);
  log = await call(page, "engineLog");
  assert.equal(only(log, "mount").length, 1, "live ticks must not remount");
  assert.equal(only(log, "destroy").length, 0);
  const setBars = only(log, "setBars");
  assert.equal(setBars.at(-1).barCount, 4);
  for (const entry of setBars) assert.deepEqual(entry.barSymbols, ["AAA"]);
  assert.deepEqual(await call(page, "wakeLock"), [false, true], "the wake lock follows real bar availability");
});

test("initially empty 5m: first bar mounts a resampled chart exactly once", async (page) => {
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 12]);
  await clickTimeframe(page, "5m");
  assert.equal((await call(page, "dom")).activeTimeframe, "5m");

  await call(page, "clearEngineLog");
  await call(page, "select", ["BBB"]);
  let dom = await call(page, "dom");
  assert.equal(dom.hasContainer, false, "BBB has no bars at all yet");
  assert.match(dom.waiting ?? "", /Waiting for bars for BBB/);
  let log = await call(page, "engineLog");
  assert.equal(only(log, "mount").length, 0);
  assert.equal(only(log, "destroy").length, 1, "AAA's engine is torn down on the symbol change");
  assert.deepEqual(dom.liveInstances, [], "no engine survives into the empty state");
  assert.equal(dom.canvasCount, 0, "no orphaned canvas");

  await call(page, "clearEngineLog");
  await call(page, "pushLiveBars", ["BBB", 60, 12]);
  log = await call(page, "engineLog");
  const mounts = only(log, "mount");
  assert.equal(mounts.length, 1, "the first minute bar must mount the waiting 5m chart exactly once");
  assert.deepEqual(mounts[0].barSymbols, ["BBB"]);
  const last = only(log, "setBars").at(-1);
  assert.equal(last.barCount, 3, "twelve 1m bars resample to three 5m buckets");
  assert.ok(last.times.every((t) => t % 300 === 0), "payload is 5m bucket-aligned, not raw 1m data");
});

test("first HISTORY arrival on a completely empty chart mounts it", async (page) => {
  // The other readiness tests flip `chartReady` with a live bar. This
  // one never pushes a bar at all: the ONLY input is the backfill hook's
  // own response, so it is the hook/mount interaction that is under
  // test, not the feed's.
  await call(page, "select", ["AAA"]);
  let dom = await call(page, "dom");
  assert.equal(dom.hasContainer, false, "no live bar and no history yet");
  assert.match(dom.waiting ?? "", /Waiting for bars for AAA/);
  assert.deepEqual(only(await call(page, "engineLog"), "mount"), []);

  const pending = (await call(page, "fetchCalls")).find((c) => c.symbol === "AAA");
  assert.ok(pending, "selecting a symbol with no bars at all still requests its history");

  await call(page, "clearEngineLog");
  await call(page, "resolveFetch", [pending.id, 5]);
  let log = await call(page, "engineLog");
  const mounts = only(log, "mount");
  assert.equal(mounts.length, 1, "history alone mounts exactly one engine");
  assert.deepEqual(mounts[0].barSymbols, ["AAA"]);
  assert.equal(mounts[0].symbolFromDom, "AAA");
  assert.deepEqual(
    mounts[0].times,
    (await call(page, "historyBars", ["AAA", 5])).map((b) => b.time),
    "it mounts on exactly the fetched history, unshifted by the 1m resample",
  );

  dom = await call(page, "dom");
  assert.equal(dom.hasContainer, true);
  assert.equal(dom.waiting, null);
  assert.equal(dom.canvasCount, 1);
  assert.equal(dom.liveInstances.length, 1);

  await call(page, "clearEngineLog");
  await call(page, "pushLiveBar", ["AAA", 60, 0]);
  log = await call(page, "engineLog");
  assert.deepEqual(only(log, "mount"), [], "the first live bar after history must not remount");
  assert.equal(only(log, "setBars").at(-1).barCount, 6, "it merges into the history already on screen");
  assert.deepEqual(only(log, "setBars").at(-1).barSymbols, ["AAA"]);
});

test("30s readiness in both directions (minute-empty and sub-minute-empty)", async (page) => {
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 4]);
  await call(page, "clearEngineLog");
  await clickTimeframe(page, "30s");

  let dom = await call(page, "dom");
  assert.equal(dom.activeTimeframe, "30s");
  assert.match(dom.subMinuteNotice ?? "", /building 30s candles/, "the existing 30s waiting UX is preserved");
  assert.equal(dom.hasContainer, true, "minute data keeps the container mounted");
  let log = await call(page, "engineLog");
  assert.equal(only(log, "mount").length, 0, "nothing to mount: no sub-minute bar exists yet");
  for (const e of only(log, "setBars")) assert.ok(e.barCount > 0, "the engine is never handed an empty series");
  for (const e of only(log, "setBars")) {
    const destroyed = log.find((d) => d.event === "destroy" && d.instance === e.instance && d.seq < e.seq);
    assert.equal(destroyed, undefined, "no engine call may land on a destroyed instance");
  }

  await call(page, "pushLiveBar", ["AAA", 30, 0]);
  log = await call(page, "engineLog");
  let mounts = only(log, "mount");
  assert.equal(mounts.length, 1, "the first 30s bar mounts the waiting chart exactly once");
  assert.deepEqual(mounts[0].barSymbols, ["AAA"]);
  assert.equal(mounts[0].barCount, 1, "exactly the one 30s bar that exists");
  assertSubMinuteSeries(mounts[0].times, await call(page, "liveBarTimes", ["AAA", 30, 1]), "AAA's first 30s bar");
  dom = await call(page, "dom");
  assert.equal(dom.subMinuteNotice, null);
  assert.equal(dom.canvasCount, 1);

  // Other direction: a symbol with 30s data but no minute data at all,
  // while "30s" is still the selected timeframe. The minute-empty early
  // return removes the container even though sub-minute data exists.
  await call(page, "pushLiveBars", ["BBB", 30, 3]);
  await call(page, "clearEngineLog");
  await call(page, "select", ["BBB"]);
  dom = await call(page, "dom");
  assert.equal(dom.hasContainer, false, "minute-empty removes the container even with 30s data present");
  assert.match(dom.waiting ?? "", /Waiting for bars for BBB/);
  assert.deepEqual(dom.liveInstances, []);
  assert.equal(dom.canvasCount, 0);
  assert.equal(dom.overlayCount, 0);
  assert.equal(only(await call(page, "engineLog"), "mount").length, 0);

  await call(page, "clearEngineLog");
  await call(page, "pushLiveBar", ["BBB", 60, 0]);
  log = await call(page, "engineLog");
  mounts = only(log, "mount");
  assert.equal(mounts.length, 1, "the first minute bar must mount the 30s chart that was waiting on the DOM gate");
  assert.deepEqual(mounts[0].barSymbols, ["BBB"]);
  assert.equal(mounts[0].barCount, 3, "it mounts with BBB's existing 30s series");
  assertSubMinuteSeries(mounts[0].times, await call(page, "liveBarTimes", ["BBB", 30, 3]), "BBB's 30s series at mount");
});

test("A->B->A: no previous symbol's history ever reaches the engine", async (page) => {
  // Start with data so this test isolates symbol-history leakage from
  // the baseline's separate empty-to-first-data mount failure.
  await call(page, "pushLiveBars", ["AAA", 60, 3]);
  await call(page, "select", ["AAA"]);
  const first = await call(page, "fetchCalls");
  assert.equal(first.length, 1);
  assert.equal(first[0].symbol, "AAA");
  assert.match(first[0].url, /\/bars\/AAA\?minutes=240$/, "the existing 240-minute request is unchanged");

  await call(page, "resolveFetch", [first[0].id, 5]);
  let log = await call(page, "engineLog");
  assert.equal(only(log, "setBars").at(-1).barCount, 8, "history merges with live bars for its own symbol");

  await call(page, "pushLiveBars", ["BBB", 60, 3]);
  await call(page, "clearEngineLog");
  await call(page, "select", ["BBB"]);
  log = await call(page, "engineLog");
  assert.ok(withoutResize(log).length > 0, "the switch must produce engine activity for this to mean anything");
  const leaked = log.filter((e) => e.barSymbols && e.barSymbols.some((s) => s !== "BBB"));
  assert.deepEqual(leaked, [], "no AAA bar may reach the engine on any commit while BBB is selected");
  assert.equal((await call(page, "dom")).headerSymbol, "BBB");

  // Switch back with B's request still in flight, then land the stale
  // response: it belongs to a selection that no longer exists.
  const bCall = (await call(page, "fetchCalls")).find((c) => c.symbol === "BBB");
  assert.ok((await call(page, "pendingFetchIds")).includes(bCall.id));
  await call(page, "select", ["AAA"]);
  await call(page, "clearEngineLog");
  await call(page, "resolveFetch", [bCall.id, 5]);
  log = await call(page, "engineLog");
  assert.deepEqual(
    log.filter((e) => e.barSymbols && e.barSymbols.includes("BBB")),
    [],
    "a response that outlived its selection must not reach the engine",
  );

  const aSecond = (await call(page, "fetchCalls")).filter((c) => c.symbol === "AAA").at(-1);
  await call(page, "resolveFetch", [aSecond.id, 5]);
  log = await call(page, "engineLog");
  const last = only(log, "setBars").at(-1);
  assert.deepEqual(last.barSymbols, ["AAA"], "A's own refreshed history still lands");
  assert.equal(last.barCount, 8);
});

test("same-symbol ticks and corrections: no remount, no refit, still delivered", async (page) => {
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 3]);
  await call(page, "clearEngineLog");

  await call(page, "pushLiveBar", ["AAA", 60, 3]);
  let log = await call(page, "engineLog");
  assert.deepEqual(only(log, "mount"), [], "a live tick must not remount the engine");
  assert.deepEqual(only(log, "destroy"), []);
  assert.deepEqual(only(log, "fitContent"), [], "a live tick must not refit/reset the viewport");
  assert.equal(only(log, "setBars").length, 1);
  assert.equal(only(log, "setBars")[0].barCount, 4);

  await call(page, "clearEngineLog");
  await call(page, "pushLiveBar", ["AAA", 60, 3, { close: 99 }]);
  log = await call(page, "engineLog");
  assert.equal(only(log, "setBars").length, 1, "an in-place correction still reaches the engine");
  assert.equal(only(log, "setBars")[0].barCount, 4, "corrected in place, not appended");
  assert.equal(only(log, "setBars")[0].lastClose, 99);
  assert.deepEqual(only(log, "mount"), []);
  assert.deepEqual(only(log, "fitContent"), []);
});

test("explicit 1m<->5m switch refits without remounting", async (page) => {
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 12]);
  await call(page, "clearEngineLog");

  await clickTimeframe(page, "5m");
  let log = await call(page, "engineLog");
  assert.equal(only(log, "mount").length, 0, "a 1m<->5m switch must not remount the engine");
  assert.equal(only(log, "destroy").length, 0);
  assert.ok(only(log, "fitContent").length >= 1, "an explicit timeframe choice refits");
  let last = only(log, "setBars").at(-1);
  assert.equal(last.barCount, 3);
  assert.ok(last.times.every((t) => t % 300 === 0));

  await call(page, "clearEngineLog");
  await clickTimeframe(page, "1m");
  log = await call(page, "engineLog");
  assert.equal(only(log, "mount").length, 0);
  assert.equal(only(log, "destroy").length, 0);
  assert.ok(only(log, "fitContent").length >= 1);
  last = only(log, "setBars").at(-1);
  assert.equal(last.barCount, 12);
  assert.equal((await call(page, "dom")).activeTimeframe, "1m");
});

test("unrelated symbols produce zero engine calls; the selected symbol still does", async (page) => {
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 3]);
  await call(page, "pushLiveBars", ["CCC", 60, 2]);
  await call(page, "clearEngineLog");

  await call(page, "pushLiveBar", ["CCC", 60, 2]);
  await call(page, "pushLiveBar", ["CCC", 30, 0]);
  let log = await call(page, "engineLog");
  assert.deepEqual(withoutResize(log), [], "another symbol's 1m and 30s traffic must not touch this chart");

  await call(page, "pushLiveBar", ["AAA", 60, 3]);
  log = await call(page, "engineLog");
  assert.equal(only(log, "setBars").length, 1, "the selected symbol's own append still reaches the engine");
  assert.deepEqual(only(log, "setBars")[0].barSymbols, ["AAA"]);
  assert.equal(only(log, "setBars")[0].barCount, 4);

  await call(page, "clearEngineLog");
  await call(page, "pushLiveBar", ["AAA", 60, 3, { close: 77 }]);
  log = await call(page, "engineLog");
  assert.equal(only(log, "setBars").length, 1, "a selected-symbol correction still reaches the engine");
  assert.equal(only(log, "setBars")[0].lastClose, 77);
});

test("four slots: an unrelated symbol touches no engine, an update touches exactly one", async (page) => {
  await call(page, "select", ["AAA"]);
  for (const symbol of ["AAA", "BBB", "CCC", "DDD"]) await call(page, "pushLiveBars", [symbol, 60, 3]);

  await call(page, "clearEngineLog");
  await page.click('.chart-multiview-picker button:text-is("4")');
  await call(page, "settle");

  let log = await call(page, "engineLog");
  const mounts = only(log, "mount");
  assert.equal(mounts.length, 4, "four slots mount four independent engines");
  assert.deepEqual(mounts.map((m) => m.symbolFromDom), ["AAA", "BBB", "CCC", "DDD"]);
  assert.deepEqual(
    mounts.map((m) => m.barSymbols),
    [["AAA"], ["BBB"], ["CCC"], ["DDD"]],
    "each engine mounted on its own slot's bars only",
  );
  assert.equal((await call(page, "dom")).liveInstances.length, 4);

  // EEE is chartable (it has bars) but owns no slot: it sorts after
  // every slot symbol, so the slot assignment is unchanged by its
  // arrival and all four engines should stay completely silent.
  await call(page, "clearEngineLog");
  await call(page, "pushLiveBars", ["EEE", 60, 3]);
  await call(page, "pushLiveBar", ["EEE", 30, 0]);
  log = await call(page, "engineLog");
  assert.deepEqual(withoutResize(log), [], "a fifth, unslotted symbol's 1m and 30s traffic must reach no engine");
  assert.equal((await call(page, "dom")).liveInstances.length, 4, "...and must not mount or destroy anything");

  await call(page, "clearEngineLog");
  await call(page, "pushLiveBar", ["AAA", 60, 3]);
  log = await call(page, "engineLog");
  const setBars = only(log, "setBars");
  assert.equal(setBars.length, 1, "one slot's own append reaches exactly one engine");
  assert.equal(setBars[0].instance, mounts[0].instance, "...its own instance, not a sibling's");
  assert.deepEqual(setBars[0].barSymbols, ["AAA"]);
  assert.equal(setBars[0].barCount, 4);
  assert.deepEqual(only(log, "mount"), [], "a sibling slot must not remount");
  assert.deepEqual(only(log, "destroy"), []);
});

test("toolbar settings survive a symbol remount", async (page) => {
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 3]);

  await page.click('button[aria-label="Chart menu"]');
  await page.click('.chart-switch-row:has-text("MA9") >> button[role="switch"]');
  await page.click('label.chart-radio-row:text-is("Line") >> button[role="radio"]');
  await page.click('.chart-switch-row:has-text("Auto-scale price axis") >> button[role="switch"]');
  await page.keyboard.press("Escape");
  await call(page, "settle");

  let log = await call(page, "engineLog");
  assert.ok(
    only(log, "setChartType").some((e) => e.detail.type === "line"),
    "the real popover controls drove the real handlers",
  );

  await call(page, "pushLiveBars", ["BBB", 60, 3]);
  await call(page, "clearEngineLog");
  await call(page, "select", ["BBB"]);
  log = await call(page, "engineLog");
  const mounts = only(log, "mount");
  assert.equal(mounts.length, 1, "a symbol change mounts exactly one fresh engine");
  assert.deepEqual(mounts[0].barSymbols, ["BBB"]);

  const after = log.filter((e) => e.seq > mounts[0].seq && e.instance === mounts[0].instance);
  const ma9 = after.find((e) => e.event === "series.applyOptions" && e.detail.key === "ma9");
  assert.ok(ma9, "the fresh instance is reconfigured by the mount effect");
  assert.equal(ma9.detail.options.visible, false, "MA9 stays hidden across the remount");
  const rightScale = after.find((e) => e.event === "priceScale.applyOptions" && e.detail.id === "right");
  assert.equal(rightScale.detail.options.autoScale, false, "auto-scale stays off across the remount");
  assert.ok(
    after.some((e) => e.event === "setChartType" && e.detail.type === "line"),
    "chart type stays Line across the remount",
  );
  const fit = after.find((e) => e.event === "series.applyOptions" && e.detail.key === "vwap" && "fitIndicators" in e.detail.options);
  assert.equal(fit.detail.options.fitIndicators, true, "fit-indicators default is re-applied, not lost");
});

test("unmount tears every engine down, and a late response is inert", async (page) => {
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 3]);
  assert.equal((await call(page, "dom")).canvasCount, 1);

  await call(page, "unmount");
  const dom = await call(page, "dom");
  assert.deepEqual(dom.liveInstances, [], "every engine is destroyed on unmount");
  assert.equal(dom.canvasCount, 0, "chart.remove() ran");
  assert.equal(dom.overlayCount, 0, "destroy() ran");

  const log = await call(page, "engineLog");
  assert.equal(only(log, "destroy").length, only(log, "mount").length);
  assert.equal(only(log, "chart.remove").length, only(log, "mount").length);
  assert.equal(only(log, "unwireTooltip").length, only(log, "wireTooltip").length, "no leaked tooltip wiring");

  const pending = await call(page, "pendingFetchIds");
  assert.ok(pending.length >= 1, "a history request was still in flight at unmount");
  await call(page, "resolveFetch", [pending[0], 5]);
  assert.deepEqual((await call(page, "dom")).liveInstances, [], "a response after unmount must not revive anything");
});

test("StrictMode: setup/cleanup balance, one live engine, cancelled request ignored", async (page) => {
  // Mounted with data already present so the engine mount effect itself
  // is what StrictMode double-invokes.
  await call(page, "pushLiveBars", ["AAA", 60, 2]);
  const log = await call(page, "engineLog");
  assert.equal(only(log, "mount").length, 2, "StrictMode double-invokes the mount effect");
  assert.equal(only(log, "destroy").length, 1, "...and the first instance is cleaned up");
  assert.equal(only(log, "chart.remove").length, 1);

  const dom = await call(page, "dom");
  assert.equal(dom.liveInstances.length, 1, "exactly one engine survives");
  assert.equal(dom.canvasCount, 1, "no orphaned canvas after the StrictMode remount");
  assert.equal(dom.overlayCount, 1);
  assert.equal(dom.headerSymbol, "AAA");

  const aCalls = (await call(page, "fetchCalls")).filter((c) => c.symbol === "AAA");
  assert.equal(aCalls.length, 2, "the backfill effect is double-invoked too");

  await call(page, "clearEngineLog");
  await call(page, "resolveFetch", [aCalls[0].id, 5]);
  let after = await call(page, "engineLog");
  assert.deepEqual(only(after, "setBars"), [], "the cancelled StrictMode request must not reach the engine");

  await call(page, "resolveFetch", [aCalls[1].id, 5]);
  after = await call(page, "engineLog");
  const last = only(after, "setBars").at(-1);
  assert.equal(last.barCount, 7, "the live request's history merges normally");
  assert.deepEqual(last.barSymbols, ["AAA"]);
}, { strict: true });

test("zero-height container: mounts, then the engine's ResizeObserver sees the real box", async (page) => {
  // Explicitly a RESIZE test, not a defect claim: the engine owns a
  // ResizeObserver (superChartEngine.ts:540-546) and the component
  // passes `container.clientHeight || undefined`, falling back to the
  // preset height. This records both behaviours rather than asserting a
  // permanent zero-height bug.
  await call(page, "setPanelHeight", [0]);
  await call(page, "select", ["AAA"]);
  await call(page, "pushLiveBars", ["AAA", 60, 3]);

  let log = await call(page, "engineLog");
  const mount = only(log, "mount")[0];
  assert.ok(mount, "a zero-height container still mounts");
  assert.equal(mount.height, 0);
  assert.equal(mount.detail.heightOption, null, "clientHeight 0 falls back to the preset height");

  await call(page, "clearEngineLog");
  await call(page, "setPanelHeight", [520]);
  log = await call(page, "engineLog");
  const resizes = only(log, "resize");
  assert.ok(resizes.length >= 1, "the ResizeObserver fires on the new box");
  assert.ok(resizes.at(-1).height > 0);
  assert.equal(only(log, "mount").length, 0, "a resize must not remount the engine");
  assert.ok((await call(page, "dom")).containerHeight > 0);
});

// ---------------------------------------------------------------------

async function main() {
  if (!fs.existsSync(path.join(DIST, "harness.js"))) {
    throw new Error("harness bundle missing -- run: bun tools/chart-recovery/build.ts");
  }
  const playwrightModule = process.env.PLAYWRIGHT_MODULE;
  const chromePath = process.env.CHROME_PATH;
  if (!playwrightModule) throw new Error("PLAYWRIGHT_MODULE is not set (path to the installed playwright-core module)");
  if (!chromePath) throw new Error("CHROME_PATH is not set (path to the Chromium executable)");
  const { chromium } = require(playwrightModule);

  const buildInfo = JSON.parse(fs.readFileSync(path.join(DIST, "build-info.json"), "utf8"));
  const { server, port } = await startServer();
  const origin = `http://127.0.0.1:${port}`;
  console.log(`source root under test: ${buildInfo.sourceRoot}`);
  console.log(`fixture: ${origin}\n`);

  const results = [];
  let failures = 0;
  let browser = null;
  // Everything from here to the finally has to leave nothing running:
  // a throw out of launch/newContext/newPage (or out of the loop
  // itself) used to leak both the browser process and the listening
  // socket, so the run never exited.
  try {
    browser = await chromium.launch({
      executablePath: chromePath,
      headless: !HEADED,
      args: [
        // Second layer under the route blocking below: nothing but the
        // loopback fixture can even be resolved.
        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
        "--disable-dev-shm-usage",
      ],
    });
    await runTests({ browser, origin, results, onFailure: () => (failures += 1) });
  } finally {
    if (browser) await browser.close().catch(() => {});
    await new Promise((resolve) => server.close(resolve));
  }

  // A --filter typo must not read as a clean run.
  if (results.length === 0) {
    failures += 1;
    console.log(FILTER ? `  FAIL  --filter "${FILTER}" matched no test name` : "  FAIL  no tests are registered");
  }

  const passed = results.filter((r) => r.status === "pass").length;
  const summary = {
    sourceRoot: buildInfo.sourceRoot,
    ranAt: new Date().toISOString(),
    filter: FILTER ?? null,
    total: results.length,
    passed,
    failures,
    results,
  };
  fs.writeFileSync(path.join(DIST, "results.json"), `${JSON.stringify(summary, null, 2)}\n`);
  console.log(`\n${passed}/${results.length} passed (source root: ${buildInfo.sourceRoot})`);
  process.exitCode = failures === 0 ? 0 : 1;
}

async function runTests({ browser, origin, results, onFailure }) {
  for (const { name, fn, options } of tests) {
    if (FILTER && !name.includes(FILTER)) continue;
    const context = await browser.newContext({ viewport: { width: 1100, height: 800 } });
    const blocked = [];
    await context.route("**/*", (route) => {
      const url = route.request().url();
      if (url.startsWith(origin)) return route.continue();
      blocked.push(url);
      return route.abort("blockedbyclient");
    });
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    const pageErrors = [];
    page.on("pageerror", (error) => pageErrors.push(String(error)));
    page.on("console", (message) => {
      // React's development build reports real lifecycle mistakes
      // (updating during render, mismatched snapshots, bad effect
      // cleanup) through console.error, so these are failures rather
      // than noise. ALLOWED_CONSOLE_ERRORS is the escape hatch for a
      // genuinely benign message, and is deliberately empty today --
      // anything added to it has to be justified in the README.
      if (message.type() !== "error") return;
      const text = message.text();
      if (ALLOWED_CONSOLE_ERRORS.some((pattern) => pattern.test(text))) return;
      pageErrors.push(`console.error: ${text}`);
    });

    const started = Date.now();
    try {
      await page.goto(`${origin}/`, { waitUntil: "load" });
      await page.waitForSelector("html[data-harness-ready]");
      await call(page, "mount", [{ strict: options?.strict === true }]);
      await fn(page);
      assert.deepEqual(pageErrors, [], "no page errors");
      assert.deepEqual(blocked, [], "no request left the loopback fixture");
      results.push({ name, status: "pass", ms: Date.now() - started });
      console.log(`  PASS  ${name}`);
    } catch (error) {
      onFailure();
      results.push({ name, status: "fail", ms: Date.now() - started, error: error && error.message ? error.message : String(error) });
      console.log(`  FAIL  ${name}`);
      console.log(`        ${(error && error.message ? error.message : String(error)).split("\n").join("\n        ")}`);
      if (pageErrors.length > 0) console.log(`        page errors: ${pageErrors.join(" | ")}`);
    } finally {
      // A context that will not close must not abort the remaining
      // tests, and must not stop the browser/server teardown either.
      await context.close().catch(() => {});
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
