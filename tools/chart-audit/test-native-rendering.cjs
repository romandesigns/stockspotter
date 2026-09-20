// Offline HTML-engine verification; this is not a physical Android/iOS bridge test.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE);
const fs = require('node:fs');
const assert = require('node:assert/strict');
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH, headless: true });
  const results = {};
  const script = fs.readFileSync('apps/client/node_modules/lightweight-charts/dist/lightweight-charts.standalone.production.js', 'utf8');
  for (const variant of ['before', 'after']) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2 });
    await context.route('**/*', r => r.request().url().startsWith('https://unpkg.com/lightweight-charts@4.1.3/')
      ? r.fulfill({ contentType: 'application/javascript', body: script }) : r.abort());
    const page = await context.newPage();
    const errors = []; page.on('pageerror', e => errors.push(e.message));
    const cdp = await context.newCDPSession(page);
    await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
    await page.setContent(fs.readFileSync(`data/native-chart-fixture/${variant}.html`, 'utf8'));
    await page.waitForFunction(() => window.__setBars);
    results[variant] = await page.evaluate(async () => {
      const base = 1789718400;
      const bars = Array.from({ length: 500 }, (_, i) => ({ time: base + i * 60, open: 100, high: 101, low: 99, close: 100 + Math.sin(i) * .1, volume: 100 }));
      const raf = () => new Promise(r => requestAnimationFrame(r));
      const series = () => [candles, area, vol, ma9, ma20, vwapSeries, bbUpper, bbLower, macdHist, macdLine, macdSignal, rsiSeries];
      const data = () => series().map(s => s.data());
      const cases = [bars, bars.map((b, i) => i === 499 ? { ...b, high: 104, close: 103, volume: 125 } : b)];
      cases.push([...cases[1], { ...bars[499], time: base + 500 * 60, close: 100, volume: 5 }]);
      cases.push(cases[2].map((b, i) => i === 200 ? { ...b, low: 98, close: 99 } : b));
      cases.push(cases[3].slice(1)); cases.push(bars.filter((_, i) => i % 5 === 0));
      const snapshots = [];
      for (const input of cases) { window.__setBars(input, 'TEST:1m'); snapshots.push(data()); }
      window.__setBars(bars, 'TEST:1m');
      const durations = [];
      for (let i = 0; i < 120; i++) {
        const input = bars.map((b, j) => j === 499 ? { ...b, close: 100 + i / 10000, volume: 100 + i } : b);
        const start = performance.now(); window.__setBars(input, 'TEST:1m');
        if (i >= 20) durations.push(performance.now() - start);
        await raf();
      }
      chart.timeScale().setVisibleLogicalRange({ from: 100, to: 200 }); await raf();
      window.__setBars(bars, 'TEST:1m'); await raf();
      const liveRange = chart.timeScale().getVisibleLogicalRange();
      window.__setBars(bars.filter((_, i) => i % 5 === 0), 'TEST:5m'); await raf();
      const changedRange = chart.timeScale().getVisibleLogicalRange();
      window.__setBars([], 'TEST:30s');
      const cleared = series().every(s => s.data().length === 0);
      window.__setBars(bars, 'TEST:30s'); await raf();
      durations.sort((a, b) => a - b);
      return { snapshots, liveRange, changedRange, cleared, timing: { n: durations.length, p50: durations[49], p95: durations[94], p99: durations[98], max: durations[99] } };
    });
    for (const viewport of [{ width: 720, height: 800 }, { width: 360, height: 780 }]) {
      await page.setViewportSize(viewport);
      await page.waitForTimeout(60);
      assert.equal(await page.evaluate(() => chart.options().width), viewport.width);
    }
    assert.deepEqual(errors, []);
    await context.close();
  }
  assert.deepEqual(results.after.snapshots, results.before.snapshots);
  assert.deepEqual(results.after.liveRange, { from: 100, to: 200 });
  assert.notDeepEqual(results.after.changedRange, { from: 100, to: 200 });
  assert.equal(results.after.cleared, true);
  for (const result of Object.values(results)) delete result.snapshots;
  const summary = { parityCases: 6, series: 12, foldResizeChecks: 4, results };
  fs.writeFileSync('data/native-chart-fixture/results.json', JSON.stringify(summary, null, 2));
  console.log(JSON.stringify(summary));
  await browser.close();
})().catch(error => { console.error(error); process.exit(1); });
