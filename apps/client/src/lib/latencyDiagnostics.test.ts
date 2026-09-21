import { test, expect } from "bun:test";
import {
  completeSeriesWrite,
  latencyEnabled,
  latencyReport,
  latencyReset,
  markBarReceipt,
  record,
  recordClockOffset,
  recordFromEnvelope,
} from "./latencyDiagnostics";

// The opt-in is read once at module load from localStorage, which is absent
// in this runner, so these tests exercise the DISABLED path -- which is the
// one that matters for production, since that is how it ships.

test("diagnostics are off unless explicitly opted in", () => {
  expect(latencyEnabled()).toBe(false);
  const r = latencyReport();
  expect(r.enabled).toBe(false);
  expect(r.stages.publication_to_client).toBeNull();
  expect(r.stages.receipt_to_series).toBeNull();
  expect(r.publicationToSeriesUpperBound).toBeNull();
});

test("every entry point is inert and total when disabled", () => {
  // None of these may throw, allocate unboundedly, or record anything --
  // they sit on the socket hot path at hundreds of messages a second.
  expect(() => {
    record("publication_to_client", 12.3);
    recordFromEnvelope("2026-09-21T15:37:00.000000Z", Date.now());
    recordFromEnvelope(undefined, Date.now());
    recordClockOffset("2026-09-21T15:37:00.000000Z", Date.now());
    markBarReceipt("DDC", 60, performance.now());
    completeSeriesWrite("DDC", 60, performance.now());
    completeSeriesWrite("NEVER_MARKED", 30, performance.now());
    latencyReset();
  }).not.toThrow();
  const r = latencyReport();
  expect(r.stages.publication_to_client).toBeNull();
  expect(r.skipped).toBe(0);
  expect(r.clockOffsetMs).toBeNull();
});

test("no diagnostic surface is attached to the global when disabled", () => {
  const w = globalThis as unknown as Record<string, unknown>;
  expect(w.__ssLatency).toBeUndefined();
  expect(w.__ssLatencyReset).toBeUndefined();
});

test("the report states its own limits rather than leaving them implied", () => {
  const note = latencyReport().note;
  // Stage E must never be read as render-to-photon, and stage C is
  // cross-clock. Both caveats travel with the numbers.
  expect(note).toContain("Not render-to-photon");
  expect(note).toContain("two clocks");
  expect(latencyReport().ringSize).toBe(512);
});

test("a malformed or absent sentAt is ignored, not recorded as a sample", () => {
  expect(() => {
    recordFromEnvelope("not-a-date", Date.now());
    recordFromEnvelope("", Date.now());
  }).not.toThrow();
  expect(latencyReport().stages.publication_to_client).toBeNull();
});
