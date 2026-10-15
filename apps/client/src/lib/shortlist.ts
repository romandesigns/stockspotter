// Pure helpers behind the Shortlist view (useShortlist.ts + components/
// shortlist/*) -- kept here, not in the hook or the components, so the
// decoding and adapter rules are unit tested (shortlist.test.ts) rather
// than only exercised through a rendered component.

import type { CatalystUpdate, FunnelSignal, ShortlistCandidate, ShortlistResponse, ShortlistSession } from "@stockspotter/shared-types";

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

/** A candidate is only rendered when every field the row and the
 * tradability verdict read unconditionally is really there -- a
 * half-formed row would otherwise show "NaN%" or a fabricated $0.00. */
function decodeCandidate(raw: unknown): ShortlistCandidate | null {
  if (!isObject(raw)) return null;
  const { rank, symbol, score, price, gapPct, relativeVolume, sessionVolume } = raw;
  if (typeof symbol !== "string" || symbol.length === 0) return null;
  if (![rank, score, price, gapPct, relativeVolume, sessionVolume].every(finite)) return null;
  const levels = isObject(raw.levels) ? raw.levels : {};
  return {
    ...(raw as unknown as ShortlistCandidate),
    funnelPassed: raw.funnelPassed === true,
    funnelAt: typeof raw.funnelAt === "string" ? raw.funnelAt : "",
    // barsComplete defaults to false: an absent flag can't vouch for the levels.
    levels: { ...(levels as unknown as ShortlistCandidate["levels"]), barsComplete: levels.barsComplete === true },
    quote: isObject(raw.quote) ? (raw.quote as unknown as ShortlistCandidate["quote"]) : undefined,
    halt: isObject(raw.halt) ? (raw.halt as unknown as ShortlistCandidate["halt"]) : undefined,
    catalyst: isObject(raw.catalyst) && Array.isArray(raw.catalyst.tags)
      ? (raw.catalyst as unknown as ShortlistCandidate["catalyst"])
      : undefined,
    trackRecord: decodeTrackRecord(raw.trackRecord),
  };
}

/** A track record is shown only when it is internally consistent: a count,
 * and rates only alongside `sufficient`. Rates without a sufficient sample
 * are dropped rather than displayed. */
function decodeTrackRecord(raw: unknown): ShortlistCandidate["trackRecord"] {
  if (!isObject(raw) || typeof raw.profile !== "string" || !finite(raw.n)) return undefined;
  const record = raw as unknown as NonNullable<ShortlistCandidate["trackRecord"]>;
  if (record.sufficient === true) return record;
  return { ...record, sufficient: false, targetRate: undefined, stopFirstRate: undefined, medianCloseReturnNetPct: undefined, medianMaxAdversePct: undefined };
}

/** Validates and normalises a /shortlist response body. Returns null for
 * anything that isn't recognisably a shortlist (so a proxy's HTML error
 * page or a renamed field reads as a failed poll, not an empty list);
 * otherwise drops malformed candidates and orders by rank. */
export function decodeShortlist(raw: unknown): ShortlistResponse | null {
  if (!isObject(raw) || !Array.isArray(raw.candidates) || typeof raw.generatedAt !== "string") return null;
  const excluded = isObject(raw.excluded) ? raw.excluded : {};
  const candidates = raw.candidates
    .map(decodeCandidate)
    .filter((c): c is ShortlistCandidate => c !== null)
    .sort((a, b) => a.rank - b.rank);
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

/** The server's own observation time for a body, as a Date -- what
 * UpdatedAgo should count from (same rule as useMovers: re-polling stale
 * data must not refresh its age). */
export function shortlistObservedAt(body: ShortlistResponse | null): Date | null {
  if (!body) return null;
  const ms = Date.parse(body.generatedAt);
  return Number.isFinite(ms) ? new Date(ms) : null;
}

/** RossFivePillarsBadge reads a FunnelSignal. Use the live feed's own
 * latest signal for the symbol when it is at least as recent as the
 * candidate's; otherwise rebuild one from the candidate, timestamped at
 * its real funnel evaluation (`funnelAt`) so the badge's own freshness
 * rule still hides it when that is stale. */
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

/** Same idea for the catalyst: the live feed's record wins; otherwise the
 * candidate's own catalyst, observed when the server generated the list.
 * Undefined when neither exists -- "unverified", never "no news". */
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

/** "$12.3M" / "$480K" / "$900" -- dollar volume at a glance. */
const etTime = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit" });

/** Market times are read in New York time whatever the viewer's zone: "9:41 AM ET". */
export function formatEtTime(iso: string): string {
  const t = new Date(iso);
  return Number.isNaN(t.getTime()) ? iso : `${etTime.format(t)} ET`;
}

export function formatDollars(n: number): string {
  if (n >= 1_000_000_000) return `$${(n / 1_000_000_000).toFixed(2)}B`;
  if (n >= 1_000_000) return `$${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `$${Math.round(n / 1_000)}K`;
  return `$${Math.round(n)}`;
}

/** "3m ago" / "5h ago" / "2d ago" for a headline's publication time. */
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

// ---------------------------------------------------------------------
// Shortlist | Classic | Journal view switch (App.tsx's rail). Same storage-may-throw
// handling as researchPreferences.ts: a blocked/private storage must never
// break the app, it just stops remembering the choice.

export type WorkspaceView = "shortlist" | "classic" | "journal";
export const WORKSPACE_VIEWS: readonly WorkspaceView[] = ["shortlist", "classic", "journal"];
export const WORKSPACE_VIEW_KEY = "stockspotter.workspaceView.v1";

export function isWorkspaceView(value: unknown): value is WorkspaceView {
  return WORKSPACE_VIEWS.includes(value as WorkspaceView);
}

/** Anything unrecognised (nothing saved, an old or hand-edited value)
 * falls back to the Shortlist, the default view. */
export function readWorkspaceView(storage: Pick<Storage, "getItem">): WorkspaceView {
  try {
    const saved = storage.getItem(WORKSPACE_VIEW_KEY);
    return isWorkspaceView(saved) ? saved : "shortlist";
  } catch {
    return "shortlist";
  }
}

export function saveWorkspaceView(storage: Pick<Storage, "setItem">, view: WorkspaceView): boolean {
  try {
    storage.setItem(WORKSPACE_VIEW_KEY, view);
    return true;
  } catch {
    return false;
  }
}
