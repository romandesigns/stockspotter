// The Scorecard workspace (fourth option of the rail's view switch): the
// app judging its own panels. For each panel, the 30-minute return after
// costs of everything it showed, against random moments in the same stock
// and session -- and a plain verdict word, which stays "Insufficient data"
// until there are enough detections over enough days. Numbers and wording
// come from shared-types' scorecard.ts so the phone says the same thing.
//
// Colour never carries the verdict alone: the badge always says the word.
// The accent is spent only on the active day in the picker (via Select).

import type { ScorecardDay, ScorecardPanel, ScorecardRow, ScorecardSampling, ScorecardSource, TrendPoint } from "@stockspotter/shared-types";
import {
  differenceText,
  formatGeneratedAt,
  formatMarketDay,
  formatNetPct,
  formatRate,
  methodText,
  notRecordedText,
  panelTrend,
  primaryStats,
  sampleText,
  samplingText,
  scorecardRows,
  sourceText,
  trendText,
  verdictRules,
  verdictView,
  windowText,
} from "@stockspotter/shared-types";
import { useMemo, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { EmptyState, PanelShell } from "../PanelShell";
import { useScorecard, type ScorecardState } from "../../lib/useScorecard";

const ROLLING = "rolling";

export function ScorecardView() {
  const sc = useScorecard(true);
  const [selection, setSelection] = useState<string>(ROLLING);

  function choose(value: string) {
    setSelection(value);
    if (value !== ROLLING && value !== sc.latest?.latest.day) sc.loadDay(value);
  }

  const picker =
    sc.latest && sc.days.length > 0 ? (
      <Select value={selection} onValueChange={choose}>
        <SelectTrigger size="sm" className="scorecard-day-trigger" aria-label="Which sessions to show">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ROLLING}>Rolling ({sc.latest.rolling.days.length})</SelectItem>
          {sc.days.map((d) => (
            <SelectItem key={d} value={d}>
              {formatMarketDay(d, true)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    ) : undefined;

  return (
    <main className="scorecard-view">
      <PanelShell title="Scorecard" subtitle="Do the panels beat random picks?" headerExtra={picker} className="scorecard-panel">
        <ScorecardBody sc={sc} selection={selection} onBackToRolling={() => setSelection(ROLLING)} />
      </PanelShell>
    </main>
  );
}

function ScorecardBody(props: { sc: ScorecardState; selection: string; onBackToRolling: () => void }) {
  const { sc, selection } = props;
  const latest = sc.latest;

  const trendDays = useMemo(
    () => (latest ? latest.rolling.days.map((d) => sc.dayFiles.get(d)).filter((d): d is ScorecardDay => d !== undefined) : []),
    [latest, sc.dayFiles],
  );

  if (!latest) {
    return (
      <EmptyState>
        {sc.status === "unavailable"
          ? "Scorecard not available on this server yet."
          : sc.status === "error"
            ? `Scorecard unavailable: ${sc.error ?? "request failed"}`
            : "Loading scorecard…"}
      </EmptyState>
    );
  }

  const rolling = selection === ROLLING;
  let panels: ScorecardPanel[] | null;
  let sampling: ScorecardSampling | null;
  let source: ScorecardSource | null;
  if (rolling) {
    panels = latest.rolling.panels;
    sampling = latest.latest.sampling;
    source = latest.latest.source;
  } else if (selection === latest.latest.day) {
    panels = latest.latest.panels;
    sampling = latest.latest.sampling;
    source = latest.latest.source;
  } else {
    const file = sc.dayFiles.get(selection);
    panels = file?.panels ?? null;
    sampling = file?.sampling ?? null;
    source = file?.source ?? null;
  }
  const dayError = !rolling ? sc.dayErrors.get(selection) : undefined;
  const rows = panels ? scorecardRows(panels) : [];
  const main = rows.filter((r) => r.group === "main");
  const other = rows.filter((r) => r.group === "other");
  const sampled = samplingText(sampling, rolling ? latest.latest.day : selection);
  const sourceNote = source ? sourceText(source) : null;

  return (
    <div className="scorecard-body">
      <section className="scorecard-header" aria-label="About these numbers">
        <p className="scorecard-window">
          <strong>{rolling ? windowText(latest.rolling.days) : `One session: ${formatMarketDay(selection, true)}`}</strong>
          <span>Generated {formatGeneratedAt(latest.generatedAt)}</span>
        </p>
        <p className="scorecard-method">{methodText(latest.method)}.</p>
        {!rolling && (
          <p className="scorecard-note">
            A single day never gets a verdict — one day is one market mood.{" "}
            <Button variant="ghost" size="xs" className="scorecard-link" onClick={props.onBackToRolling}>Back to the rolling summary</Button>
          </p>
        )}
        {sampled && <p className="scorecard-note">{sampled}{rolling ? " Earlier days may be sampled too." : ""}</p>}
        {sourceNote && <p className="scorecard-note">{rolling ? `Latest day (${formatMarketDay(latest.latest.day)}): ` : ""}{sourceNote}</p>}
        {sc.error && <div className="panel-warning">Couldn't refresh ({sc.error}) — showing the last scorecard received.</div>}
        <details className="scorecard-rules">
          <summary>How the verdict works</summary>
          <ul>
            {verdictRules(latest.method).map((line) => <li key={line}>{line}</li>)}
          </ul>
        </details>
      </section>

      {panels === null ? (
        <EmptyState>{dayError ? `Couldn't load ${formatMarketDay(selection, true)}: ${dayError}` : `Loading ${formatMarketDay(selection, true)}…`}</EmptyState>
      ) : (
        <>
          <ul className="scorecard-rows" aria-label="Dashboard panels">
            {main.map((r) => <PanelCard key={r.key} row={r} trend={rolling ? panelTrend(trendDays, r.key) : null} />)}
          </ul>
          {other.length > 0 && (
            <>
              <h3 className="scorecard-group">Other detectors</h3>
              <ul className="scorecard-rows" aria-label="Other detectors">
                {other.map((r) => <PanelCard key={r.key} row={r} trend={rolling ? panelTrend(trendDays, r.key) : null} />)}
              </ul>
            </>
          )}
        </>
      )}
      <p className="journal-footer">Past detections only. A panel that beats random here is still not a trade signal.</p>
    </div>
  );
}

function PanelCard(props: { row: ScorecardRow; trend: TrendPoint[] | null }) {
  const { row } = props;
  const s = row.stats;
  if (!s) {
    return (
      <li className="scorecard-row">
        <div className="scorecard-row-head">
          <h4>{row.title}</h4>
          <Badge variant="outline" className="scorecard-verdict scorecard-verdict-muted">No data</Badge>
        </div>
        <p className="scorecard-reason">{notRecordedText()}</p>
      </li>
    );
  }
  const v = verdictView(s.verdict);
  const h = primaryStats(s);
  return (
    <li className="scorecard-row">
      <div className="scorecard-row-head">
        <h4>{row.title}</h4>
        <Badge variant="outline" className={`scorecard-verdict scorecard-verdict-${v.tone}`}>{v.label}</Badge>
      </div>
      <p className="scorecard-diff">{differenceText(h)}</p>
      <dl className="scorecard-facts">
        <div><dt>{s.primaryHorizon} avg after costs</dt><dd>{formatNetPct(h?.meanNetPct ?? null)}</dd></div>
        <div><dt>Random avg</dt><dd>{formatNetPct(h?.baselineMeanNetPct ?? null)}</dd></div>
        <div><dt>Hit rate</dt><dd>{formatRate(h?.hitRate ?? null)}</dd></div>
        <div className="scorecard-fact-wide"><dt>Sample</dt><dd>{sampleText(s)}</dd></div>
      </dl>
      {props.trend && props.trend.length > 1 && <TrendBars points={props.trend} />}
      <p className="scorecard-reason">{s.verdictReason}</p>
    </li>
  );
}

const TREND_W = 140;
const TREND_H = 30;

/** One bar per scored day: panel minus random at the primary horizon, up
 * or down from a zero line. Neutral colour on purpose -- direction from the
 * line carries the sign, the text alternative carries the values. */
function TrendBars(props: { points: TrendPoint[] }) {
  const pts = props.points;
  const max = Math.max(0.5, ...pts.map((p) => Math.abs(p.diffPct ?? 0)));
  const slot = TREND_W / pts.length;
  const barW = Math.max(2, Math.min(10, slot - 2));
  const mid = TREND_H / 2;
  const label = trendText(pts);
  return (
    <div className="scorecard-trend">
      <span className="scorecard-trend-label">Per day vs random</span>
      <svg width={TREND_W} height={TREND_H} role="img" aria-label={label}>
        <title>{label}</title>
        <line x1={0} x2={TREND_W} y1={mid} y2={mid} className="scorecard-trend-zero" />
        {pts.map((p, i) => {
          if (p.diffPct === null) return null;
          const hgt = Math.max(1, (Math.abs(p.diffPct) / max) * (mid - 1));
          return (
            <rect
              key={p.day}
              x={i * slot + (slot - barW) / 2}
              y={p.diffPct >= 0 ? mid - hgt : mid}
              width={barW}
              height={hgt}
              rx={1}
              className="scorecard-trend-bar"
            />
          );
        })}
      </svg>
    </div>
  );
}
