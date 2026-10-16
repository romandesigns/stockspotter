// Panel scorecard (2026-10-16): the app judging its own panels. After each
// market close the server scores every detection each dashboard panel made
// -- the 30-minute return after costs -- against random moments in the same
// stock and session (crates/backtest-metrics/src/panel_scorecard, schema
// `panel-scorecard-v1`). ws-server serves the files:
//
//   GET /scorecard/latest     latest.json: rolling summary + the newest day
//   GET /scorecard/days       the scored days, newest first
//   GET /scorecard/day/:day   one day's file (per-detection rows only with ?rows=1)
//
// This module holds the wire types, tolerant decoders and every piece of
// wording both clients show, so the web page and the phone screen say the
// same thing about the same numbers. It is plain on purpose: a verdict is
// always a word (never colour alone), "insufficient data" is the honest
// default, and the comparison is against random entries, not against zero.

export const SCORECARD_SCHEMA = "panel-scorecard-v1";

/** How often an open scorecard re-fetches. The files change once a day
 * (after the close), so this only catches a new day landing while the page
 * is left open. */
export const SCORECARD_REFRESH_MS = 10 * 60_000;

/** First market day the panel-detection recorder (panel-detection-v1)
 * wrote. Days before it are scored from live_pending_signals.jsonl, which
 * never carried Top Gainers, Highly Trading, Halt Early-Warning or
 * Catalysts -- so those panels have no rows before this date. Shown in
 * "Not recorded yet" so an empty panel is not mistaken for a bad one. */
export const PANEL_RECORDER_STARTED = "2026-10-16";

export type ScorecardVerdict = "insufficient data" | "beats random" | "worse than random" | "no difference shown";

export interface ScorecardCi {
  lo: number;
  hi: number;
}

export interface ScorecardHorizon {
  horizon: string;
  /** Detections with both a value and a baseline at this horizon. */
  n: number;
  meanNetPct: number | null;
  medianNetPct: number | null;
  /** Share with a net return above 0. */
  hitRate: number | null;
  baselineMeanNetPct: number | null;
  /** Panel mean minus random mean, in percentage points. */
  diffPct: number | null;
  meanCi: ScorecardCi | null;
  diffCi: ScorecardCi | null;
}

export interface ScorecardPanel {
  panel: string;
  /** Every detection considered (scored + skipped). */
  detections: number;
  scored: number;
  symbols: number;
  /** Distinct market days at the primary horizon. */
  sessions: number;
  symbolDays: number;
  quoteCostRows: number;
  proxyCostRows: number;
  primaryHorizon: string;
  horizons: ScorecardHorizon[];
  targetFirstRate: number | null;
  stopFirstRate: number | null;
  baselineTargetFirstRate: number | null;
  verdict: ScorecardVerdict;
  verdictReason: string;
  skipped: Record<string, number>;
}

export interface ScorecardSampling {
  sampled: boolean;
  maxSymbols: number;
  symbolsAvailable: number;
  symbolsScored: number;
  detectionsAvailable: number;
  detectionsKept: number;
  method: string;
}

export interface ScorecardSource {
  kind: string;
  file: string | null;
  note: string;
}

export interface ScorecardMethod {
  primaryHorizon: string;
  kRandom: number;
  ciLevel: number;
  minDetections: number;
  minSessions: number;
  rollingSessions: number;
  roundTripCostProxyPct: number;
  targetPct: number;
  stopPct: number;
  notes: string[];
}

export interface ScorecardLatest {
  schema: typeof SCORECARD_SCHEMA;
  generatedAt: string;
  method: ScorecardMethod;
  rolling: { maxSessions: number; days: string[]; detectionsScored: number; panels: ScorecardPanel[] };
  latest: { day: string; source: ScorecardSource; sampling: ScorecardSampling | null; panels: ScorecardPanel[] };
}

export interface ScorecardDay {
  schema: typeof SCORECARD_SCHEMA;
  generatedAt: string;
  day: string;
  method: ScorecardMethod;
  source: ScorecardSource;
  sampling: ScorecardSampling | null;
  panels: ScorecardPanel[];
}

// ---- decoding -------------------------------------------------------------
// Tolerant: a missing or malformed optional number becomes null instead of
// rejecting the whole file, but a wrong schema or a missing panels array is
// "not recognised" -- showing numbers from a format we don't understand
// would be worse than showing nothing.

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const int = (v: unknown): number => num(v) ?? 0;
const str = (v: unknown, fallback = ""): string => (typeof v === "string" ? v : fallback);
const DAY_RE = /^\d{4}-\d{2}-\d{2}$/;
const VERDICTS: readonly ScorecardVerdict[] = ["insufficient data", "beats random", "worse than random", "no difference shown"];

