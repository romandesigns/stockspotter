// Order-flow summary for one symbol: the JSON `GET /orderflow/:symbol`
// and `/orderflow/:symbol/full` return (crates/ws-server/src/http.rs,
// shaped by order_flow_wire.rs), serialized from
// `order_flow::OrderFlowSummary` (crates/order-flow/src/engine.rs). Field
// names here must match that struct's serde output exactly -- camelCase
// fields, snake_case enum strings.
//
// Everything in it is OBSERVATIONAL RESEARCH. No level or state here has
// been validated against outcomes, none feeds the momentum score or any
// alert, and the thresholds behind them are untuned defaults. UI built on
// these types must keep saying so (a "research" tag), not present them as
// a signal.
//
// **Why this lives here and not in @stockspotter/shared-types.** Order
// flow is a web-only opt-in research protocol. The shared package is the
// contract the mobile app's own handshake and panels read, and mobile
// changes are behind the Android advisory gate; adding a message there
// would commit both apps to a protocol only one of them speaks. Nothing
// in this file alters or shadows an existing shared type -- the realtime
// feed still parses `RealtimeMessage` exactly as before and routes the
// `order_flow` frame out before that switch sees it (useRealtimeFeed.ts).
//
// Mobile parity is deferred. When the advisory gate opens, the right move
// is to lift these types into shared-types unchanged; they are written to
// make that a move rather than a rewrite.

export type OrderFlowSide = "buy" | "sell" | "unknown";
export type OrderFlowLevel = "none" | "low" | "moderate" | "high" | "extreme";

export interface OrderFlowWindow {
  windowSecs: number;
  buyVolume: number;
  sellVolume: number;
  /** Volume whose side could not be inferred. Kept separate on purpose. */
  unknownVolume: number;
  delta: number;
  /** (buy - sell) / (buy + sell). null when nothing was classified. */
  deltaRatio: number | null;
  trades: number;
  shares: number;
  dollars: number;
  tradesPerSec: number;
  sharesPerSec: number;
  dollarsPerSec: number;
  /** This window's share rate over the previous one's. null from nothing. */
  shareVelocityAcceleration: number | null;
}

export interface OrderFlowProfile {
  poc: number;
  valueAreaLow: number;
  valueAreaHigh: number;
  hvn: number[];
  lvn: number[];
  /** `volume` is everything traded at the price; `buyVolume` and
   * `sellVolume` are the aggressive parts (the footprint). The remainder
   * could not be given a side. Empty in a lite summary. */
  bins: { price: number; volume: number; buyVolume: number; sellVolume: number }[];
  totalVolume: number;
  /** Epoch seconds of the first trade in the profile: it starts when the
   * symbol became tracked, not at the session open, unless `coverage`
   * says the session was backfilled. */
  sinceSecs: number;
}

export interface OrderFlowBookSide {
  removedSize: number;
  addedSize: number;
  levelsCleared: number;
  refillsAtTouch: number;
  possiblePersistentLiquidity: boolean;
}

export interface OrderFlowSweep {
  timestampSecs: number;
  direction: OrderFlowSide;
  levels: number;
  volume: number;
  dollars: number;
  durationSecs: number;
  displacement: number;
  /** Lit exchanges the run printed on. */
  venues?: number;
}

export interface OrderFlowDeltaBucket {
  startSecs: number;
  buyVolume: number;
  sellVolume: number;
  unknownVolume: number;
  open: number;
  close: number;
  /** Size at the best bid / ask as of the bucket's last trade, in the
   * feed's quote units (round lots). null with no usable quote. ONE level
   * only: the touch, not depth. NBBO carries no book, so no UI built on
   * this may describe it as one. */
  bidSize: number | null;
  askSize: number | null;
}

/** A price where heavy aggressive volume was absorbed and price has not
 * traded through. Liquidity that was HIT, never liquidity known to be
 * resting -- UI must word it that way ("absorbed here"). */
