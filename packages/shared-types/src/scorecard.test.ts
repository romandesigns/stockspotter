import { describe, expect, test } from "bun:test";
import {
  decodeScorecardDay,
  decodeScorecardDays,
  decodeScorecardLatest,
  differenceText,
  fetchScorecard,
  formatGeneratedAt,
  formatMarketDay,
  formatNetPct,
  formatPoints,
  formatRate,
  methodText,
  notRecordedText,
  panelTrend,
  primaryStats,
  sampleText,
  samplingText,
  scorecardRows,
  sourceText,
  trendText,
  verdictRules,
  verdictView,
  windowText,
  type ScorecardDay,
} from "./scorecard";

// Shapes copied from a real latest.json produced from 2026-10-15's data.
function horizon(h: string, diff: number, lo: number, hi: number) {
  return { horizon: h, n: 536, meanNetPct: -1.8534, medianNetPct: -0.7322, hitRate: 0.278, baselineMeanNetPct: -0.3317, diffPct: diff, meanCi: { lo: -3.08, hi: -0.9 }, diffCi: { lo, hi } };
}
function panel(name: string, extra: Record<string, unknown> = {}) {
  return {
    panel: name, detections: 2275, scored: 583, symbols: 95, sessions: 1, symbolDays: 95, quoteCostRows: 0, proxyCostRows: 583,
    primaryHorizon: "30m",
    horizons: [horizon("5m", -0.4188, -0.7271, -0.1396), horizon("30m", -1.5217, -2.6347, -0.667)],
    targetFirstRate: 0.1355, stopFirstRate: 0.4786, baselineTargetFirstRate: 0.2098, medianMfe30Pct: 0.97, medianMae30Pct: -2.2,
    verdict: "insufficient data", verdictReason: "n=536 over 1 market day(s); a verdict needs n>=30 over >=3 days",
    skipped: { afterClose: 240, notSampled: 1440 },
    ...extra,
  };
}
const method = {
  horizons: ["5m", "15m", "30m", "60m", "close"], primaryHorizon: "30m", targetPct: 10, stopPct: 5, kRandom: 20, ciLevel: 0.95,
  minDetections: 30, minSessions: 3, rollingSessions: 20, roundTripCostProxyPct: 0.5, notes: ["a note"],
};
const sampling = {
  sampled: true, maxSymbols: 200, symbolsAvailable: 3328, symbolsScored: 200, detectionsAvailable: 37163, detectionsKept: 3814, method: "symbol cap", seed: 1, perPanel: [],
};
const latestRaw = {
  schema: "panel-scorecard-v1",
  generatedAt: "2026-10-16T12:52:52.180329400Z",
  method,
  rolling: { maxSessions: 20, days: ["2026-10-15", "2026-09-18"], detectionsScored: 3096, panels: [panel("momentum"), panel("micropullback"), panel("gap_and_go")] },
  latest: { day: "2026-10-15", source: { kind: "live-pending-fallback", file: "pending-20261015.jsonl", note: "fallback" }, sampling, panels: [panel("momentum")] },
};

