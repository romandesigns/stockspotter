// The Journal workspace (third option of the rail's view switch): every
// trade logged from a decision card's Plan section, what the card said at
// the time, and how it turned out. Kept on the server (GET/POST /journal)
// so desktop and phone share one journal; an update is a new version of
// the same trade id. The point is the trader's own feedback loop -- their
// picks against the tool's labels -- not a performance claim.

import type { JournalEntry } from "@stockspotter/shared-types";
import { journalResult, summarizeJournal } from "@stockspotter/shared-types";
import { useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { EmptyState, PanelShell } from "../PanelShell";
import { formatPrice } from "../../lib/format";
import { closeJournalEntry, formatEtDay, formatMoney, formatR, MAX_NOTES_CHARS, parseNumberInput, withJournalNotes } from "../../lib/journal";
import { formatEtTime } from "../../lib/shortlist";
import { useJournal, type JournalSaveResult } from "../../lib/useJournal";

const VERDICT_LABEL = { good: "Good", caution: "Caution", poor: "Poor", unknown: "Unknown" } as const;

function Summary(props: { entries: JournalEntry[] }) {
  const s = summarizeJournal(props.entries);
  if (s.closed === 0) {
    return (
      <div className="journal-summary">
        <div className="journal-stat"><span>Trades</span><strong>{s.trades}</strong></div>
        <p className="journal-summary-empty">No closed trades yet</p>
      </div>
    );
  }
  return (
    <div className="journal-summary">
      <div className="journal-stat"><span>Trades</span><strong>{s.trades}</strong></div>
      <div className="journal-stat"><span>Closed</span><strong>{s.closed}</strong></div>
      <div className="journal-stat"><span>Wins</span><strong>{s.wins}</strong></div>
      <div className="journal-stat"><span>Win rate</span><strong>{s.winRate !== undefined ? `${Math.round(s.winRate * 100)}%` : "—"}</strong></div>
      <div className="journal-stat">
        <span>Total P&amp;L</span>
        <strong className={s.totalPnlDollars < 0 ? "pct-down" : s.totalPnlDollars > 0 ? "pct-up" : undefined}>{formatMoney(s.totalPnlDollars, true)}</strong>
      </div>
      <div className="journal-stat"><span>Average R</span><strong>{s.averageR !== undefined ? formatR(s.averageR) : "—"}</strong></div>
    </div>
  );
}

type RowStatus = { kind: "idle" } | { kind: "saving" } | { kind: "error"; text: string } | { kind: "saved" };

function JournalRow(props: { entry: JournalEntry; save: (entry: JournalEntry) => Promise<JournalSaveResult> }) {
  const e = props.entry;
  const ids = useId();
  const [exitText, setExitText] = useState("");
  const [notes, setNotes] = useState(e.notes ?? "");
  const [status, setStatus] = useState<RowStatus>({ kind: "idle" });
  const result = journalResult(e);
  const snap = e.snapshot;
  const exitPrice = parseNumberInput(exitText);
  const notesChanged = notes.trim() !== (e.notes ?? "").trim();
  // Saves from one row go out one at a time, in order: blurring the notes
  // by clicking Close sends both, and the later version must land last.
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  // The notes text last sent, so the blur and the button can't both send it.
  const sentNotes = useRef(e.notes ?? "");

  function run(next: JournalEntry) {
    setStatus({ kind: "saving" });
    queue.current = queue.current.then(async () => {
      const saved = await props.save(next);
      if (!saved.ok) sentNotes.current = e.notes ?? ""; // allow a retry
      setStatus(saved.ok ? { kind: "saved" } : { kind: "error", text: saved.error });
    });
  }

  function close() {
    if (exitPrice === undefined || exitPrice <= 0) {
      setStatus({ kind: "error", text: "Enter the exit price first." });
      return;
    }
    const now = new Date().toISOString();
    // Carry an unsaved notes draft along, so closing never drops it.
    run(closeJournalEntry(notesChanged ? withJournalNotes(e, notes, now) : e, exitPrice, now));
  }

  function saveNotes() {
    if (!notesChanged || notes.trim() === sentNotes.current.trim()) return;
    sentNotes.current = notes;
    run(withJournalNotes(e, notes, new Date().toISOString()));
  }

  return (
    <li className="journal-row">
      <div className="journal-row-head">
        <span className="journal-symbol">{e.symbol}</span>
        <span className="dim">{formatEtDay(e.createdAt)} · {formatEtTime(e.createdAt)}</span>
        <span className={`chip ${result ? "journal-closed" : "journal-open"}`}>{result ? "closed" : "open"}</span>
        {result && (
          <span className={`journal-result ${result.pnlDollars < 0 ? "pct-down" : result.pnlDollars > 0 ? "pct-up" : ""}`}>
            {formatMoney(result.pnlDollars, true)} · {formatR(result.rMultiple)}
          </span>
        )}
      </div>

      <dl className="journal-facts">
        <div><dt>Entry</dt><dd>{formatPrice(e.entry)}</dd></div>
        <div><dt>Stop</dt><dd>{formatPrice(e.stop)}</dd></div>
        <div><dt>Target</dt><dd>{e.target !== undefined ? formatPrice(e.target) : "—"}</dd></div>
        <div><dt>Shares</dt><dd>{e.shares.toLocaleString("en-US")}</dd></div>
        {e.exit && <div><dt>Exit</dt><dd>{formatPrice(e.exit.price)} <span className="dim">at {formatEtTime(e.exit.at)}</span></dd></div>}
      </dl>

      {snap && (
        <p className="journal-snapshot">
          <span className="dim">When logged:</span>{" "}
          {snap.tradability && (
            <span className={`tradability-${snap.tradability}`}>
              <span className="tradability-dot" aria-hidden="true" /> {VERDICT_LABEL[snap.tradability]}
            </span>
          )}
          {snap.rank !== undefined && <span> · #{snap.rank}</span>}
          {snap.targetRate !== undefined && <span> · track record: {Math.round(snap.targetRate * 100)}% reached target first</span>}
          {snap.profile && <span className="dim"> · {snap.profile}</span>}
        </p>
      )}

      {!result && (
        <div className="journal-close">
          <Label htmlFor={`${ids}-exit`} className="plan-label">Exit price</Label>
          <Input
            id={`${ids}-exit`}
            className="plan-input journal-exit-input"
            type="number"
            inputMode="decimal"
            min="0"
            step="any"
            value={exitText}
            onChange={(ev) => setExitText(ev.target.value)}
            onKeyDown={(ev) => { if (ev.key === "Enter") close(); }}
          />
          <Button size="sm" variant="secondary" onClick={close}>Close</Button>
        </div>
      )}

      <div className="journal-notes">
        <Label htmlFor={`${ids}-notes`} className="plan-label">Notes</Label>
        <Textarea
          id={`${ids}-notes`}
          className="journal-notes-input"
          value={notes}
          maxLength={MAX_NOTES_CHARS}
          placeholder="Why you took it, what happened"
          onChange={(ev) => setNotes(ev.target.value)}
          onBlur={saveNotes}
        />
        {notesChanged && (
          <Button size="xs" variant="ghost" onClick={saveNotes}>Save notes</Button>
        )}
      </div>

      {status.kind === "error" && <p className="journal-status journal-status-error" role="status">Not saved: {status.text}</p>}
      {status.kind === "saving" && <p className="journal-status" role="status">Saving…</p>}
    </li>
  );
}

export function JournalView() {
  const journal = useJournal();
  const { entries, error, loading } = journal;
  return (
    <main className="journal-view">
      <PanelShell title="Journal" count={entries.length} className="journal-panel">
        <Summary entries={entries} />
        {error && entries.length > 0 && <div className="panel-warning">Couldn't refresh ({error}) — showing the last journal received.</div>}
        {entries.length === 0 ? (
          <EmptyState>
            {error
              ? `Journal unavailable: ${error}`
              : loading
                ? "Loading journal…"
                : "No trades logged yet. Use “Log trade” in a shortlist card's Plan section."}
          </EmptyState>
        ) : (
          <ul className="journal-rows" aria-label="Logged trades">
            {/* key includes updatedAt so a saved version resets its row's drafts */}
            {entries.map((e) => <JournalRow key={`${e.id}-${e.updatedAt}`} entry={e} save={journal.save} />)}
          </ul>
        )}
        <p className="journal-footer">Your own record. Compare it with each card's track record before trusting any setup.</p>
      </PanelShell>
    </main>
  );
}