export interface OrderFlowAbsorptionLevel {
  price: number;
  /** sell_side = buyers were absorbed (a ceiling); buy_side = a floor. */
  side: "buy_side" | "sell_side";
  volume: number;
  firstSecs: number;
  lastSecs: number;
}

export interface OrderFlowLargePrint {
  timestampSecs: number;
  price: number;
  size: number;
  side: OrderFlowSide;
}

export interface OrderFlowSummary {
  asOfSecs: number;
  /** false = no quote seen yet, so every side is tick-rule only. */
  hasQuotes: boolean;
  /** 1s, 5s, 15s, 30s, 60s, 300s -- in that order. */
  windows: OrderFlowWindow[];
  cumulativeDeltaPremarket: number;
  cumulativeDeltaRegular: number;
  cumulativeDeltaAfterHours: number;
  large: {
    threshold: number | null;
    count: number;
    volume: number;
    buyVolume: number;
    sellVolume: number;
    delta: number;
    intensity: number | null;
  };
  absorption: {
    level: OrderFlowLevel;
    /** sell_side = buyers are being absorbed; buy_side = sellers are. */
    side: "buy_side" | "sell_side" | null;
    intensity: number | null;
  };
  /** Highest price first. */
  absorptionLevels: OrderFlowAbsorptionLevel[];
  netImpactEfficiency: number | null;
  exhaustion: {
    level: "none" | "possible" | "likely";
    velocityFading: boolean;
    deltaFading: boolean;
    largeTradesFading: boolean;
  };
  sweeps: {
    state: OrderFlowLevel;
    count: number;
    buyCount: number;
    sellCount: number;
    flaggedBuyVolume: number;
    flaggedSellVolume: number;
    /** How many of `count` printed on three or more lit exchanges.
     * Absent from a server that does not read the exchange code. */
    multiVenueCount?: number;
    last: OrderFlowSweep | null;
  };
  book: { ask: OrderFlowBookSide; bid: OrderFlowBookSide };
  profile: OrderFlowProfile | null;
  interpretation: {
    pressureSide: "buy" | "sell" | "balanced";
    pressureLevel: OrderFlowLevel;
    largeTrades: "buy_dominant" | "sell_dominant" | "balanced" | "none";
    flow: "confirming" | "diverging" | "neutral";
  };
  /** 30-second buckets, oldest first. Empty in a lite summary. */
  deltaBuckets: OrderFlowDeltaBucket[];
  largePrints: OrderFlowLargePrint[];
  /** When the three arrays above were built, which is NOT `asOfSecs` once
   * they have been carried forward across lite updates. Absent means
   * "this body says nothing about them" -- see `withChartArrays`. */
  chartArraysAsOfSecs?: number;
}

/** How much of today the summary covers, straight from the server. Never
 * derived on the client: an engine that was evicted and recreated has a
 * hole in its day that no backfill repairs, and only the server knows. */
export type OrderFlowCoverage = "partial" | "session_pending" | "session";

/** Correction and cancel frames are COUNTED, never applied. This says the
 * tape is uncorrected and how much correction traffic has been seen
 * process-wide; it is not a per-symbol reconciliation claim. */
export interface OrderFlowTape {
  provenance: "uncorrected" | "corrections_pending";
  correctionsSeen: number;
  cancelsSeen: number;
}

export interface OrderFlowResponse {
  /** false = the server has the feature switched off. */
  enabled: boolean;
  symbol: string;
  /** Whether the symbol's earlier session was fetched and replayed into
   * the engine serving this summary. Absent from a server that predates
   * backfill. Prefer `coverage`, which also accounts for engine churn. */
  backfill?: "none" | "running" | "done" | "failed";
  coverage?: OrderFlowCoverage;
  tape?: OrderFlowTape;
  /** null = enabled, but this symbol has not traded while tracked today. */
  summary: OrderFlowSummary | null;
}

/** One pushed frame: the lite body, tagged and addressed. */
export interface OrderFlowPushFrame extends OrderFlowResponse {
  type: "order_flow";
}

