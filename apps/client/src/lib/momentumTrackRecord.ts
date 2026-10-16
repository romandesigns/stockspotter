// How past Bullish Momentum crossings turned out, by strength band --
// ws-server's GET /momentum/track-record (backtest-metrics base_rates
// observe_momentum): bought at the ask once the crossing minute closed,
// +targetPct first vs -stopPct first, close valued at the bid. Rates are
// present only when a band has at least minSample cases; below that there
// is a count and nothing more. Also decodes GET /universe/funds (the
// ETF/ETN/fund symbols the panel leaves out). Decoders return null for an
// unrecognised body so the caller keeps its last good value.

import type { MomentumBand } from "./momentumChart";

export interface MomentumBandRecord {
  band: MomentumBand;
  range: string;
  n: number;
  sessions: number;
  targetRate?: number;
  stopFirstRate?: number;
  medianCloseReturnNetPct?: number;
  medianMaxAdversePct?: number;
}

export interface MomentumTrackRecord {
  measured: boolean;
  generatedAt?: string;
  targetPct: number;
  stopPct: number;
  minSample: number;
  sourceNote: string;
  bands: MomentumBandRecord[];
}

const BANDS: ReadonlySet<string> = new Set(["Mild", "Solid", "Strong"]);
const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
const optNum = (v: unknown): number | undefined => (isNum(v) ? v : undefined);

export function decodeMomentumTrackRecord(raw: unknown): MomentumTrackRecord | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.measured !== "boolean" || !isNum(r.targetPct) || !isNum(r.stopPct) || !isNum(r.minSample) || !Array.isArray(r.bands)) return null;
  const bands: MomentumBandRecord[] = [];
  for (const b of r.bands as unknown[]) {
    if (!b || typeof b !== "object") continue;
    const row = b as Record<string, unknown>;
    if (typeof row.band !== "string" || !BANDS.has(row.band) || !isNum(row.n)) continue;
    bands.push({
      band: row.band as MomentumBand,
      range: typeof row.range === "string" ? row.range : "",
      n: row.n,
      sessions: isNum(row.sessions) ? row.sessions : 0,
      targetRate: optNum(row.targetRate),
      stopFirstRate: optNum(row.stopFirstRate),
      medianCloseReturnNetPct: optNum(row.medianCloseReturnNetPct),
      medianMaxAdversePct: optNum(row.medianMaxAdversePct),
    });
  }
  return {
    measured: r.measured,
    generatedAt: typeof r.generatedAt === "string" ? r.generatedAt : undefined,
    targetPct: r.targetPct,
    stopPct: r.stopPct,
    minSample: r.minSample,
    sourceNote: typeof r.sourceNote === "string" ? r.sourceNote : "",
    bands,
  };
}

export function decodeFunds(raw: unknown): string[] | null {
  return Array.isArray(raw) && raw.every((s) => typeof s === "string") ? (raw as string[]) : null;
}

/** "10" / "2.5" -- whole numbers stay whole. */
function num(n: number): string {
  return Number.isInteger(n) ? `${n}` : n.toFixed(1);
}
const pct = (rate: number) => `${Math.round(rate * 100)}%`;
/** Signed with a real minus sign: "+1.4%" / "−3.2%". */
function signedPct(n: number): string {
  const r = Math.round(n * 10) / 10;
  return r > 0 ? `+${r.toFixed(1)}%` : r < 0 ? `−${Math.abs(r).toFixed(1)}%` : "0.0%";
}

function hasRates(row: MomentumBandRecord | undefined): row is MomentumBandRecord & { targetRate: number; stopFirstRate: number } {
  return row !== undefined && row.targetRate !== undefined && row.stopFirstRate !== undefined;
}

/** The details line for one band:
 * "Past Solid crossings: 18% reached +10% first · 64% hit −5% first · median −3.2% at close (n=120)". */
export function bandTrackRecordLine(record: MomentumTrackRecord | null, band: MomentumBand): string {
  if (!record || !record.measured) return "Track record not measured yet";
  const row = record.bands.find((b) => b.band === band);
  if (!hasRates(row)) return `Only ${row?.n ?? 0} past ${band} ${row?.n === 1 ? "case" : "cases"} — too few to state a rate`;
  const median = row.medianCloseReturnNetPct !== undefined ? ` · median ${signedPct(row.medianCloseReturnNetPct)} at close` : "";
  return `Past ${band} crossings: ${pct(row.targetRate)} reached +${num(record.targetPct)}% first · ${pct(row.stopFirstRate)} hit −${num(record.stopPct)}% first${median} (n=${row.n})`;
}

/** The compact legend line: "Reached +10% before −5%: Mild 12% (n=340) · Solid 18% (n=120) · Strong too few (n=14)". */
export function legendTrackRecordLine(record: MomentumTrackRecord | null, bands: readonly MomentumBand[]): string {
  if (!record || !record.measured) return "Track record not measured yet";
  const parts = bands.map((band) => {
    const row = record.bands.find((b) => b.band === band);
    return hasRates(row) ? `${band} ${pct(row.targetRate)} (n=${row.n})` : `${band} too few (n=${row?.n ?? 0})`;
  });
  return `Past crossings that reached +${num(record.targetPct)}% before −${num(record.stopPct)}%: ${parts.join(" · ")}`;
}
