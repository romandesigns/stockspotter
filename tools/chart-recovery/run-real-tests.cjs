"use strict";
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert/strict");
const { chromium } = require("playwright-core");
const dist = path.join(__dirname, "dist-real");
assert.equal(JSON.parse(fs.readFileSync(path.join(dist, "build-info.json"))).realEngine, true);
const server = http.createServer((req, res) => {
  const name = req.url === "/" ? "index.html" : path.basename(req.url);
  const file = path.join(dist, name);
  if (!fs.existsSync(file)) { res.writeHead(404); res.end(); return; }
  res.setHeader("Content-Type", name.endsWith(".js") ? "text/javascript" : "text/html");
  res.end(fs.readFileSync(file));
});
(async () => {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const browser = await chromium.launch({ headless: true, channel: "chromium-headless-shell" });
  try {
    const page = await browser.newPage({ viewport: { width: 1000, height: 800 } });
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    const origin = `http://127.0.0.1:${server.address().port}`;
    await page.route("**/*", (route) => route.request().url().startsWith(origin) ? route.continue() : route.abort());
    await page.goto(origin);
    await page.waitForSelector("html[data-harness-ready]");
    const call = (method, ...args) => page.evaluate(([m, a]) => window.harness[m](...a), [method, args]);
    await call("mount");
    await call("pushLiveBar", "AAA", 60, 200);
    await call("pushLiveBar", "AAA", 60, 201);
    await call("select", "AAA");
    const request = (await call("fetchCalls"))[0];
    await call("resolveFetch", request.id, 200);
    await page.waitForTimeout(150);
    const stats = await page.evaluate(() => ({ range: window.realChartState.api.chart.timeScale().getVisibleLogicalRange(), count: window.realChartState.barCount, volume: window.realChartState.api.series.volume.options() }));

    assert.ok(stats.range.from < 5 && stats.range.to >= stats.count - 1 && stats.range.to - stats.range.from > 190, "real viewport includes delayed history");
    assert.equal(stats.volume.lastValueVisible, false);
    assert.equal(stats.volume.priceLineVisible, false);
    await page.evaluate(() => window.realChartState.api.chart.timeScale().setVisibleLogicalRange({ from: 50, to: 100 }));
    await page.waitForTimeout(150);
    const range = await page.evaluate(() => window.realChartState.api.chart.timeScale().getVisibleLogicalRange());
    await call("pushLiveBar", "AAA", 60, 202);
    await page.waitForTimeout(150);
    const after = await page.evaluate(() => window.realChartState.api.chart.timeScale().getVisibleLogicalRange());
    assert.deepEqual(after, range, "an ordinary tick does not refit the real chart");
    assert.ok(await page.locator("canvas").count() > 0, "real chart canvases are mounted");
    assert.deepEqual(errors, []);
    const screenshot = process.env.DASHBOARD_SCREENSHOT;
    if (screenshot) await page.screenshot({ path: screenshot, fullPage: true });
    fs.writeFileSync(path.join(dist, "results.json"), JSON.stringify({ status: "pass", actualEngine: true, stats }, null, 2));
    console.log("PASS real chart: delayed history visible, ordinary tick preserves viewport, volume has no axis label/price line");
  } finally { await browser.close(); }
})().catch((e) => { console.error(e); process.exitCode = 1; }).finally(() => server.close());