// ---------------------------------------------------------------------------
// Decoding
//
// Both ways a summary reaches this client -- the pushed socket frame and
// the `/orderflow` HTTP body -- go through `decodeOrderFlowBody` below.
// There is deliberately ONE decoder: the frame used to be checked and the
// HTTP body cast, and the parts of the summary the strip dereferences
// without asking (`exhaustion.level`, `book.ask`, `sweeps.state`,
// `deltaBuckets`, `interpretation.largeTrades` as a map key) were not
// among the fields either path checked. A server on a different version,
// or anything else answering that URL, would then throw inside a React
// render instead of being read as "this body is not usable".
//
// Every field a consumer reads is checked here: numbers are finite,
// enumerations are one of their own values, nested arrays are arrays and
// their elements are objects of the right shape. A malformed body is
// rejected whole -- `null` -- rather than patched up with defaults,
// because a zero in place of a missing number is indistinguishable on
// screen from a real zero.
// ---------------------------------------------------------------------------

/** Thrown while reading a body and caught at the boundary below; never
 * escapes this module. */
class Malformed extends Error {}

function fail(field: string): never {
  throw new Malformed(field);
}

/** A finite number. `NaN` and the infinities are rejected: they travel
 * through JSON as `null` or as strings, and arrive here as an arithmetic
 * result that renders as "NaN%" in a meter. */
function num(value: unknown, field: string): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fail(field);
}

function numOrNull(value: unknown, field: string): number | null {
  return value === null ? null : num(value, field);
}

function bool(value: unknown, field: string): boolean {
  return typeof value === "boolean" ? value : fail(field);
}

function record(value: unknown, field: string): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : fail(field);
}

function list(value: unknown, field: string): unknown[] {
  return Array.isArray(value) ? value : fail(field);
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[], field: string): T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value) ? (value as T) : fail(field);
}

/** An absent field is absent; a present one must be valid. Used only for
 * fields a server that predates them legitimately omits. */
function optional<T>(value: unknown, read: (value: unknown) => T): T | undefined {
  return value === undefined ? undefined : read(value);
}

const SIDES = ["buy", "sell", "unknown"] as const;
const LEVELS = ["none", "low", "moderate", "high", "extreme"] as const;
const BOOK_SIDES = ["buy_side", "sell_side"] as const;
const COVERAGES = ["partial", "session_pending", "session"] as const;
const BACKFILLS = ["none", "running", "done", "failed"] as const;
const PROVENANCES = ["uncorrected", "corrections_pending"] as const;

function readWindow(value: unknown): OrderFlowWindow {
  const w = record(value, "window");
  return {
    windowSecs: num(w.windowSecs, "window.windowSecs"),
    buyVolume: num(w.buyVolume, "window.buyVolume"),
    sellVolume: num(w.sellVolume, "window.sellVolume"),
    unknownVolume: num(w.unknownVolume, "window.unknownVolume"),
    delta: num(w.delta, "window.delta"),
    deltaRatio: numOrNull(w.deltaRatio, "window.deltaRatio"),
    trades: num(w.trades, "window.trades"),
    shares: num(w.shares, "window.shares"),
    dollars: num(w.dollars, "window.dollars"),
    tradesPerSec: num(w.tradesPerSec, "window.tradesPerSec"),
    sharesPerSec: num(w.sharesPerSec, "window.sharesPerSec"),
    dollarsPerSec: num(w.dollarsPerSec, "window.dollarsPerSec"),
    shareVelocityAcceleration: numOrNull(w.shareVelocityAcceleration, "window.shareVelocityAcceleration"),
  };
}

function readBookSide(value: unknown, field: string): OrderFlowBookSide {
  const s = record(value, field);
  return {
    removedSize: num(s.removedSize, `${field}.removedSize`),
    addedSize: num(s.addedSize, `${field}.addedSize`),
    levelsCleared: num(s.levelsCleared, `${field}.levelsCleared`),
    refillsAtTouch: num(s.refillsAtTouch, `${field}.refillsAtTouch`),
    possiblePersistentLiquidity: bool(s.possiblePersistentLiquidity, `${field}.possiblePersistentLiquidity`),
  };
}

