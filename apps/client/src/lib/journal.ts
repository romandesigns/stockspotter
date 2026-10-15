// Pure helpers behind the plan calculator (components/shortlist/
// PlanCalculator.tsx) and the Journal view (JournalView.tsx + useJournal.ts):
// form defaults, the risk preference, building the exact JournalEntry the
// server accepts, and decoding GET /journal. Kept out of the components so
// the rules are unit tested (journal.test.ts).
//
// The server (crates/ws-server/src/journal.rs) refuses unknown fields, so
// every entry built here carries exactly the JournalEntry fields and drops
// undefined ones instead of sending them as null.

import type { JournalEntry, JournalSnapshot, ShortlistCandidate } from "@stockspotter/shared-types";
import { assessTradability, suggestStop, TRADABILITY_QUOTE_MAX_AGE_SECS } from "@stockspotter/shared-types";

/** A quote older than this is not "fresh" -- the same limit the shared
 * tradability verdict uses. */
export const QUOTE_FRESH_SECS = TRADABILITY_QUOTE_MAX_AGE_SECS;

// ---------------------------------------------------------------------
// Plan form

export const PLAN_RISK_KEY = "stockspotter.planRisk.v1";
export const DEFAULT_PLAN_RISK = 100;
export const MAX_NOTES_CHARS = 2000;

/** The candidate's spread when its quote is fresh enough to trust (same
 * age rule as assessTradability); undefined otherwise. */
export function freshSpreadPct(c: ShortlistCandidate): number | undefined {
  const q = c.quote;
  return q && q.ageSecs <= QUOTE_FRESH_SECS && Number.isFinite(q.spreadPct) ? q.spreadPct : undefined;
}

/** The LULD upper band, only while a halt can actually trigger. */
export function activeUpperBand(c: ShortlistCandidate): number | undefined {
  return c.halt?.luldInEffect ? c.halt.upperBand : undefined;
}

/** Prices go into inputs at a tick-like precision: cents from $1 up,
 * four decimals below (sub-dollar names trade in 0.0001 increments). */
export function formatPriceInput(n: number): string {
  if (!Number.isFinite(n)) return "";
  return n >= 1 ? n.toFixed(2) : n.toFixed(4);
}

export interface PlanDefaults {
  entry: string;
  entryBasis: "ask" | "last price";
  stop: string;
  stopBasis: string;
}

/** Starting values for the plan form: entry at the ask when the quote is
 * fresh (that is what a buyer pays), else the last price; stop from
 * suggestStop at that entry. Both are suggestions to edit. */
export function planDefaults(c: ShortlistCandidate): PlanDefaults {
  const q = c.quote;
  const useAsk = q !== undefined && q.ageSecs <= QUOTE_FRESH_SECS && Number.isFinite(q.ask) && q.ask > 0;
  const entryText = formatPriceInput(useAsk ? q.ask : c.price);
  const stop = suggestStop(c, Number(entryText));
  return { entry: entryText, entryBasis: useAsk ? "ask" : "last price", stop: formatPriceInput(stop.price), stopBasis: stop.basis };
}

/** A typed number, or undefined for blank/unparseable text. */
export function parseNumberInput(text: string): number | undefined {
  const trimmed = text.trim();
  if (trimmed === "") return undefined;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : undefined;
}

export function readPlanRisk(storage: Pick<Storage, "getItem">): number {
  try {
    const n = Number(storage.getItem(PLAN_RISK_KEY));
    return Number.isFinite(n) && n > 0 ? n : DEFAULT_PLAN_RISK;
  } catch {
    return DEFAULT_PLAN_RISK;
  }
}