function decodeCi(v: unknown): ScorecardCi | null {
  if (!isObj(v)) return null;
  const lo = num(v.lo);
  const hi = num(v.hi);
  return lo === null || hi === null ? null : { lo, hi };
}

function decodeHorizon(v: unknown): ScorecardHorizon | null {
  if (!isObj(v) || typeof v.horizon !== "string") return null;
  return {
    horizon: v.horizon,
    n: int(v.n),
    meanNetPct: num(v.meanNetPct),
    medianNetPct: num(v.medianNetPct),
    hitRate: num(v.hitRate),
    baselineMeanNetPct: num(v.baselineMeanNetPct),
    diffPct: num(v.diffPct),
    meanCi: decodeCi(v.meanCi),
    diffCi: decodeCi(v.diffCi),
  };
}

function decodePanel(v: unknown): ScorecardPanel | null {
  if (!isObj(v) || typeof v.panel !== "string") return null;
  const verdict = VERDICTS.includes(v.verdict as ScorecardVerdict) ? (v.verdict as ScorecardVerdict) : "insufficient data";
  const skipped: Record<string, number> = {};
  if (isObj(v.skipped)) for (const [k, n] of Object.entries(v.skipped)) if (num(n) !== null) skipped[k] = n as number;
  return {
    panel: v.panel,
    detections: int(v.detections),
    scored: int(v.scored),
    symbols: int(v.symbols),
    sessions: int(v.sessions),
    symbolDays: int(v.symbolDays),
    quoteCostRows: int(v.quoteCostRows),
    proxyCostRows: int(v.proxyCostRows),
    primaryHorizon: str(v.primaryHorizon, "30m"),
    horizons: Array.isArray(v.horizons) ? v.horizons.map(decodeHorizon).filter((h): h is ScorecardHorizon => h !== null) : [],
    targetFirstRate: num(v.targetFirstRate),
    stopFirstRate: num(v.stopFirstRate),
    baselineTargetFirstRate: num(v.baselineTargetFirstRate),
    verdict,
    verdictReason: str(v.verdictReason),
    skipped,
  };
}

function decodePanels(v: unknown): ScorecardPanel[] | null {
  return Array.isArray(v) ? v.map(decodePanel).filter((p): p is ScorecardPanel => p !== null) : null;
}

function decodeSampling(v: unknown): ScorecardSampling | null {
  if (!isObj(v)) return null;
  return {
    sampled: v.sampled === true,
    maxSymbols: int(v.maxSymbols),
    symbolsAvailable: int(v.symbolsAvailable),
    symbolsScored: int(v.symbolsScored),
    detectionsAvailable: int(v.detectionsAvailable),
    detectionsKept: int(v.detectionsKept),
    method: str(v.method),
  };
}

function decodeSource(v: unknown): ScorecardSource {
  if (!isObj(v)) return { kind: "none", file: null, note: "" };
  return { kind: str(v.kind, "none"), file: typeof v.file === "string" ? v.file : null, note: str(v.note) };
}

function decodeMethod(v: unknown): ScorecardMethod {
  const m = isObj(v) ? v : {};
  return {
    primaryHorizon: str(m.primaryHorizon, "30m"),
    kRandom: num(m.kRandom) ?? 20,
    ciLevel: num(m.ciLevel) ?? 0.95,
    minDetections: num(m.minDetections) ?? 30,
    minSessions: num(m.minSessions) ?? 3,
    rollingSessions: num(m.rollingSessions) ?? 20,
    roundTripCostProxyPct: num(m.roundTripCostProxyPct) ?? 0.5,
    targetPct: num(m.targetPct) ?? 10,
    stopPct: num(m.stopPct) ?? 5,
    notes: Array.isArray(m.notes) ? m.notes.filter((n): n is string => typeof n === "string") : [],
  };
}