function readSweep(value: unknown): OrderFlowSweep {
  const s = record(value, "sweep");
  return {
    timestampSecs: num(s.timestampSecs, "sweep.timestampSecs"),
    direction: oneOf(s.direction, SIDES, "sweep.direction"),
    levels: num(s.levels, "sweep.levels"),
    volume: num(s.volume, "sweep.volume"),
    dollars: num(s.dollars, "sweep.dollars"),
    durationSecs: num(s.durationSecs, "sweep.durationSecs"),
    displacement: num(s.displacement, "sweep.displacement"),
    venues: optional(s.venues, (v) => num(v, "sweep.venues")),
  };
}

function readDeltaBucket(value: unknown): OrderFlowDeltaBucket {
  const b = record(value, "deltaBucket");
  return {
    startSecs: num(b.startSecs, "deltaBucket.startSecs"),
    buyVolume: num(b.buyVolume, "deltaBucket.buyVolume"),
    sellVolume: num(b.sellVolume, "deltaBucket.sellVolume"),
    unknownVolume: num(b.unknownVolume, "deltaBucket.unknownVolume"),
    open: num(b.open, "deltaBucket.open"),
    close: num(b.close, "deltaBucket.close"),
    bidSize: numOrNull(b.bidSize, "deltaBucket.bidSize"),
    askSize: numOrNull(b.askSize, "deltaBucket.askSize"),
  };
}

function readProfile(value: unknown): OrderFlowProfile | null {
  if (value === null || value === undefined) return null;
  const p = record(value, "profile");
  return {
    poc: num(p.poc, "profile.poc"),
    valueAreaLow: num(p.valueAreaLow, "profile.valueAreaLow"),
    valueAreaHigh: num(p.valueAreaHigh, "profile.valueAreaHigh"),
    hvn: list(p.hvn, "profile.hvn").map((v) => num(v, "profile.hvn[]")),
    lvn: list(p.lvn, "profile.lvn").map((v) => num(v, "profile.lvn[]")),
    bins: list(p.bins, "profile.bins").map((v) => {
      const bin = record(v, "profile.bins[]");
      return {
        price: num(bin.price, "profile.bins[].price"),
        volume: num(bin.volume, "profile.bins[].volume"),
        buyVolume: num(bin.buyVolume, "profile.bins[].buyVolume"),
        sellVolume: num(bin.sellVolume, "profile.bins[].sellVolume"),
      };
    }),
    totalVolume: num(p.totalVolume, "profile.totalVolume"),
    sinceSecs: num(p.sinceSecs, "profile.sinceSecs"),
  };
}

