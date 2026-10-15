// Trade journal (2026-10-15): what the trader actually did with a
// shortlisted candidate, next to what the card said at the time. Kept on
// the server (GET/POST /journal) so desktop and phone see one journal.
//
// The point is feedback: after a few weeks the trader can see whether their
// own picks from the shortlist work, instead of trusting the tool's labels.

export interface JournalSnapshot {
  rank?: number;
  gapPct: number;
  relativeVolume: number;
  spreadPct?: number;
  tradability?: "good" | "caution" | "poor" | "unknown";
  /** The track-record profile and its +target-first rate, if known. */
  profile?: string;
  targetRate?: number;
}

export interface JournalExit {
  price: number;
  at: string; // ISO 8601
}

export interface JournalEntry {
  /** Client-generated, stable across updates (same id = same trade). */
  id: string;
  symbol: string;
  createdAt: string; // ISO 8601
  updatedAt: string; // ISO 8601
  entry: number;
  stop: number;
  target?: number;
  shares: number;
  snapshot?: JournalSnapshot;
  exit?: JournalExit;
  notes?: string;
}

/** Profit in dollars and in R (multiples of the planned risk) for a closed
 * trade; null while it is open. Long only, as the shortlist is. */
export function journalResult(e: JournalEntry): { pnlDollars: number; rMultiple: number } | null {
  if (!e.exit) return null;
  const riskPerShare = e.entry - e.stop;
  const pnlDollars = (e.exit.price - e.entry) * e.shares;
  return { pnlDollars, rMultiple: riskPerShare > 0 ? (e.exit.price - e.entry) / riskPerShare : 0 };
}

export interface JournalSummary {
  trades: number;
  closed: number;
  wins: number;
  /** Wins over closed trades; undefined until something has closed. */
  winRate?: number;
  totalPnlDollars: number;
  averageR?: number;
}

export function summarizeJournal(entries: readonly JournalEntry[]): JournalSummary {
  const results = entries.map(journalResult).filter((r): r is NonNullable<typeof r> => r !== null);
  const wins = results.filter((r) => r.pnlDollars > 0).length;
  return {
    trades: entries.length,
    closed: results.length,
    wins,
    winRate: results.length > 0 ? wins / results.length : undefined,
    totalPnlDollars: results.reduce((s, r) => s + r.pnlDollars, 0),
    averageR: results.length > 0 ? results.reduce((s, r) => s + r.rMultiple, 0) / results.length : undefined,
  };
}
