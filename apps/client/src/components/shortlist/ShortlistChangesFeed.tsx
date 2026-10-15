// "What changed" -- a compact, newest-first feed of the last few list
// changes (lib/shortlistChanges.ts), under the ranked list. Display only:
// no sound, push or OS notification. A change is a reason to look again,
// not a signal to act.

import { formatEtTime } from "../../lib/shortlist";
import { describeChange, type ShortlistChange } from "../../lib/shortlistChanges";

/** Rows shown; the hook keeps a longer history for the day. */
const MAX_SHOWN = 8;

function tone(change: ShortlistChange): string {
  switch (change.kind) {
    case "haltEscalated":
      return change.to === "red" ? "changes-critical" : "changes-warning";
    case "crossedPremarketHigh":
      return "changes-good";
    case "entered":
      return "changes-accent";
    case "left":
      return "changes-muted";
  }
}

export function ShortlistChangesFeed(props: { changes: ShortlistChange[]; onSelectSymbol: (symbol: string) => void }) {
  const shown = props.changes.slice(0, MAX_SHOWN);
  return (
    <section className="shortlist-changes" aria-label="What changed">
      <h3 className="shortlist-changes-title">What changed</h3>
      {shown.length === 0 ? (
        <p className="shortlist-changes-empty">Nothing yet today. Changes to the list appear here as it refreshes.</p>
      ) : (
        <ol className="shortlist-changes-list">
          {shown.map((change) => (
            <li key={`${change.at}-${change.kind}-${change.symbol}`} className={`shortlist-change ${tone(change)}`}>
              <span className="shortlist-change-time">{formatEtTime(change.at)}</span>
              <button type="button" className="shortlist-change-symbol" onClick={() => props.onSelectSymbol(change.symbol)}>
                {change.symbol}
              </button>
              <span className="shortlist-change-text">{describeChange(change)}</span>
            </li>
          ))}
        </ol>
      )}
      <p className="shortlist-changes-note">Shown here only; no alerts are sent.</p>
    </section>
  );
}