function readSummary(value: unknown): OrderFlowSummary {
  const s = record(value, "summary");
  const large = record(s.large, "large");
  const absorption = record(s.absorption, "absorption");
  const exhaustion = record(s.exhaustion, "exhaustion");
  const sweeps = record(s.sweeps, "sweeps");
  const book = record(s.book, "book");
  const interpretation = record(s.interpretation, "interpretation");
  return {
    asOfSecs: num(s.asOfSecs, "asOfSecs"),
    hasQuotes: bool(s.hasQuotes, "hasQuotes"),
    windows: list(s.windows, "windows").map(readWindow),
    cumulativeDeltaPremarket: num(s.cumulativeDeltaPremarket, "cumulativeDeltaPremarket"),
    cumulativeDeltaRegular: num(s.cumulativeDeltaRegular, "cumulativeDeltaRegular"),
    cumulativeDeltaAfterHours: num(s.cumulativeDeltaAfterHours, "cumulativeDeltaAfterHours"),
    large: {
      threshold: numOrNull(large.threshold, "large.threshold"),
      count: num(large.count, "large.count"),
      volume: num(large.volume, "large.volume"),
      buyVolume: num(large.buyVolume, "large.buyVolume"),
      sellVolume: num(large.sellVolume, "large.sellVolume"),
      delta: num(large.delta, "large.delta"),
      intensity: numOrNull(large.intensity, "large.intensity"),
    },
    absorption: {
      level: oneOf(absorption.level, LEVELS, "absorption.level"),
      side: absorption.side === null ? null : oneOf(absorption.side, BOOK_SIDES, "absorption.side"),
      intensity: numOrNull(absorption.intensity, "absorption.intensity"),
    },
    absorptionLevels: list(s.absorptionLevels, "absorptionLevels").map((v) => {
      const level = record(v, "absorptionLevels[]");
      return {
        price: num(level.price, "absorptionLevels[].price"),
        side: oneOf(level.side, BOOK_SIDES, "absorptionLevels[].side"),
        volume: num(level.volume, "absorptionLevels[].volume"),
        firstSecs: num(level.firstSecs, "absorptionLevels[].firstSecs"),
        lastSecs: num(level.lastSecs, "absorptionLevels[].lastSecs"),
      };
    }),
    netImpactEfficiency: numOrNull(s.netImpactEfficiency, "netImpactEfficiency"),
    exhaustion: {
      level: oneOf(exhaustion.level, ["none", "possible", "likely"] as const, "exhaustion.level"),
      velocityFading: bool(exhaustion.velocityFading, "exhaustion.velocityFading"),
      deltaFading: bool(exhaustion.deltaFading, "exhaustion.deltaFading"),
      largeTradesFading: bool(exhaustion.largeTradesFading, "exhaustion.largeTradesFading"),
    },
    sweeps: {
      state: oneOf(sweeps.state, LEVELS, "sweeps.state"),
      count: num(sweeps.count, "sweeps.count"),
      buyCount: num(sweeps.buyCount, "sweeps.buyCount"),
      sellCount: num(sweeps.sellCount, "sweeps.sellCount"),
      flaggedBuyVolume: num(sweeps.flaggedBuyVolume, "sweeps.flaggedBuyVolume"),
      flaggedSellVolume: num(sweeps.flaggedSellVolume, "sweeps.flaggedSellVolume"),
      multiVenueCount: optional(sweeps.multiVenueCount, (v) => num(v, "sweeps.multiVenueCount")),
      last: sweeps.last === null || sweeps.last === undefined ? null : readSweep(sweeps.last),
    },
    book: { ask: readBookSide(book.ask, "book.ask"), bid: readBookSide(book.bid, "book.bid") },
    profile: readProfile(s.profile),
    interpretation: {
      pressureSide: oneOf(interpretation.pressureSide, ["buy", "sell", "balanced"] as const, "interpretation.pressureSide"),
      pressureLevel: oneOf(interpretation.pressureLevel, LEVELS, "interpretation.pressureLevel"),
      largeTrades: oneOf(interpretation.largeTrades, ["buy_dominant", "sell_dominant", "balanced", "none"] as const, "interpretation.largeTrades"),
      flow: oneOf(interpretation.flow, ["confirming", "diverging", "neutral"] as const, "interpretation.flow"),
    },
    deltaBuckets: list(s.deltaBuckets, "deltaBuckets").map(readDeltaBucket),
    largePrints: list(s.largePrints, "largePrints").map((v) => {
      const print = record(v, "largePrints[]");
      return {
        timestampSecs: num(print.timestampSecs, "largePrints[].timestampSecs"),
        price: num(print.price, "largePrints[].price"),
        size: num(print.size, "largePrints[].size"),
        side: oneOf(print.side, SIDES, "largePrints[].side"),
      };
    }),
    chartArraysAsOfSecs: optional(s.chartArraysAsOfSecs, (v) => num(v, "chartArraysAsOfSecs")),
  };
}

/**
 * Reads an `/orderflow` response body or a pushed frame, or returns null
 * for anything that is not one.
 *
 * `expectSymbol`, when given, is the symbol the caller asked about: a body
 * for another symbol is rejected rather than shown against the open chart.
 * Comparison is case-insensitive because the server upper-cases the
 * symbol it echoes and the client sends what the user picked.
 */
