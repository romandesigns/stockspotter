// Pure helpers behind the mobile Shortlist tab (useShortlist.ts,
// useTradeJournal.ts, components/shortlist/*). Mobile port of
// apps/client/src/lib/shortlist.ts's decoding rules -- mobile can't import
// from apps/client, and the shared arithmetic (assessTradability,
// computePlan, suggestStop, journalResult, summarizeJournal) already lives
// in @stockspotter/shared-types. Kept free of React Native imports so
// `bun test apps/mobile/src/shortlist.test.ts` runs it directly.
//
// The shortlist is a WATCHLIST ordering -- where to look, not when to
// enter. Nothing here produces a buy signal.

import type {
  CatalystUpdate,
  FunnelSignal,
  JournalEntry,
  JournalSnapshot,
  PlanResult,
  ShortlistCandidate,
  ShortlistResponse,
  ShortlistSession,
  ShortlistTrackRecord,
  TradabilityVerdict,
} from "@stockspotter/shared-types";
import { assessTradability, computePlan, suggestStop, TRADABILITY_QUOTE_MAX_AGE_SECS } from "@stockspotter/shared-types";

/** A quote older than this is treated as missing -- the same limit the
 * shared tradability verdict uses. */
export const QUOTE_MAX_AGE_SECS = TRADABILITY_QUOTE_MAX_AGE_SECS;

const SESSIONS: readonly ShortlistSession[] = ["premarket", "regular", "afterhours", "overnight"];