export function savePlanRisk(storage: Pick<Storage, "setItem">, risk: number): boolean {
  if (!(Number.isFinite(risk) && risk > 0)) return false;
  try {
    storage.setItem(PLAN_RISK_KEY, String(risk));
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------
// Building entries

const ID_RULE = /^[A-Za-z0-9_-]{1,64}$/;

/** A trade id the server accepts: a UUID (36 chars of hex and '-'). */
export function newJournalId(cryptoImpl: Pick<Crypto, "getRandomValues"> & { randomUUID?: () => string } = crypto): string {
  if (typeof cryptoImpl.randomUUID === "function") return cryptoImpl.randomUUID().replace(/[{}]/g, "");
  // randomUUID needs a secure context; fall back to 16 random bytes in hex.
  const bytes = cryptoImpl.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function isValidJournalId(id: string): boolean {
  return ID_RULE.test(id);
}

function dropUndefined<T extends object>(o: T): T {
  return Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;
}

/** What the card said when the trade was planned. */
export function journalSnapshot(c: ShortlistCandidate): JournalSnapshot {
  return dropUndefined<JournalSnapshot>({
    rank: c.rank,
    gapPct: c.gapPct,
    relativeVolume: c.relativeVolume,
    spreadPct: freshSpreadPct(c),
    tradability: assessTradability(c).verdict,
    profile: c.trackRecord?.profile,
    targetRate: c.trackRecord?.targetRate,
  });
}

export function buildJournalEntry(args: {
  id: string;
  candidate: ShortlistCandidate;
  entry: number;
  stop: number;
  target?: number;
  shares: number;
  nowIso: string;
}): JournalEntry {
  return dropUndefined<JournalEntry>({
    id: args.id,
    symbol: args.candidate.symbol,
    createdAt: args.nowIso,
    updatedAt: args.nowIso,
    entry: args.entry,
    stop: args.stop,
    target: args.target,
    shares: args.shares,
    snapshot: journalSnapshot(args.candidate),
  });
}

/** The same trade, closed at `price`. */
export function closeJournalEntry(e: JournalEntry, price: number, nowIso: string): JournalEntry {
  return { ...e, exit: { price, at: nowIso }, updatedAt: nowIso };
}

/** The same trade with new notes; blank notes are removed. */
export function withJournalNotes(e: JournalEntry, notes: string, nowIso: string): JournalEntry {
  const trimmed = notes.trim();
  const next: JournalEntry = { ...e, updatedAt: nowIso };
  if (trimmed === "") delete next.notes;
  else next.notes = notes.slice(0, MAX_NOTES_CHARS);
  return next;
}

/** Only the JournalEntry fields, in case a server or proxy ever adds one:
 * a POST of an entry read back from GET must not be refused for it. */
export function journalPayload(e: JournalEntry): JournalEntry {
  const snapshot = e.snapshot
    ? dropUndefined<JournalSnapshot>({
        rank: e.snapshot.rank,
        gapPct: e.snapshot.gapPct,
        relativeVolume: e.snapshot.relativeVolume,
        spreadPct: e.snapshot.spreadPct,
        tradability: e.snapshot.tradability,
        profile: e.snapshot.profile,
        targetRate: e.snapshot.targetRate,
      })
    : undefined;
  return dropUndefined<JournalEntry>({
    id: e.id,
    symbol: e.symbol,
    createdAt: e.createdAt,
    updatedAt: e.updatedAt,
    entry: e.entry,
    stop: e.stop,
    target: e.target,
    shares: e.shares,
    snapshot,
    exit: e.exit ? { price: e.exit.price, at: e.exit.at } : undefined,
    notes: e.notes,
  });
}

// ---------------------------------------------------------------------
// Decoding GET /journal

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function finite(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function decodeEntry(raw: unknown): JournalEntry | null {
  if (!isObject(raw)) return null;
  const { id, symbol, createdAt, updatedAt, entry, stop, shares } = raw;
  if (typeof id !== "string" || typeof symbol !== "string" || typeof createdAt !== "string" || typeof updatedAt !== "string") return null;
  if (![entry, stop, shares].every(finite)) return null;
  const exit = isObject(raw.exit) && finite(raw.exit.price) && typeof raw.exit.at === "string" ? { price: raw.exit.price, at: raw.exit.at } : undefined;
  const snapshot = isObject(raw.snapshot) && finite(raw.snapshot.gapPct) && finite(raw.snapshot.relativeVolume)
    ? (raw.snapshot as unknown as JournalSnapshot)
    : undefined;
  return dropUndefined<JournalEntry>({
    id, symbol, createdAt, updatedAt,
    entry: entry as number,
    stop: stop as number,
    target: finite(raw.target) ? raw.target : undefined,
    shares: shares as number,
    snapshot,
    exit,
    notes: typeof raw.notes === "string" ? raw.notes : undefined,
  });
}

/** Validates a GET /journal body. Null when it isn't a list at all (an
 * HTML error page, say); otherwise malformed entries are dropped. */
export function decodeJournal(raw: unknown): JournalEntry[] | null {
  if (!Array.isArray(raw)) return null;
  return raw.map(decodeEntry).filter((e): e is JournalEntry => e !== null);
}

// ---------------------------------------------------------------------
// Display

const money = new Intl.NumberFormat("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });

/** "$1,234.50" / "-$80.00"; `signed` adds "+" to gains (P&L). */
export function formatMoney(n: number, signed = false): string {
  if (!Number.isFinite(n)) return "—";
  const sign = n < 0 ? "-" : signed && n > 0 ? "+" : "";
  return `${sign}$${money.format(Math.abs(n))}`;
}

/** "+1.25R" / "-1.00R". */
export function formatR(r: number): string {
  if (!Number.isFinite(r)) return "—";
  return `${r > 0 ? "+" : ""}${r.toFixed(2)}R`;
}

const etDay = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", month: "short", day: "numeric" });

/** "Oct 15" in New York time -- the journal spans days, the time alone
 * (formatEtTime) does not say which. */
export function formatEtDay(iso: string): string {
  const t = new Date(iso);
  return Number.isNaN(t.getTime()) ? "" : etDay.format(t);
}