describe("decoding", () => {
  test("decodes a real-shaped latest.json and sorts rolling days oldest first", () => {
    const d = decodeScorecardLatest(latestRaw);
    expect(d).not.toBeNull();
    expect(d!.rolling.days).toEqual(["2026-09-18", "2026-10-15"]);
    expect(d!.rolling.panels[0].horizons[1].diffCi).toEqual({ lo: -2.6347, hi: -0.667 });
    expect(d!.latest.sampling?.sampled).toBe(true);
    expect(d!.method.kRandom).toBe(20);
  });

  test("rejects a different schema or a missing panels array", () => {
    expect(decodeScorecardLatest({ ...latestRaw, schema: "panel-scorecard-v2" })).toBeNull();
    expect(decodeScorecardLatest({ ...latestRaw, rolling: { days: [] } })).toBeNull();
    expect(decodeScorecardLatest(null)).toBeNull();
  });

  test("tolerates null numbers and an unknown verdict", () => {
    const d = decodeScorecardLatest({
      ...latestRaw,
      rolling: { ...latestRaw.rolling, panels: [panel("ignition", { verdict: "great", targetFirstRate: null, horizons: [{ horizon: "30m", n: 0, diffPct: null, diffCi: null }] })] },
    })!;
    const p = d.rolling.panels[0];
    expect(p.verdict).toBe("insufficient data");
    expect(p.targetFirstRate).toBeNull();
    expect(primaryStats(p)?.diffCi).toBeNull();
  });

  test("decodes a day file", () => {
    const day = decodeScorecardDay({ schema: "panel-scorecard-v1", generatedAt: "x", day: "2026-10-15", method, source: { kind: "panel-detections" }, sampling: { sampled: false }, panels: [panel("momentum")], rows: [] });
    expect(day?.day).toBe("2026-10-15");
    expect(day?.sampling?.sampled).toBe(false);
    expect(decodeScorecardDay({ schema: "panel-scorecard-v1", panels: [] })).toBeNull();
  });

  test("days list accepts several shapes, newest first, de-duplicated", () => {
    expect(decodeScorecardDays(["2026-09-18", "2026-10-15", "bad", "2026-10-15"])).toEqual(["2026-10-15", "2026-09-18"]);
    expect(decodeScorecardDays({ days: [{ day: "2026-10-14" }, { day: "2026-10-15", generatedAt: "x" }] })).toEqual(["2026-10-15", "2026-10-14"]);
    expect(decodeScorecardDays({})).toBeNull();
  });
});

describe("fetchScorecard", () => {
  const ok = (body: unknown, status = 200) => async () => new Response(JSON.stringify(body), { status });
  test("404 means the server has no scorecard yet", async () => {
    expect(await fetchScorecard(ok("nope", 404), "u", decodeScorecardLatest)).toEqual({ kind: "unavailable" });
  });
  test("other failures are errors with a reason", async () => {
    expect(await fetchScorecard(ok("x", 500), "u", decodeScorecardLatest)).toEqual({ kind: "error", message: "HTTP 500" });
    expect(await fetchScorecard(ok({ schema: "other" }), "u", decodeScorecardLatest)).toEqual({ kind: "error", message: "unrecognised response" });
    const boom = async (): Promise<Response> => { throw new Error("offline"); };
    expect(await fetchScorecard(boom, "u", decodeScorecardLatest)).toEqual({ kind: "error", message: "offline" });
  });
  test("decodes a good body", async () => {
    const r = await fetchScorecard(ok(latestRaw), "u", decodeScorecardLatest);
    expect(r.kind).toBe("ok");
  });
});

describe("ordering", () => {
  test("fixed dashboard order, detectors under other, missing panels kept with null stats", () => {
    const d = decodeScorecardLatest(latestRaw)!;
    const rows = scorecardRows(d.rolling.panels);
    expect(rows.map((r) => r.key)).toEqual([
      "halt_speed", "momentum", "gap_and_go", "ignition", "top_gainers", "highly_trading", "catalyst", "micropullback", "consolidation_breakout",
    ]);
    expect(rows.find((r) => r.key === "halt_speed")?.stats).toBeNull();
    expect(rows.find((r) => r.key === "momentum")?.stats?.panel).toBe("momentum");
    expect(rows.filter((r) => r.group === "other").map((r) => r.key)).toEqual(["micropullback", "consolidation_breakout"]);
  });

  test("exchange halts appear only when scored; unknown panels go last", () => {
    const d = decodeScorecardLatest({ ...latestRaw, rolling: { ...latestRaw.rolling, panels: [panel("halt_actual"), panel("new_thing")] } })!;
    const rows = scorecardRows(d.rolling.panels);
    expect(rows.slice(-2).map((r) => [r.key, r.group])).toEqual([["halt_actual", "other"], ["new_thing", "other"]]);
    expect(rows.at(-1)?.title).toBe("new thing");
  });
});

