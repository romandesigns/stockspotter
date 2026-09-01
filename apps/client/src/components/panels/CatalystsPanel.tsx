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

import { ChartIcon } from "../ChartIcon";
import { formatTag, formatTime } from "../../lib/format";
import type { CatalystUpdate } from "@stockspotter/shared-types";
import { EmptyState, PanelShell } from "../PanelShell";

export function CatalystsPanel(props: { rows: CatalystUpdate[]; onSelectSymbol: (symbol: string) => void; className?: string }) {
  return (
    <PanelShell title="Catalysts" subtitle="news tags, tracked symbols — click to view chart" count={props.rows.length} className={props.className}>
      {props.rows.length === 0 ? (
        <EmptyState>No catalyst lookups yet -- fires once a symbol starts being tracked…</EmptyState>
      ) : (
        <ul className="feed">
          {props.rows.map((r) => (
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
          ))}
        </ul>
      )}
    </PanelShell>
  );
}