export function decodeOrderFlowBody(message: unknown, expectSymbol?: string): OrderFlowResponse | null {
  try {
    const body = record(message, "body");
    const symbol = typeof body.symbol === "string" && body.symbol.length > 0 ? body.symbol : fail("symbol");
    if (expectSymbol !== undefined && symbol.toUpperCase() !== expectSymbol.toUpperCase()) return null;
    // Present and null are both valid ("no summary for this symbol"); an
    // object is a summary; anything else is a body this cannot read.
    const summary = body.summary === null || body.summary === undefined ? null : readSummary(body.summary);
    return {
      enabled: bool(body.enabled, "enabled"),
      symbol,
      backfill: optional(body.backfill, (v) => oneOf(v, BACKFILLS, "backfill")),
      coverage: optional(body.coverage, (v) => oneOf(v, COVERAGES, "coverage")),
      tape: optional(body.tape, (v) => {
        const tape = record(v, "tape");
        return {
          provenance: oneOf(tape.provenance, PROVENANCES, "tape.provenance"),
          correctionsSeen: num(tape.correctionsSeen, "tape.correctionsSeen"),
          cancelsSeen: num(tape.cancelsSeen, "tape.cancelsSeen"),
        };
      }),
      summary,
    };
  } catch (error) {
    // Only this module's own rejection is swallowed. A real fault in the
    // decoder must not be reported to the UI as "the server sent rubbish".
    if (error instanceof Malformed) return null;
    throw error;
  }
}

/**
 * Reads an `order_flow` frame off the realtime socket, or returns null for
 * anything that is not one.
 *
 * The same decoder as the HTTP path, plus the frame tag: this is the one
 * place a socket message becomes a typed object without going through
 * `RealtimeMessage`.
 */
export function decodeOrderFlowFrame(message: unknown): OrderFlowPushFrame | null {
  if (typeof message !== "object" || message === null) return null;
  if ((message as Record<string, unknown>).type !== "order_flow") return null;
  const body = decodeOrderFlowBody(message);
  return body === null ? null : { type: "order_flow", ...body };
}

/**
 * A "lite" summary arrives without the chart-only arrays --
 * `deltaBuckets`, `largePrints`, `profile.bins` -- because they are most
 * of the response's bytes and change slowly
 * (crates/ws-server/src/order_flow_wire.rs). This puts the arrays from the
 * last full summary back, so every consumer always sees a complete one.
 *
 * The carried-forward arrays keep the age they actually have:
 * `chartArraysAsOfSecs` comes from the full summary they came from, never
 * from the lite one's own `asOfSecs`. Without that the delta pane and the
 * profile look exactly as current as the header numbers beside them while
 * being minutes older. With no full summary yet the arrays are empty and
 * `chartArraysAsOfSecs` is undefined, which is the honest answer rather
 * than a timestamp for data that is not there.
 *
 * Arrays further than `MAX_CARRY_FORWARD_SECS` from the summary they would
 * be shown beside are dropped rather than labelled. That is the market-day
 * case: a chart left open overnight would otherwise draw yesterday's delta
 * buckets and yesterday's profile under today's numbers, and "20 hours
 * old" is not a chart anyone wants drawn. It holds in both directions,
 * because a summary whose clock has moved backwards that far is not the
 * same session either.
 */
export function withChartArrays(lite: OrderFlowSummary, lastFull: OrderFlowSummary | null): OrderFlowSummary {
  const arraysAsOf = lastFull ? (lastFull.chartArraysAsOfSecs ?? lastFull.asOfSecs) : undefined;
  const carry = arraysAsOf !== undefined && Math.abs(lite.asOfSecs - arraysAsOf) <= MAX_CARRY_FORWARD_SECS ? lastFull : null;
  return {
    ...lite,
    deltaBuckets: carry?.deltaBuckets ?? [],
    largePrints: carry?.largePrints ?? [],
    profile: lite.profile ? { ...lite.profile, bins: carry?.profile?.bins ?? [] } : null,
    chartArraysAsOfSecs: carry ? arraysAsOf : undefined,
  };
}