export const SESSION_LABEL: Record<ShortlistSession, string> = {
  premarket: "Premarket",
  regular: "Regular session",
  afterhours: "After hours",
  overnight: "Overnight",
};

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function finite(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function count(value: unknown): number {
  return finite(value) && value >= 0 ? value : 0;
}

function optionalFinite(value: unknown): number | undefined {
  return finite(value) ? value : undefined;
}

// ---------------------------------------------------------------------
// GET /shortlist

function decodeLevels(raw: unknown): ShortlistCandidate["levels"] {
  const l = isObject(raw) ? raw : {};
  return {
    premarketHigh: optionalFinite(l.premarketHigh),
    premarketLow: optionalFinite(l.premarketLow),
    dayHigh: optionalFinite(l.dayHigh),
    vwap: optionalFinite(l.vwap),
    vwapAnchor: l.vwapAnchor === "premarket" || l.vwapAnchor === "regular" ? l.vwapAnchor : undefined,
    dollarVolume: optionalFinite(l.dollarVolume),
    barsFrom: typeof l.barsFrom === "string" ? l.barsFrom : undefined,
    // An absent flag can't vouch for the levels.
    barsComplete: l.barsComplete === true,
  };
}

function decodeQuote(raw: unknown): ShortlistCandidate["quote"] {
  if (!isObject(raw)) return undefined;
  const { bid, ask, spreadPct, ageSecs } = raw;
  if (![bid, ask, spreadPct, ageSecs].every(finite)) return undefined;
  return { bid: bid as number, ask: ask as number, spreadPct: spreadPct as number, ageSecs: ageSecs as number };
}

function decodeHalt(raw: unknown): ShortlistCandidate["halt"] {
  if (!isObject(raw)) return undefined;
  const level = raw.level;
  if (level !== "calm" && level !== "amber" && level !== "red") return undefined;
  const { proximityRatio, upperBand, lowerBand, distanceToUpperPct, ageSecs } = raw;
  if (![proximityRatio, upperBand, lowerBand, distanceToUpperPct].every(finite)) return undefined;
  return {
    level,
    proximityRatio: proximityRatio as number,
    upperBand: upperBand as number,
    lowerBand: lowerBand as number,
    distanceToUpperPct: distanceToUpperPct as number,
    // Unknown provenance reads as an estimate, never as SIP-supplied.
    estimated: raw.estimated !== false,
    luldInEffect: raw.luldInEffect === true,
    ageSecs: count(ageSecs),
  };
}

function decodeCatalyst(raw: unknown): ShortlistCandidate["catalyst"] {
  if (!isObject(raw) || !Array.isArray(raw.tags)) return undefined;
  return {
    tags: raw.tags.filter((t): t is string => typeof t === "string" && t.length > 0),
    headlineCount: count(raw.headlineCount),
    headline: typeof raw.headline === "string" && raw.headline.length > 0 ? raw.headline : undefined,
    publishedAt: typeof raw.publishedAt === "string" ? raw.publishedAt : undefined,
  };
}

/** A track record is shown only when it is internally consistent: a count,
 * and rates only alongside `sufficient === true`. Rates without a
 * sufficient sample are dropped rather than displayed. */
export function decodeTrackRecord(raw: unknown): ShortlistTrackRecord | undefined {
  if (!isObject(raw) || typeof raw.profile !== "string" || !finite(raw.n)) return undefined;
  const base: ShortlistTrackRecord = {
    profile: raw.profile,
    n: Math.max(0, Math.round(raw.n)),
    sufficient: raw.sufficient === true,
    targetPct: finite(raw.targetPct) ? raw.targetPct : 10,
    stopPct: finite(raw.stopPct) ? raw.stopPct : 5,
    decisionTimeEt: typeof raw.decisionTimeEt === "string" ? raw.decisionTimeEt : "09:35",
    sourceNote: typeof raw.sourceNote === "string" ? raw.sourceNote : "",
    generatedAt: typeof raw.generatedAt === "string" ? raw.generatedAt : "",
  };
  if (!base.sufficient) return base;
  return {
    ...base,
    targetRate: rate(raw.targetRate),
    stopFirstRate: rate(raw.stopFirstRate),
    medianCloseReturnNetPct: optionalFinite(raw.medianCloseReturnNetPct),
    medianMaxAdversePct: optionalFinite(raw.medianMaxAdversePct),
  };
}

function rate(value: unknown): number | undefined {
  return finite(value) && value >= 0 && value <= 1 ? value : undefined;
}

/** A candidate is only rendered when every field the row and the
 * tradability verdict read unconditionally is really there -- a
 * half-formed row would otherwise show "NaN%" or a fabricated $0.00. */
export function decodeCandidate(raw: unknown): ShortlistCandidate | null {
  if (!isObject(raw)) return null;
  const { rank, symbol, score, price, gapPct, relativeVolume, sessionVolume } = raw;
  if (typeof symbol !== "string" || symbol.trim().length === 0) return null;
  if (![rank, score, price, gapPct, relativeVolume, sessionVolume].every(finite)) return null;
  if ((price as number) <= 0) return null;
  return {
    rank: rank as number,
    symbol: symbol.trim().toUpperCase(),
    score: score as number,
    price: price as number,
    gapPct: gapPct as number,
    relativeVolume: relativeVolume as number,
    floatShares: optionalFinite(raw.floatShares),
    sessionVolume: sessionVolume as number,
    funnelPassed: raw.funnelPassed === true,
    funnelAt: typeof raw.funnelAt === "string" ? raw.funnelAt : "",
    levels: decodeLevels(raw.levels),
    quote: decodeQuote(raw.quote),
    halt: decodeHalt(raw.halt),
    catalyst: decodeCatalyst(raw.catalyst),
    trackRecord: decodeTrackRecord(raw.trackRecord),
  };
}

/** Validates and normalises a /shortlist response body. Returns null for
 * anything that isn't recognisably a shortlist (so a proxy's HTML error
 * page or a renamed field reads as a failed poll, not an empty list);
 * otherwise drops malformed candidates, de-duplicates symbols, and orders
 * by rank. */
export function decodeShortlist(raw: unknown): ShortlistResponse | null {
  if (!isObject(raw) || !Array.isArray(raw.candidates) || typeof raw.generatedAt !== "string") return null;
  const excluded = isObject(raw.excluded) ? raw.excluded : {};
  const seen = new Set<string>();
  const candidates = raw.candidates
    .map(decodeCandidate)
    .filter((c): c is ShortlistCandidate => c !== null)
    .sort((a, b) => a.rank - b.rank)
    .filter((c) => (seen.has(c.symbol) ? false : (seen.add(c.symbol), true)));
  return {
    generatedAt: raw.generatedAt,
    marketDay: typeof raw.marketDay === "string" ? raw.marketDay : null,
    session: SESSIONS.includes(raw.session as ShortlistSession) ? (raw.session as ShortlistSession) : "overnight",
    rankedBy: typeof raw.rankedBy === "string" ? raw.rankedBy : "",
    poolSize: count(raw.poolSize),
    excluded: {
      outsidePriceRange: count(excluded.outsidePriceRange),
      notGappingUp: count(excluded.notGappingUp),
      noRelativeVolume: count(excluded.noRelativeVolume),
    },
    candidates,
  };
}

/** The server's own observation time for a body -- what the "updated"
 * age counts from, so re-polling stale data never refreshes its age. */
export function shortlistObservedAt(body: ShortlistResponse | null): Date | null {
  if (!body) return null;
  const ms = Date.parse(body.generatedAt);
  return Number.isFinite(ms) ? new Date(ms) : null;
}

// ---------------------------------------------------------------------
// Ross pillars inputs (RossFivePillarsFlag reads a FunnelSignal + catalyst)

/** The live feed's own latest funnel signal for the symbol when it is at
 * least as recent as the candidate's; otherwise one rebuilt from the
 * candidate, timestamped at its real funnel evaluation so the flag's own
 * freshness rule still hides it when stale. */
export function candidateFunnel(c: ShortlistCandidate, live?: FunnelSignal): FunnelSignal {
  const candidateAt = Date.parse(c.funnelAt);
  if (live && !(Date.parse(live.timestamp) < candidateAt)) return live;
  return {
    type: "funnel_signal",
    symbol: c.symbol,
    timestamp: c.funnelAt,
    price: c.price,
    gapPct: c.gapPct,
    sessionVolume: c.sessionVolume,
    floatShares: c.floatShares ?? null,
    relativeVolume: c.relativeVolume,
    priceOk: c.funnelPassed,
    floatOk: c.funnelPassed,
    relVolOk: c.funnelPassed,
    gapOk: c.funnelPassed,
    passed: c.funnelPassed,
  };
}

/** The live feed's catalyst record wins; otherwise the candidate's own,
 * observed when the server generated the list. Undefined when neither
 * exists -- "unverified", never "no news". */
export function candidateCatalyst(c: ShortlistCandidate, generatedAt: string, live?: CatalystUpdate): CatalystUpdate | undefined {
  if (live) return live;
  if (!c.catalyst) return undefined;
  return {
    type: "catalyst_update",
    symbol: c.symbol,
    timestamp: generatedAt,
    catalystTags: c.catalyst.tags,
    headlineCount: c.catalyst.headlineCount,
    mostRecentHeadline: c.catalyst.headline ?? null,
    mostRecentPublishedAt: c.catalyst.publishedAt ?? null,
  };
}

// ---------------------------------------------------------------------
// Formatting

/** 4 decimals under $1 (sub-dollar names move in hundredths of a cent),
 * else 2 -- same rule ChartScreen's header price already uses. */
export function priceDecimals(n: number): number {
  return Math.abs(n) < 1 ? 4 : 2;
}

export function formatShortlistPrice(n: number | undefined | null): string {
  if (n == null || !Number.isFinite(n)) return "—";
  return `$${n.toFixed(priceDecimals(n))}`;
}

/** A price as an editable input value (no "$"). */
export function priceInput(n: number): string {
  return Number.isFinite(n) ? n.toFixed(priceDecimals(n)) : "";
}

export function formatSignedPct(n: number | undefined | null, digits = 1): string {
  if (n == null || !Number.isFinite(n)) return "—";
  return `${n >= 0 ? "+" : "−"}${Math.abs(n).toFixed(digits)}%`;
}

export function formatRvol(n: number): string {
  return `${n >= 100 ? Math.round(n) : n.toFixed(1)}×`;
}

export function formatShares(n: number | undefined | null): string {
  if (n == null || !Number.isFinite(n)) return "unknown";
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(2)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
  return String(Math.round(n));
}

export function formatDollars(n: number | undefined | null): string {
  if (n == null || !Number.isFinite(n)) return "unknown";
  const sign = n < 0 ? "−" : "";
  const a = Math.abs(n);
  if (a >= 1_000_000_000) return `${sign}$${(a / 1_000_000_000).toFixed(2)}B`;
  if (a >= 1_000_000) return `${sign}$${(a / 1_000_000).toFixed(1)}M`;
  if (a >= 10_000) return `${sign}$${Math.round(a / 1_000)}K`;
  return `${sign}$${a.toFixed(2)}`;
}

/** "+$123.45" / "−$80.00" for P&L. */
export function formatSignedDollars(n: number): string {
  return `${n >= 0 ? "+" : "−"}$${Math.abs(n).toFixed(2)}`;
}

export function formatR(n: number | undefined | null): string {
  if (n == null || !Number.isFinite(n)) return "—";
  return `${n >= 0 ? "+" : "−"}${Math.abs(n).toFixed(2)}R`;
}

export function formatRate(r: number | undefined): string {
  return r === undefined ? "—" : `${Math.round(r * 100)}%`;
}

export function formatTag(tag: string): string {
  return tag.replace(/_/g, " ");
}

// Hermes ships Intl with IANA zones; format.ts's formatTime already relies
// on timeZone "America/New_York" in production.
const etTime = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit" });
const etDate = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", month: "short", day: "numeric" });

/** Market times are read in New York time whatever the phone's zone: "9:41 AM ET". */
export function formatEtTime(iso: string | undefined | null): string {
  if (!iso) return "—";
  const t = new Date(iso);
  return Number.isNaN(t.getTime()) ? "—" : `${etTime.format(t)} ET`;
}

/** "Oct 15, 9:41 AM ET" -- for journal rows, which span days. */
export function formatEtDateTime(iso: string | undefined | null): string {
  if (!iso) return "—";
  const t = new Date(iso);
  return Number.isNaN(t.getTime()) ? "—" : `${etDate.format(t)}, ${etTime.format(t)} ET`;
}

/** "3m ago" / "5h ago" / "2d ago". */
export function formatAge(iso: string | undefined | null, nowMs = Date.now()): string | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  if (!Number.isFinite(ms)) return null;
  const minutes = Math.max(0, Math.round((nowMs - ms) / 60_000));
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

export const TRADABILITY_LABEL: Record<TradabilityVerdict, string> = {
  good: "Tradable",
  caution: "Caution",
  poor: "Hard to trade",
  unknown: "Unknown",
};

// ---------------------------------------------------------------------
// Plan

/** A positive decimal from a text field, or NaN. Accepts "1,234.5" and a
 * leading "$"; rejects anything else rather than guessing. */
export function parsePositive(text: string): number {
  const cleaned = text.trim().replace(/^\$/, "").replace(/,/g, "");
  if (!/^\d*\.?\d+$|^\d+\.$/.test(cleaned)) return Number.NaN;
  const n = Number(cleaned);
  return Number.isFinite(n) && n > 0 ? n : Number.NaN;
}

/** A spread is only worth pricing into the plan when the quote is fresh. */
export function freshQuote(c: ShortlistCandidate): ShortlistCandidate["quote"] {
  return c.quote && c.quote.ageSecs <= QUOTE_MAX_AGE_SECS ? c.quote : undefined;
}

export const DEFAULT_RISK_DOLLARS = 100;

export interface PlanDefaults {
  entry: number;
  entryBasis: "ask" | "last price";
  stop: number;
  stopBasis: string;
}

/** Entry = the fresh ask (what a buyer would actually pay), else the last
 * price; stop = suggestStop's nearest key level below it. Suggestions to
 * edit, not calls. */
export function planDefaults(c: ShortlistCandidate): PlanDefaults {
  const quote = freshQuote(c);
  const useAsk = quote !== undefined && quote.ask > 0;
  const entry = useAsk ? quote.ask : c.price;
  const stop = suggestStop(c, entry);
  return { entry, entryBasis: useAsk ? "ask" : "last price", stop: stop.price, stopBasis: stop.basis };
}

export interface PlanForm {
  entry: string;
  stop: string;
  target: string;
  risk: string;
}

/** computePlan over the form's text fields; an empty target is omitted,
 * a non-numeric one is an error rather than silently ignored. */
export function planFromForm(c: ShortlistCandidate, form: PlanForm): PlanResult {
  const targetText = form.target.trim();
  const target = targetText === "" ? undefined : parsePositive(targetText);
  if (target !== undefined && Number.isNaN(target)) return { ok: false, error: "The target must be a positive number, or left empty." };
  return computePlan({
    entry: parsePositive(form.entry),
    stop: parsePositive(form.stop),
    riskDollars: parsePositive(form.risk),
    target,
    spreadPct: freshQuote(c)?.spreadPct,
    haltUpperBand: c.halt?.upperBand,
  });
}

// ---------------------------------------------------------------------
// Journal

/** Matches the server's id rule, [A-Za-z0-9_-]{1,64}. */
export const JOURNAL_ID_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

const ID_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/** "m-<base36 ms>-<12 random chars>": unique enough for one person's
 * journal, sortable by creation, and always inside the server's id rule.
 * Uses crypto.getRandomValues when the runtime has it, else Math.random. */
export function newJournalId(nowMs = Date.now(), random: (n: number) => Uint8Array = randomBytes): string {
  const bytes = random(12);
  let suffix = "";
  for (let i = 0; i < 12; i++) suffix += ID_ALPHABET[(bytes[i] ?? 0) % ID_ALPHABET.length];
  return `m-${Math.max(0, Math.floor(nowMs)).toString(36)}-${suffix}`;
}

function randomBytes(n: number): Uint8Array {
  const out = new Uint8Array(n);
  const c = (globalThis as { crypto?: { getRandomValues?: (a: Uint8Array) => Uint8Array } }).crypto;
  if (c?.getRandomValues) {
    try {
      c.getRandomValues(out);
      return out;
    } catch {
      // fall through
    }
  }
  for (let i = 0; i < n; i++) out[i] = Math.floor(Math.random() * 256);
  return out;
}

/** What the card said when the trade was logged -- the point of the
 * journal is comparing the trader's results with these. */
export function journalSnapshot(c: ShortlistCandidate): JournalSnapshot {
  const snapshot: JournalSnapshot = {
    rank: c.rank,
    gapPct: c.gapPct,
    relativeVolume: c.relativeVolume,
    tradability: assessTradability(c).verdict,
  };
  const quote = freshQuote(c);
  if (quote) snapshot.spreadPct = quote.spreadPct;
  if (c.trackRecord) {
    snapshot.profile = c.trackRecord.profile;
    if (c.trackRecord.sufficient && c.trackRecord.targetRate !== undefined) snapshot.targetRate = c.trackRecord.targetRate;
  }
  return snapshot;
}

/** The POST /journal body for a newly logged plan. Records a plan; it
 * places no order. */
export function buildJournalEntry(args: {
  id: string;
  candidate: ShortlistCandidate;
  entry: number;
  stop: number;
  target?: number;
  shares: number;
  now: Date;
}): JournalEntry {
  const at = args.now.toISOString();
  const entry: JournalEntry = {
    id: args.id,
    symbol: args.candidate.symbol,
    createdAt: at,
    updatedAt: at,
    entry: args.entry,
    stop: args.stop,
    shares: args.shares,
    snapshot: journalSnapshot(args.candidate),
  };
  if (args.target !== undefined && Number.isFinite(args.target)) entry.target = args.target;
  return entry;
}

/** Same id, an exit at `price` now. */
export function closeJournalEntry(e: JournalEntry, price: number, now: Date): JournalEntry {
  const at = now.toISOString();
  return { ...e, exit: { price, at }, updatedAt: at };
}

/** Same id, new notes (empty clears them). */
export function updateJournalNotes(e: JournalEntry, notes: string, now: Date): JournalEntry {
  const trimmed = notes.trim();
  const next: JournalEntry = { ...e, updatedAt: now.toISOString() };
  if (trimmed === "") delete next.notes;
  else next.notes = trimmed;
  return next;
}

function decodeSnapshot(raw: unknown): JournalSnapshot | undefined {
  if (!isObject(raw) || !finite(raw.gapPct) || !finite(raw.relativeVolume)) return undefined;
  const t = raw.tradability;
  return {
    rank: optionalFinite(raw.rank),
    gapPct: raw.gapPct,
    relativeVolume: raw.relativeVolume,
    spreadPct: optionalFinite(raw.spreadPct),
    tradability: t === "good" || t === "caution" || t === "poor" || t === "unknown" ? t : undefined,
    profile: typeof raw.profile === "string" ? raw.profile : undefined,
    targetRate: rate(raw.targetRate),
  };
}

export function decodeJournalEntry(raw: unknown): JournalEntry | null {
  if (!isObject(raw)) return null;
  const { id, symbol, createdAt, updatedAt, entry, stop, shares } = raw;
  if (typeof id !== "string" || !JOURNAL_ID_PATTERN.test(id)) return null;
  if (typeof symbol !== "string" || symbol.length === 0) return null;
  if (typeof createdAt !== "string" || !Number.isFinite(Date.parse(createdAt))) return null;
  if (![entry, stop, shares].every(finite)) return null;
  const out: JournalEntry = {
    id,
    symbol,
    createdAt,
    updatedAt: typeof updatedAt === "string" ? updatedAt : createdAt,
    entry: entry as number,
    stop: stop as number,
    shares: shares as number,
  };
  if (finite(raw.target)) out.target = raw.target;
  const snapshot = decodeSnapshot(raw.snapshot);
  if (snapshot) out.snapshot = snapshot;
  if (isObject(raw.exit) && finite(raw.exit.price) && typeof raw.exit.at === "string") out.exit = { price: raw.exit.price, at: raw.exit.at };
  if (typeof raw.notes === "string" && raw.notes.length > 0) out.notes = raw.notes;
  return out;
}

/** GET /journal body -> entries newest first; malformed rows dropped.
 * Null when the body isn't an array at all (a failed load, not empty). */
export function decodeJournal(raw: unknown): JournalEntry[] | null {
  if (!Array.isArray(raw)) return null;
  return raw
    .map(decodeJournalEntry)
    .filter((e): e is JournalEntry => e !== null)
    .sort((a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt));
}

/** Replace-or-prepend by id, keeping newest-first order. */
export function upsertJournalEntry(entries: readonly JournalEntry[], next: JournalEntry): JournalEntry[] {
  const rest = entries.filter((e) => e.id !== next.id);
  return [next, ...rest].sort((a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt));
}