export function decodeScorecardLatest(raw: unknown): ScorecardLatest | null {
  if (!isObj(raw) || raw.schema !== SCORECARD_SCHEMA || !isObj(raw.rolling) || !isObj(raw.latest)) return null;
  const rollingPanels = decodePanels(raw.rolling.panels);
  const latestPanels = decodePanels(raw.latest.panels);
  if (!rollingPanels || !latestPanels) return null;
  const days = Array.isArray(raw.rolling.days) ? raw.rolling.days.filter((d): d is string => typeof d === "string" && DAY_RE.test(d)) : [];
  return {
    schema: SCORECARD_SCHEMA,
    generatedAt: str(raw.generatedAt),
    method: decodeMethod(raw.method),
    rolling: { maxSessions: int(raw.rolling.maxSessions), days: [...days].sort(), detectionsScored: int(raw.rolling.detectionsScored), panels: rollingPanels },
    latest: { day: str(raw.latest.day), source: decodeSource(raw.latest.source), sampling: decodeSampling(raw.latest.sampling), panels: latestPanels },
  };
}

export function decodeScorecardDay(raw: unknown): ScorecardDay | null {
  if (!isObj(raw) || raw.schema !== SCORECARD_SCHEMA || typeof raw.day !== "string") return null;
  const panels = decodePanels(raw.panels);
  if (!panels) return null;
  return {
    schema: SCORECARD_SCHEMA,
    generatedAt: str(raw.generatedAt),
    day: raw.day,
    method: decodeMethod(raw.method),
    source: decodeSource(raw.source),
    sampling: decodeSampling(raw.sampling),
    panels,
  };
}

/** Accepts `["2026-10-15", …]`, `{ days: [...] }`, or entries shaped
 * `{ day: "2026-10-15", … }`; returns unique valid days, newest first. */
export function decodeScorecardDays(raw: unknown): string[] | null {
  const list = Array.isArray(raw) ? raw : isObj(raw) && Array.isArray(raw.days) ? raw.days : null;
  if (!list) return null;
  const days = new Set<string>();
  for (const item of list) {
    const d = typeof item === "string" ? item : isObj(item) && typeof item.day === "string" ? item.day : null;
    if (d && DAY_RE.test(d)) days.add(d);
  }
  return [...days].sort().reverse();
}

// ---- fetching ---------------------------------------------------------------

export type ScorecardResult<T> =
  | { kind: "ok"; body: T }
  /** 404: this server predates the scorecard routes, or has no file yet. */
  | { kind: "unavailable" }
  | { kind: "error"; message: string };

/** One GET, decoded. `fetcher` is authenticatedFetch in the apps; injected
 * so this stays testable and free of any platform import. */
export async function fetchScorecard<T>(
  fetcher: (url: string) => Promise<Response>,
  url: string,
  decode: (raw: unknown) => T | null,
): Promise<ScorecardResult<T>> {
  try {
    const response = await fetcher(url);
    if (response.status === 404) return { kind: "unavailable" };
    if (!response.ok) return { kind: "error", message: `HTTP ${response.status}` };
    const body = decode(await response.json());
    return body === null ? { kind: "error", message: "unrecognised response" } : { kind: "ok", body };
  } catch (err) {
    return { kind: "error", message: err instanceof Error ? err.message : "request failed" };
  }
}

export const scorecardUrls = {
  latest: (base: string) => `${base}/scorecard/latest`,
  days: (base: string) => `${base}/scorecard/days`,
  day: (base: string, day: string) => `${base}/scorecard/day/${encodeURIComponent(day)}`,
};

// ---- ordering ---------------------------------------------------------------

export interface ScorecardPanelInfo {
  key: string;
  title: string;
  group: "main" | "other";
  /** Shown only when the file has rows for it. */
  onlyWhenPresent?: boolean;
}

/** The dashboard's panels in the order the dashboard shows them, then the
 * detectors that feed them. Exchange halts are events, not a prediction,
 * so they appear only if the server ever scores them. */
export const SCORECARD_PANELS: readonly ScorecardPanelInfo[] = [
  { key: "halt_speed", title: "Halt Early-Warning", group: "main" },
  { key: "momentum", title: "Bullish Momentum", group: "main" },
  { key: "gap_and_go", title: "Gap & Go", group: "main" },
  { key: "ignition", title: "Ignition", group: "main" },
  { key: "top_gainers", title: "Top Gainers", group: "main" },
  { key: "highly_trading", title: "Highly Trading", group: "main" },
  { key: "catalyst", title: "Catalysts", group: "main" },
  { key: "micropullback", title: "Micropullback", group: "other" },
  { key: "consolidation_breakout", title: "Consolidation breakout", group: "other" },
  { key: "halt_actual", title: "Exchange halts", group: "other", onlyWhenPresent: true },
];