/** How far from the summary beside them carried-forward arrays may be
 * before they are dropped instead of drawn. Fifteen minutes: long enough
 * that an occasional missed full fetch keeps the panes, short enough that
 * nothing from another session is ever drawn. */
export const MAX_CARRY_FORWARD_SECS = 900;

/**
 * Whether a body is the full shape -- the one that carries the chart
 * arrays and is therefore what later lite summaries carry forward.
 *
 * Read from `chartArraysAsOfSecs`, which the server stamps on a full body
 * and only a full body (crates/ws-server/src/order_flow_wire.rs). The
 * array contents cannot decide this: a session with no large prints yet,
 * or a symbol in its first 30 seconds, has an empty `deltaBuckets` in a
 * perfectly full answer, and treating that as "lite" left the client
 * carrying older arrays forward over it for ever.
 *
 * The fallback is for a server that predates both `/full` and the stamp:
 * it answers the lite route with everything, so non-empty arrays still
 * count as full.
 */
export function isFullSummary(summary: OrderFlowSummary | null): boolean {
  if (summary === null) return false;
  return summary.chartArraysAsOfSecs !== undefined || summary.deltaBuckets.length > 0;
}

/** How far behind the rest of the summary the chart arrays are, in
 * seconds, or null when they are current or absent. Used to label the
 * delta pane and the profile rather than to hide them. */
export function chartArrayLagSecs(summary: OrderFlowSummary): number | null {
  if (summary.chartArraysAsOfSecs === undefined) return null;
  const lag = summary.asOfSecs - summary.chartArraysAsOfSecs;
  return lag > 0 ? lag : null;
}

/** The summary window that best matches a chart timeframe, so the numbers
 * beside a 30-second chart describe 30 seconds and not five minutes.
 * Charts coarser than the longest window use the longest one there is. */
export function orderFlowWindowFor(summary: OrderFlowSummary, chartBucketMinutes: number): OrderFlowWindow | null {
  const wanted = Math.min(300, Math.max(30, chartBucketMinutes * 60));
  let best: OrderFlowWindow | null = null;
  for (const w of summary.windows) {
    if (best === null || Math.abs(w.windowSecs - wanted) < Math.abs(best.windowSecs - wanted)) best = w;
  }
  return best;
}

/** "30s", "1m", "5m". */
export function formatWindowLabel(windowSecs: number): string {
  return windowSecs < 60 ? `${windowSecs}s` : `${Math.round(windowSecs / 60)}m`;
}

/** Signed share counts the way the design shows them: +342K, -1.2M, +850. */
export function formatSignedShares(value: number): string {
  const sign = value > 0 ? "+" : value < 0 ? "-" : "";
  return sign + formatShares(Math.abs(value));
}

export function formatShares(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(value >= 100_000 ? 0 : 1)}K`;
  return String(Math.round(value));
}

const LEVEL_ORDER: OrderFlowLevel[] = ["none", "low", "moderate", "high", "extreme"];

/** How many of a four-segment meter's segments a level fills (none = 0). */
export function levelSegments(level: OrderFlowLevel): number {
  const index = LEVEL_ORDER.indexOf(level);
  return index < 0 ? 0 : index;
}

/** Buy / sell / unknown as whole percentages of the window's shares that
 * always sum to 100 (unknown takes the rounding remainder), or null for
 * an empty window. */
export function sideShares(w: OrderFlowWindow): { buy: number; sell: number; unknown: number } | null {
  if (w.shares <= 0) return null;
  const buy = Math.round((w.buyVolume / w.shares) * 100);
  const sell = Math.round((w.sellVolume / w.shares) * 100);
  return { buy, sell, unknown: Math.max(0, 100 - buy - sell) };
}

/** Running cumulative delta across the delta buckets, for the CVD line. */
export function cumulativeDeltaSeries(buckets: OrderFlowDeltaBucket[]): number[] {
  let run = 0;
  return buckets.map((b) => (run += b.buyVolume - b.sellVolume));
}
