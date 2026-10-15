// Decision card for the selected shortlist candidate -- everything worth
// knowing before deciding whether to even watch it closely, in five fixed
// sections: why it's on the list, whether it is practically tradable,
// the levels that matter, a sizing plan (PlanCalculator.tsx), and its
// track record: how often this setup profile followed through
// historically, with its sample size. Nothing on this card is an entry
// signal.

import type { CatalystUpdate, FunnelSignal, ShortlistCandidate } from "@stockspotter/shared-types";
import { assessTradability } from "@stockspotter/shared-types";
import type { ReactNode } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { EmptyState } from "../PanelShell";
import { RossFivePillarsBadge } from "../RossFivePillarsBadge";
import { TickerButton } from "../TickerButton";
import { formatPct, formatPrice, formatTag, formatTime, formatVolume } from "../../lib/format";
import { candidateCatalyst, candidateFunnel, formatAge, formatDollars, formatEtTime } from "../../lib/shortlist";
import { PlanCalculator } from "./PlanCalculator";
import { TradabilityBadge } from "./TradabilityDot";

const NONE = "—";

function Section(props: { title: string; extra?: ReactNode; children: ReactNode }) {
  return (
    <section className="decision-section">
      <header className="decision-section-header">
        <h3>{props.title}</h3>
        {props.extra}
      </header>
      {props.children}
    </section>
  );
}

function Facts(props: { children: ReactNode }) {
  return <dl className="decision-facts">{props.children}</dl>;
}

function Fact(props: { label: string; children: ReactNode }) {
  return (
    <div className="decision-fact">
      <dt>{props.label}</dt>
      <dd>{props.children}</dd>
    </div>
  );
}

const price = (n: number | undefined) => (n == null || !Number.isFinite(n) ? NONE : formatPrice(n));

function WhyItsHere(props: { c: ShortlistCandidate; funnel: FunnelSignal; catalyst: CatalystUpdate | undefined }) {
  const { c } = props;
  const tags = props.catalyst?.catalystTags ?? [];
  const headline = props.catalyst?.mostRecentHeadline;
  const age = formatAge(props.catalyst?.mostRecentPublishedAt);
  return (
    <Section title="Why it's here" extra={<RossFivePillarsBadge funnel={props.funnel} catalyst={props.catalyst} />}>
      <Facts>
        <Fact label="Day change"><span className={c.gapPct >= 0 ? "pct-up" : "pct-down"}>{formatPct(c.gapPct)}</span></Fact>
        <Fact label="Relative volume"><span className="price">{c.relativeVolume.toFixed(1)}×</span></Fact>
        <Fact label="Float"><span className="price">{c.floatShares != null ? formatVolume(c.floatShares) : "unknown"}</span></Fact>
        <Fact label="Session volume"><span className="price">{formatVolume(c.sessionVolume)}</span></Fact>
      </Facts>
      <div className="decision-chips">
        <span
          className={`chip ${c.funnelPassed ? "chip-good" : "chip-bad"}`}
          title={c.funnelAt ? `Fast-funnel verdict at ${formatTime(c.funnelAt)}` : undefined}
        >
          {c.funnelPassed ? "funnel passed" : "funnel not passed"}
        </span>
        {tags.map((tag) => (
          <span key={tag} className="chip chip-accent decision-tag">{formatTag(tag)}</span>
        ))}
      </div>
      {headline ? (
        <p className="decision-headline">
          {headline}
          {age && <span className="dim"> · {age}</span>}
        </p>
      ) : (
        <p className="decision-note">No catalyst on record — that is not proof there is no news.</p>
      )}
    </Section>
  );
}

function CanITradeIt(props: { c: ShortlistCandidate }) {
  const { c } = props;
  const assessment = assessTradability(c);
  const quote = c.quote;
  const halt = c.halt;
  const haltNotes = halt ? [halt.estimated && "estimated band", !halt.luldInEffect && "outside LULD hours"].filter(Boolean).join(" · ") : "";
  return (
    <Section title="Can I trade it?" extra={<TradabilityBadge assessment={assessment} />}>
      {assessment.reasons.length > 0 && (
        <ul className="decision-reasons">
          {assessment.reasons.map((r) => <li key={r}>{r}</li>)}
        </ul>
      )}
      <Facts>
        <Fact label="Bid / ask">
          {quote ? (
            <span className="price">
              {formatPrice(quote.bid)} / {formatPrice(quote.ask)}
              <span className="dim"> · spread {quote.spreadPct.toFixed(2)}% · {Math.round(quote.ageSecs)}s old</span>
            </span>
          ) : (
            "no quote"
          )}
        </Fact>
        <Fact label="Dollar volume">
          <span className="price">{c.levels.dollarVolume != null ? formatDollars(c.levels.dollarVolume) : "unknown"}</span>
        </Fact>
        <Fact label="Halt band">
          {halt ? (
            <span>
              <span className={`chip decision-halt-${halt.level}`}>{halt.level}</span>{" "}
              <span className="price">{halt.distanceToUpperPct.toFixed(1)}% to upper</span>
              {haltNotes && <span className="dim"> · {haltNotes}</span>}
            </span>
          ) : (
            "no halt reading"
          )}
        </Fact>
      </Facts>
      <p className="decision-note">Heuristic thresholds (spread, liquidity, halt proximity), not backtested.</p>
    </Section>
  );
}

