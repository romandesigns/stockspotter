import { describe, expect, test } from "bun:test";
import { bandTrackRecordLine, decodeFunds, decodeMomentumTrackRecord, legendTrackRecordLine } from "./momentumTrackRecord";

const raw = {
  measured: true,
  generatedAt: "2026-10-15T00:00:00Z",
  targetPct: 10,
  stopPct: 5,
  minSample: 30,
  sourceNote: "fixture",
  bands: [
    { band: "Mild", range: "0.60–0.65", n: 340, sessions: 40, targetRate: 0.12, stopFirstRate: 0.7, medianCloseReturnNetPct: -3.24, medianMaxAdversePct: -6 },
    { band: "Solid", range: "0.65–0.70", n: 120, sessions: 30, targetRate: 0.18, stopFirstRate: 0.64, medianCloseReturnNetPct: 1.25 },
    { band: "Strong", range: "0.70+", n: 14, sessions: 9 },
    { band: "Bogus", n: 5 },
  ],
};

describe("decodeMomentumTrackRecord", () => {
  test("keeps known bands, drops the rest", () => {
    const r = decodeMomentumTrackRecord(raw)!;
    expect(r.bands.map((b) => b.band)).toEqual(["Mild", "Solid", "Strong"]);
    expect(r.bands[2].targetRate).toBeUndefined();
  });
  test("unrecognised bodies decode to null", () => {
    expect(decodeMomentumTrackRecord(null)).toBeNull();
    expect(decodeMomentumTrackRecord({ measured: true })).toBeNull();
    expect(decodeMomentumTrackRecord([])).toBeNull();
  });
});

describe("track record lines", () => {
  const r = decodeMomentumTrackRecord(raw)!;
  test("band with enough cases states its rates", () => {
    expect(bandTrackRecordLine(r, "Mild")).toBe("Past Mild crossings: 12% reached +10% first · 70% hit −5% first · median −3.2% at close (n=340)");
    expect(bandTrackRecordLine(r, "Solid")).toBe("Past Solid crossings: 18% reached +10% first · 64% hit −5% first · median +1.3% at close (n=120)");
  });
  test("too few cases states only the count", () => {
    expect(bandTrackRecordLine(r, "Strong")).toBe("Only 14 past Strong cases — too few to state a rate");
    expect(bandTrackRecordLine({ ...r, bands: [] }, "Strong")).toBe("Only 0 past Strong cases — too few to state a rate");
  });
  test("not measured", () => {
    expect(bandTrackRecordLine(null, "Mild")).toBe("Track record not measured yet");
    expect(bandTrackRecordLine({ ...r, measured: false }, "Mild")).toBe("Track record not measured yet");
    expect(legendTrackRecordLine(null, ["Mild"])).toBe("Track record not measured yet");
  });
  test("legend line", () => {
    expect(legendTrackRecordLine(r, ["Mild", "Solid", "Strong"])).toBe("Past crossings that reached +10% before −5%: Mild 12% (n=340) · Solid 18% (n=120) · Strong too few (n=14)");
  });
});

test("decodeFunds", () => {
  expect(decodeFunds(["NVD", "NVDY"])).toEqual(["NVD", "NVDY"]);
  expect(decodeFunds([1])).toBeNull();
  expect(decodeFunds({})).toBeNull();
});
