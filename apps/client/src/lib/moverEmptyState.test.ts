import { expect, test } from "bun:test";
import { moverEmptyState } from "./moverEmptyState";
import type { TodayMovers } from "./useMovers";

const base: TodayMovers = { gainers: [], mostActive: [], peakGainers: [], peakMostActive: [], currentAvailable: true, lastUpdated: null };

test("failure outranks cached success or refreshing, in both views", () => {
  for (const peak of [false, true]) {
    for (const refreshStatus of [undefined, "success", "refreshing", "failed"] as const) {
      expect(moverEmptyState({ ...base, refreshStatus, clientFetchFailed: true }, peak)).toBe("Couldn't retrieve the mover snapshot. Retrying…");
    }
    expect(moverEmptyState({ ...base, refreshStatus: "failed", lastUpdated: new Date() }, peak)).toBe("The mover refresh failed. Retrying…");
    expect(moverEmptyState({ ...base, refreshStatus: "refreshing" }, peak)).toBe("Refreshing the mover snapshot…");
  }
});

test("explicit success describes only selected-list emptiness, without market inference", () => {
  expect(moverEmptyState({ ...base, refreshStatus: "success" }, false)).toBe("No readings available for this list in the latest snapshot.");
  expect(moverEmptyState({ ...base, refreshStatus: "success" }, true)).toBe("No retained peak readings in this snapshot.");
});

test("missing current arrays remain unavailable even when a peak refresh succeeds", () => {
  expect(moverEmptyState({ ...base, currentAvailable: false, refreshStatus: "success" }, false)).toBe("Current snapshot unavailable. Peak shows retained observations.");
});

test("legacy timestamps and absent or unrecognized status cannot imply success", () => {
  for (const peak of [false, true]) {
    for (const lastUpdated of [null, new Date()]) {
      expect(moverEmptyState({ ...base, lastUpdated }, peak)).toBe("Mover snapshot status is unavailable.");
      expect(moverEmptyState({ ...base, lastUpdated, refreshStatus: "unexpected" as unknown as TodayMovers["refreshStatus"] }, peak)).toBe("Mover snapshot status is unavailable.");
    }
  }
});