export interface ScorecardRow extends ScorecardPanelInfo {
  stats: ScorecardPanel | null;
}

/** Every known panel in fixed order (with null stats when the file has none),
 * then any panel this client doesn't know yet, under "other". */
export function scorecardRows(panels: readonly ScorecardPanel[]): ScorecardRow[] {
  const byKey = new Map(panels.map((p) => [p.panel, p]));
  const rows: ScorecardRow[] = [];
  for (const info of SCORECARD_PANELS) {
    const stats = byKey.get(info.key) ?? null;
    if (info.onlyWhenPresent && !stats) continue;
    rows.push({ ...info, stats });
  }
  const known = new Set(SCORECARD_PANELS.map((p) => p.key));
  for (const p of panels) {
    if (!known.has(p.panel)) rows.push({ key: p.panel, title: p.panel.replace(/_/g, " "), group: "other", stats: p });
  }
  return rows;
}

// ---- wording ----------------------------------------------------------------

export type VerdictTone = "good" | "critical" | "neutral" | "muted";

export function verdictView(verdict: ScorecardVerdict): { label: string; tone: VerdictTone } {
  switch (verdict) {
    case "beats random":
      return { label: "Beats random", tone: "good" };
    case "worse than random":
      return { label: "Worse than random", tone: "critical" };
    case "no difference shown":
      return { label: "No difference shown", tone: "neutral" };
    default:
      return { label: "Insufficient data", tone: "muted" };
  }
}

export function primaryStats(panel: ScorecardPanel): ScorecardHorizon | null {
  return panel.horizons.find((h) => h.horizon === panel.primaryHorizon) ?? null;
}

const MINUS = "−";

/** A percentage-point number: two decimals under 1, one above, real minus sign. */
export function formatPoints(x: number, digits = Math.abs(x) < 1 ? 2 : 1): string {
  const s = Math.abs(x).toFixed(digits);
  if (Number(s) === 0) return (0).toFixed(digits);
  return x < 0 ? `${MINUS}${s}` : s;
}

export function formatSignedPoints(x: number): string {
  const s = formatPoints(x);
  return x > 0 && Number(s) !== 0 ? `+${s}` : s;
}

/** "−1.85%" / "+0.40%". */
export function formatNetPct(x: number | null): string {
  return x === null ? "—" : `${formatSignedPoints(x)}%`;
}

export function formatRate(x: number | null): string {
  return x === null ? "—" : `${Math.round(x * 100)}%`;
}

/** "−1.5 pts vs random, range −2.63 to −0.67" -- both ends of the range at
 * the precision the smaller one needs, so "−3.6 to 0.96" can't read as
 * mismatched. */
export function differenceText(h: ScorecardHorizon | null): string {
  if (!h || h.diffPct === null) return "No comparison with random yet";
  const head = `${formatSignedPoints(h.diffPct)} pts vs random`;
  if (!h.diffCi) return `${head}, no range yet`;
  const digits = Math.min(Math.abs(h.diffCi.lo), Math.abs(h.diffCi.hi)) < 1 ? 2 : 1;
  return `${head}, range ${formatPoints(h.diffCi.lo, digits)} to ${formatPoints(h.diffCi.hi, digits)}`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "2026-10-15" → "Oct 15" (or "Oct 15, 2026" with the year). Pure string
 * math: a market day is a calendar date, not an instant, so no time zone. */
export function formatMarketDay(day: string, withYear = false): string {
  if (!DAY_RE.test(day)) return day;
  const [y, m, d] = day.split("-").map(Number);
  const base = `${MONTHS[m - 1]} ${d}`;
  return withYear ? `${base}, ${y}` : base;
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
}

/** "536 detections over 1 day". */
export function sampleText(panel: ScorecardPanel): string {
  const n = primaryStats(panel)?.n ?? 0;
  return `${plural(n, "detection")} over ${plural(panel.sessions, "day")}`;
}

/** "Last 20 sessions: Sep 18 – Oct 15". */
export function windowText(days: readonly string[]): string {
  if (days.length === 0) return "No scored sessions yet";
  const sorted = [...days].sort();
  const first = sorted[0];
  const last = sorted[sorted.length - 1];
  const span = first === last ? formatMarketDay(last) : `${formatMarketDay(first)} – ${formatMarketDay(last)}`;
  return `Last ${plural(sorted.length, "session")}: ${span}`;
}

/** "Oct 16, 8:52 AM ET"; the raw string when unparsable. */
export function formatGeneratedAt(iso: string): string {
  const t = new Date(iso);
  if (!iso || Number.isNaN(t.getTime())) return iso || "—";
  try {
    const text = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }).format(t);
    return `${text} ET`;
  } catch {
    return t.toISOString().replace("T", " ").slice(0, 16) + " UTC";
  }
}

