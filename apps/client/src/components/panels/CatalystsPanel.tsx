// Catalysts panel (doc section 4.4) -- news catalyst tags for tracked
// symbols, computed server-side by the Python qualitative layer and
// broadcast once per symbol at promotion time (market_data::events::
// ScanEvent::CatalystUpdate) -- this event type already existed and was
// already flowing over the wire; the only real gap was that no panel
// ever read it (App.tsx's placeholder note about "not broadcast yet" was
// stale by the time this was checked).
//
// Made actionable per Roman's explicit ask ("not only there to display
// headlines... we want to make it actionable, practical"): every row is
// a real click target that loads that symbol into the Super Chart panel
// (App.tsx's lifted selectedSymbol), not just readable text. Same
// onSelectSymbol callback CatalystBadge uses everywhere else a ticker
// with a catalyst shows up.
//
// Confirmation badge (Confirmed/Unconfirmed) is the second pass of that
// same "make it actionable" ask -- a catalyst tag alone doesn't say
// whether the stock actually did anything with it; derive.ts's
// catalystConfirmation() joins the tag against the same real
// momentum_scorer reading every other panel already uses. "pending" (no
// momentum reading yet) renders no badge at all rather than a third,
// duller chip -- most catalyst lookups fire the instant a symbol is
// promoted, before there's been time to accumulate a real reading, so a
// visible "pending" state would be the common case, not a rare one.

import { ChartIcon } from "../ChartIcon";
import { formatTag, formatTime } from "../../lib/format";
import { catalystConfirmation, type CatalystConfirmation } from "../../lib/derive";
import type { CatalystUpdate, MomentumUpdate } from "@stockspotter/shared-types";
import { EmptyState, PanelShell } from "../PanelShell";

const CONFIRMATION_LABEL: Record<Exclude<CatalystConfirmation, "pending">, string> = {
  confirmed: "Confirmed",
  unconfirmed: "Unconfirmed",
};

export function CatalystsPanel(props: { rows: CatalystUpdate[]; momentumBySymbol: Map<string, MomentumUpdate>; onSelectSymbol: (symbol: string) => void; className?: string }) {
  return (
    <PanelShell title="Catalysts" subtitle="news tags, tracked symbols — click to view chart" count={props.rows.length} className={props.className}>
      {props.rows.length === 0 ? (
        <EmptyState>No catalyst lookups yet -- fires once a symbol starts being tracked…</EmptyState>
      ) : (
        <ul className="feed">
          {props.rows.map((r) => {
            const confirmation = catalystConfirmation(props.momentumBySymbol.get(r.symbol));
            return (
              <li
                key={r.symbol}
                className="feed-row feed-row-clickable"
                role="button"
                tabIndex={0}
                onClick={() => props.onSelectSymbol(r.symbol)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    props.onSelectSymbol(r.symbol);
                  }
                }}
              >
                <div className="feed-row-main">
                  <span className="ticker">{r.symbol}</span>
                  {confirmation !== "pending" && (
                    <span className={`chip ${confirmation === "confirmed" ? "chip-good" : "chip-bad"}`} title="Whether real momentum currently backs this catalyst, not just that it was tagged">
                      {CONFIRMATION_LABEL[confirmation]}
                    </span>
                  )}
                  <span className="dim">{r.headlineCount} headline{r.headlineCount === 1 ? "" : "s"}</span>
                  <span className="dim time">{formatTime(r.timestamp)}</span>
                  <span className="feed-row-go-spacer" />
                  <ChartIcon name="chevron-r" />
                </div>
                {r.catalystTags.length > 0 && (
                  <div className="feed-row-conditions">
                    {r.catalystTags.map((tag) => (
                      <span key={tag} className="chip chip-accent">
                        {formatTag(tag)}
                      </span>
                    ))}
                  </div>
                )}
                {r.mostRecentHeadline && <div className="catalyst-headline">{r.mostRecentHeadline}</div>}
              </li>
            );
          })}
        </ul>
      )}
    </PanelShell>
  );
}
