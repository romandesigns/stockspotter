// Bounded, opt-in latency diagnostics.
//
// The 2026-09-21 chart audit could measure exchange timestamp -> backend
// (p50 13.1ms, p90 20.7ms, p99 82.7ms) from server-side capture, but
// publication -> client and client -> rendered candle were unmeasurable, and
// the attempt to infer transport latency from event timestamps produced
// nonsense: momentum_update implied a p50 of 23 minutes and halt_warning a
// maximum of 6.9 hours, because those fields describe market moments and get
// re-broadcast long afterwards.
//
// Two things close that gap. The server's `sentAt` (added on the
// chart-fidelity port branch) says when a frame was prepared for
// transmission. This module records it against local receipt, and records
// receipt against the chart write.
//
// # Off by default, and cheap when off
//
// Enabled only by an explicit opt-in, so the hot path costs one boolean read
// per message when disabled. Nothing is logged: samples go into fixed-size
// ring buffers and are read on demand. There is no network egress and no
// console output, so this cannot spam production.
//
//   localStorage.setItem("stockspotter.latency", "1"); location.reload()
//   window.__ssLatency()            // percentiles for every stage
//   window.__ssLatencyReset()
//
// # What each stage means, and what it does not
//
//   C  publication -> client receipt   sentAt -> message handler entry
//   D  client receipt -> series update newest-bar receipt -> chart write done
//   E  C + D, i.e. observable publication -> series update
//
// Stage E is NOT render-to-photon latency. It ends when the chart library has
// been handed the data and the write call has returned -- before layout,
// compositing, GPU work and scan-out. Do not quote it as what the user sees.
//
// Stage C spans two machines, so it is only as meaningful as the clock offset
// between them. The client cannot know that offset; `welcome.serverTime`
// gives a one-shot estimate at handshake and is recorded here for context so
// a reader can judge the numbers rather than trust them.

const RING = 512;

export type LatencyStage = "publication_to_client" | "receipt_to_series";

interface Ring {
  values: Float64Array;
  count: number;
  next: number;
}

function ring(): Ring {
  return { values: new Float64Array(RING), count: 0, next: 0 };
}

const rings: Record<LatencyStage, Ring> = {
  publication_to_client: ring(),
  receipt_to_series: ring(),
};

let enabled = false;
let clockOffsetMs: number | null = null;
let skipped = 0;

try {
  enabled = globalThis.localStorage?.getItem("stockspotter.latency") === "1";
} catch {
  // Private mode or blocked site data. Diagnostics stay off; never throw.
  enabled = false;
}

export function latencyEnabled(): boolean {
  return enabled;
}

/** One-shot clock-offset estimate from the handshake, for context only. */
export function recordClockOffset(serverTimeIso: string, receivedAtMs: number): void {
  if (!enabled) return;
  const t = Date.parse(serverTimeIso);
  if (Number.isFinite(t)) clockOffsetMs = receivedAtMs - t;
}

export function record(stage: LatencyStage, ms: number): void {
  if (!enabled) return;
  // A negative sample means the clocks disagree by more than the latency, so
  // the number is not a measurement of anything. Counted, not silently
  // dropped, because a large skipped count is itself the finding.
  if (!Number.isFinite(ms) || ms < 0) {
    skipped++;
    return;
  }
  const r = rings[stage];
  r.values[r.next] = ms;
  r.next = (r.next + 1) % RING;
  if (r.count < RING) r.count++;
}

/** Records publication -> receipt from an envelope that carries `sentAt`. */
export function recordFromEnvelope(sentAt: string | undefined, receivedAtMs: number): void {
  if (!enabled || !sentAt) return;
  const t = Date.parse(sentAt);
  if (Number.isFinite(t)) record("publication_to_client", receivedAtMs - t);
}

export interface StageStats {
  n: number;
  p50: number;
  p90: number;
  p95: number;
  p99: number;
  max: number;
  min: number;
}

function stats(r: Ring): StageStats | null {
  if (r.count === 0) return null;
  const a = Array.from(r.values.subarray(0, r.count)).sort((x, y) => x - y);
  const q = (p: number) => a[Math.min(Math.floor(a.length * p), a.length - 1)];
  return {
    n: a.length,
    p50: +q(0.5).toFixed(1),
    p90: +q(0.9).toFixed(1),
    p95: +q(0.95).toFixed(1),
    p99: +q(0.99).toFixed(1),
    max: +a[a.length - 1].toFixed(1),
    min: +a[0].toFixed(1),
  };
}

export interface LatencyReport {
  enabled: boolean;
  /** Samples discarded as non-measurements (negative, i.e. clock skew). */
  skipped: number;
  clockOffsetMs: number | null;
  ringSize: number;
  stages: Record<LatencyStage, StageStats | null>;
  /** C + D at the percentile level, which is an upper bound on the true
   *  combined distribution rather than the distribution itself -- the two
   *  stages are sampled independently and their percentiles do not
   *  necessarily come from the same messages. Labelled so it cannot be
   *  mistaken for a measured end-to-end figure. */
  publicationToSeriesUpperBound: StageStats | null;
  note: string;
}

export function latencyReport(): LatencyReport {
  const c = stats(rings.publication_to_client);
  const d = stats(rings.receipt_to_series);
  const sum =
    c && d
      ? {
          n: Math.min(c.n, d.n),
          p50: +(c.p50 + d.p50).toFixed(1),
          p90: +(c.p90 + d.p90).toFixed(1),
          p95: +(c.p95 + d.p95).toFixed(1),
          p99: +(c.p99 + d.p99).toFixed(1),
          max: +(c.max + d.max).toFixed(1),
          min: +(c.min + d.min).toFixed(1),
        }
      : null;
  return {
    enabled,
    skipped,
    clockOffsetMs,
    ringSize: RING,
    stages: { publication_to_client: c, receipt_to_series: d },
    publicationToSeriesUpperBound: sum,
    note:
      "receipt_to_series ends when the chart write returns, BEFORE layout/compositing/GPU. " +
      "Not render-to-photon. publication_to_client spans two clocks; read clockOffsetMs first.",
  };
}

// Receipt marks for stage D. Bounded by tracked-symbol count, and only
// populated when diagnostics are on. Keyed by symbol+interval so the 30s and
// 1m streams do not overwrite each other's mark.
const receiptMarks = new Map<string, number>();
const MAX_MARKS = 1024;

/** Called from the socket message handler the instant a bar is parsed. */
export function markBarReceipt(symbol: string, intervalSecs: number, atMs: number): void {
  if (!enabled) return;
  if (receiptMarks.size >= MAX_MARKS) receiptMarks.clear();
  receiptMarks.set(`${symbol}:${intervalSecs}`, atMs);
}

/** Called once the chart has actually been written, closing stage D. */
export function completeSeriesWrite(symbol: string, intervalSecs: number, atMs: number): void {
  if (!enabled) return;
  const key = `${symbol}:${intervalSecs}`;
  const mark = receiptMarks.get(key);
  if (mark === undefined) return;
  receiptMarks.delete(key);
  record("receipt_to_series", atMs - mark);
}

export function latencyReset(): void {
  receiptMarks.clear();
  rings.publication_to_client = ring();
  rings.receipt_to_series = ring();
  skipped = 0;
}

// Diagnostic surface. Attached only when opted in, so production windows are
// untouched.
if (enabled) {
  const w = globalThis as unknown as Record<string, unknown>;
  w.__ssLatency = latencyReport;
  w.__ssLatencyReset = latencyReset;
}