export function methodText(method: ScorecardMethod): string {
  return `${horizonWords(method.primaryHorizon)} return after costs vs ${method.kRandom} random moments in the same stock and session`;
}

function horizonWords(h: string): string {
  const m = /^(\d+)m$/.exec(h);
  return m ? `${m[1]}-min` : h === "close" ? "Hold-to-close" : h;
}

/** The verdict rules, one sentence each, from the file's own thresholds. */
export function verdictRules(method: ScorecardMethod): string[] {
  const level = Math.round(method.ciLevel * 100);
  return [
    `Each detection is entered at the next 1-minute bar and valued ${horizonWords(method.primaryHorizon).toLowerCase()} later, after costs (the quoted spread when there was a fresh quote, otherwise a ${method.roundTripCostProxyPct}% round trip).`,
    `It is compared with ${method.kRandom} random entry minutes in the same stock, same day and same session, at the same cost. The difference is the panel's average minus the random average, in percentage points.`,
    `The range is a ${level}% bootstrap interval that treats each stock-day as one unit, so one stock that ran all day cannot dominate.`,
    `Insufficient data: fewer than ${method.minDetections} detections, or fewer than ${method.minSessions} market days. A single day is always insufficient — one day is one market mood.`,
    `Beats random: the whole range is above 0. Worse than random: the whole range is below 0. No difference shown: the range includes 0.`,
    `Hit rate is the share of detections up after costs at ${method.primaryHorizon}. Random entries in these stocks also tend to lose after costs, which is why the comparison is with random and not with zero.`,
  ];
}

/** Disclosure when a day was scored from a sample of symbols; null otherwise. */
export function samplingText(sampling: ScorecardSampling | null, day?: string): string | null {
  if (!sampling || !sampling.sampled) return null;
  const when = day ? ` on ${formatMarketDay(day)}` : "";
  return `Sampled${when}: ${sampling.symbolsScored.toLocaleString("en-US")} of ${sampling.symbolsAvailable.toLocaleString("en-US")} stocks scored (${sampling.detectionsKept.toLocaleString("en-US")} of ${sampling.detectionsAvailable.toLocaleString("en-US")} detections). Stocks were picked by a fixed shuffle, and every detection of a picked stock is kept.`;
}

/** Shown for a panel the file has no rows for. */
export function notRecordedText(): string {
  return `Not recorded yet — recording started ${formatMarketDay(PANEL_RECORDER_STARTED, true)}`;
}

/** A plain-words note when the day came from the older live-signals log. */
export function sourceText(source: ScorecardSource): string | null {
  if (source.kind === "live-pending-fallback") return "Scored from the older live-signals log (no quotes, so costs use the round-trip estimate).";
  if (source.kind === "none") return "No detections were recorded for this day.";
  return null;
}

// ---- per-day trend -------------------------------------------------------------

export interface TrendPoint {
  day: string;
  /** Panel minus random at the primary horizon; null when not scored that day. */
  diffPct: number | null;
  n: number;
}

/** One point per day (oldest first) for one panel, from the day files. */
export function panelTrend(days: readonly ScorecardDay[], panelKey: string): TrendPoint[] {
  return [...days]
    .sort((a, b) => (a.day < b.day ? -1 : a.day > b.day ? 1 : 0))
    .map((d) => {
      const p = d.panels.find((x) => x.panel === panelKey);
      const h = p ? primaryStats(p) : null;
      return { day: d.day, diffPct: h?.diffPct ?? null, n: h?.n ?? 0 };
    });
}

/** "Oct 14 −0.40, Oct 15 −1.52" -- the trend as text, for screen readers. */
export function trendText(points: readonly TrendPoint[]): string {
  const scored = points.filter((p) => p.diffPct !== null);
  if (scored.length === 0) return "No per-day difference yet";
  return `Difference per day: ${scored.map((p) => `${formatMarketDay(p.day)} ${formatSignedPoints(p.diffPct!)}`).join(", ")}`;
}