function KeyLevels(props: { c: ShortlistCandidate }) {
  const { levels } = props.c;
  const halt = props.c.halt;
  return (
    <Section title="Key levels">
      {!levels.barsComplete && (
        <p className="decision-note decision-note-warning">
          Partial session data{levels.barsFrom ? ` — bars start at ${formatEtTime(levels.barsFrom)}` : ""}, so premarket levels and VWAP may be incomplete.
        </p>
      )}
      <Facts>
        <Fact label="Premarket high"><span className="price">{price(levels.premarketHigh)}</span></Fact>
        <Fact label="Premarket low"><span className="price">{price(levels.premarketLow)}</span></Fact>
        <Fact label="VWAP">
          <span className="price">
            {price(levels.vwap)}
            {levels.vwap != null && levels.vwapAnchor && <span className="dim"> · from {levels.vwapAnchor === "premarket" ? "premarket" : "the open"}</span>}
          </span>
        </Fact>
        <Fact label="Day high"><span className="price">{price(levels.dayHigh)}</span></Fact>
        <Fact label="Halt upper band"><span className="price">{price(halt?.upperBand)}</span></Fact>
        <Fact label="Halt lower band"><span className="price">{price(halt?.lowerBand)}</span></Fact>
      </Facts>
    </Section>
  );
}

const pct = (rate: number) => `${Math.round(rate * 100)}%`;

// The measured history of this setup profile, stated as frequencies with
// their sample size -- never a score. Below the minimum sample the card
// shows the count and says the evidence is too thin.
function TrackRecordSection(props: { c: ShortlistCandidate }) {
  const tr = props.c.trackRecord;
  if (!tr) {
    return (
      <Section title="Track record">
        <p className="decision-note">Historical base rate for this setup: not measured yet.</p>
      </Section>
    );
  }
  return (
    <Section title="Track record">
      <p className="decision-note">
        {tr.profile}, bought at the ask at {tr.decisionTimeEt} ET, spread paid both ways.
      </p>
      {tr.sufficient && tr.targetRate !== undefined && tr.stopFirstRate !== undefined ? (
        <Facts>
          <Fact label={`Reached +${tr.targetPct}% first`}><span className="price">{pct(tr.targetRate)}</span></Fact>
          <Fact label={`Hit −${tr.stopPct}% first`}><span className="price">{pct(tr.stopFirstRate)}</span></Fact>
          <Fact label="Median result at close">
            <span className={(tr.medianCloseReturnNetPct ?? 0) < 0 ? "pct-down" : "pct-up"}>{tr.medianCloseReturnNetPct !== undefined ? formatPct(tr.medianCloseReturnNetPct) : "—"}</span>
          </Fact>
          <Fact label="Median drawdown first"><span className="price">{tr.medianMaxAdversePct !== undefined ? formatPct(tr.medianMaxAdversePct) : "—"}</span></Fact>
        </Facts>
      ) : (
        <p className="decision-note decision-note-warning">Only {tr.n} past case{tr.n === 1 ? "" : "s"} like this — too few to state a rate.</p>
      )}
      <p className="decision-note dim">
        {tr.sufficient ? `${tr.n} past cases. ` : ""}{tr.sourceNote}
      </p>
    </Section>
  );
}

export function DecisionCard(props: {
  /** The symbol in focus -- may be one that isn't on the shortlist. */
  symbol: string | null;
  candidate: ShortlistCandidate | null;
  generatedAt: string | null;
  liveFunnel?: FunnelSignal;
  liveCatalyst?: CatalystUpdate;
  onSelectSymbol: (symbol: string) => void;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
}) {
  const c = props.candidate;
  if (!c) {
    return (
      <Card className="decision-card">
        <EmptyState>
          {props.symbol ? `${props.symbol} isn't on today's shortlist.` : "Select a candidate to see why it's on the list."}
        </EmptyState>
      </Card>
    );
  }

  const funnel = candidateFunnel(c, props.liveFunnel);
  const catalyst = candidateCatalyst(c, props.generatedAt ?? c.funnelAt, props.liveCatalyst);

  return (
    <Card className="decision-card">
      <CardHeader className="decision-card-header">
        <CardTitle className="decision-card-title">
          <span className="dim">#{c.rank}</span>
          <TickerButton symbol={c.symbol} onSelectSymbol={props.onSelectSymbol} saved={props.saved.has(c.symbol)} onToggleSaved={props.onToggleSaved} />
          <span className="price">{formatPrice(c.price)}</span>
        </CardTitle>
      </CardHeader>
      <ScrollArea className="decision-card-scroll">
        {/* key restarts the fade when the selection changes -- opacity
            only, so the swap never shifts layout. */}
        <CardContent key={c.symbol} className="decision-card-content">
          <WhyItsHere c={c} funnel={funnel} catalyst={catalyst} />
          <CanITradeIt c={c} />
          <KeyLevels c={c} />
          <Section title="Plan">
            <PlanCalculator key={c.symbol} c={c} />
          </Section>
          <TrackRecordSection c={c} />
          <p className="decision-disclaimer">A watchlist ranking, not an entry signal. Not financial advice.</p>
        </CardContent>
      </ScrollArea>
    </Card>
  );
}