describe("wording", () => {
  test("verdicts are always words, with a tone", () => {
    expect(verdictView("beats random")).toEqual({ label: "Beats random", tone: "good" });
    expect(verdictView("worse than random")).toEqual({ label: "Worse than random", tone: "critical" });
    expect(verdictView("no difference shown")).toEqual({ label: "No difference shown", tone: "neutral" });
    expect(verdictView("insufficient data")).toEqual({ label: "Insufficient data", tone: "muted" });
  });

  test("numbers use a real minus sign and sensible precision", () => {
    expect(formatPoints(-1.5217)).toBe("−1.5");
    expect(formatPoints(-0.0224)).toBe("−0.02");
    expect(formatPoints(-0.001)).toBe("0.00");
    expect(formatNetPct(-1.8534)).toBe("−1.9%");
    expect(formatNetPct(0.4)).toBe("+0.40%");
    expect(formatNetPct(null)).toBe("—");
    expect(formatRate(0.278)).toBe("28%");
    expect(formatRate(null)).toBe("—");
  });

  test("difference sentence with its range", () => {
    const p = decodeScorecardLatest(latestRaw)!.rolling.panels[0];
    expect(differenceText(primaryStats(p))).toBe("−1.5 pts vs random, range −2.63 to −0.67");
    expect(differenceText({ ...primaryStats(p)!, diffPct: -1.5, diffCi: { lo: -3.588, hi: 0.961 } })).toBe("−1.5 pts vs random, range −3.59 to 0.96");
    expect(differenceText({ ...primaryStats(p)!, diffCi: null })).toBe("−1.5 pts vs random, no range yet");
    expect(differenceText(null)).toBe("No comparison with random yet");
    expect(sampleText(p)).toBe("536 detections over 1 day");
  });

  test("window, dates and method", () => {
    expect(windowText(["2026-10-15", "2026-09-18"])).toBe("Last 2 sessions: Sep 18 – Oct 15");
    expect(windowText(["2026-10-15"])).toBe("Last 1 session: Oct 15");
    expect(windowText([])).toBe("No scored sessions yet");
    expect(formatMarketDay("2026-10-16", true)).toBe("Oct 16, 2026");
    expect(formatGeneratedAt("2026-10-16T12:52:52.180329400Z")).toBe("Oct 16, 8:52 AM ET");
    expect(formatGeneratedAt("")).toBe("—");
    const m = decodeScorecardLatest(latestRaw)!.method;
    expect(methodText(m)).toBe("30-min return after costs vs 20 random moments in the same stock and session");
    expect(verdictRules(m).join(" ")).toContain("fewer than 30 detections, or fewer than 3 market days");
    expect(notRecordedText()).toBe("Not recorded yet — recording started Oct 16, 2026");
  });

  test("sampling and source disclosures", () => {
    const d = decodeScorecardLatest(latestRaw)!;
    expect(samplingText(d.latest.sampling, d.latest.day)).toContain("Sampled on Oct 15: 200 of 3,328 stocks scored (3,814 of 37,163 detections)");
    expect(samplingText({ ...d.latest.sampling!, sampled: false })).toBeNull();
    expect(samplingText(null)).toBeNull();
    expect(sourceText(d.latest.source)).toContain("older live-signals log");
    expect(sourceText({ kind: "panel-detections", file: null, note: "" })).toBeNull();
  });
});

describe("trend", () => {
  const day = (d: string, diff: number | null): ScorecardDay =>
    decodeScorecardDay({ schema: "panel-scorecard-v1", generatedAt: "", day: d, method, source: {}, sampling: null, panels: diff === null ? [] : [panel("momentum", { horizons: [horizon("30m", diff, -1, 1)] })] })!;

  test("one point per day, oldest first, null when not scored", () => {
    const pts = panelTrend([day("2026-10-15", -1.52), day("2026-10-13", 0.4), day("2026-10-14", null)], "momentum");
    expect(pts.map((p) => [p.day, p.diffPct])).toEqual([["2026-10-13", 0.4], ["2026-10-14", null], ["2026-10-15", -1.52]]);
    expect(trendText(pts)).toBe("Difference per day: Oct 13 +0.40, Oct 15 −1.5");
    expect(trendText([])).toBe("No per-day difference yet");
  });
});
