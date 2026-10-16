// Confirmed bullish momentum panel (doc Panels #2) -- the symbols whose
// momentum score crossed into qualifying territory (see
// deriveConfirmedMomentum), one card each, strongest first. A card reads
// at a glance: a radial gauge with the score and its strength band
// (Mild / Solid / Strong, coloured dim to bright green), the ticker, a
// News badge, and the one or two factors driving it as badges. The four
// factors themselves sit in a hover card as a small bar chart. Click a
// card to open that symbol's chart.

import type { CatalystUpdate, MomentumUpdate } from "@stockspotter/shared-types";
import { Bar, BarChart, LabelList, PolarAngleAxis, RadialBar, RadialBarChart, XAxis } from "recharts";
import { Badge } from "@/components/ui/badge";
import { ChartContainer, type ChartConfig } from "@/components/ui/chart";
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";
import { EmptyState, PanelShell } from "../PanelShell";
import { bandColor, MOMENTUM_BANDS, momentumCards, type MomentumCard } from "../../lib/momentumChart";
import { formatEtTime } from "../../lib/shortlist";

const GAUGE_CONFIG: ChartConfig = { score: { label: "Momentum score" } };
const FACTOR_CONFIG: ChartConfig = { value: { label: "Factor" } };
const GAUGE_TRACK = "#2c2e3a";

function ScoreGauge(props: { card: MomentumCard }) {
  const color = bandColor(props.card.band);
  return (
    <div className="momentum-gauge" role="img" aria-label={`Momentum score ${props.card.score.toFixed(2)}, ${props.card.band}`}>
      <ChartContainer config={GAUGE_CONFIG} className="momentum-gauge-chart">
        <RadialBarChart data={[{ score: props.card.score }]} startAngle={90} endAngle={-270} innerRadius="78%" outerRadius="100%" barSize={7}>
          <PolarAngleAxis type="number" domain={[0, 1]} tick={false} axisLine={false} />
          <RadialBar dataKey="score" fill={color} background={{ fill: GAUGE_TRACK }} cornerRadius={8} isAnimationActive={false} />
        </RadialBarChart>
      </ChartContainer>
      <span className="momentum-gauge-label" aria-hidden="true">
        <span className="momentum-gauge-score">{props.card.score.toFixed(2)}</span>
        <span className="momentum-gauge-band" style={{ color }}>{props.card.band}</span>
      </span>
    </div>
  );
}

function FactorChart(props: { card: MomentumCard }) {
  const data = props.card.factors.map((f) => ({ name: f.short, value: f.value }));
  return (
    <ChartContainer config={FACTOR_CONFIG} className="momentum-factor-chart">
      <BarChart data={data} margin={{ top: 18, right: 4, bottom: 0, left: 4 }}>
        <XAxis dataKey="name" tickLine={false} axisLine={false} interval={0} tick={{ fill: "var(--text)", fontSize: 11 }} />
        <Bar dataKey="value" fill={bandColor(props.card.band)} radius={[4, 4, 0, 0]} maxBarSize={24} isAnimationActive={false}>
          <LabelList dataKey="value" position="top" formatter={(v: unknown) => (typeof v === "number" ? `${Math.round(v * 100)}%` : "")} className="momentum-factor-label" />
        </Bar>
      </BarChart>
    </ChartContainer>
  );
}

function MomentumCardView(props: { card: MomentumCard; catalyst?: CatalystUpdate; onSelectSymbol: (symbol: string) => void }) {
  const { card, catalyst } = props;
  const news = catalyst && catalyst.catalystTags.length > 0 ? catalyst.catalystTags.join(", ").replaceAll("_", " ") : undefined;
  return (
    <HoverCard openDelay={150} closeDelay={80}>
      <HoverCardTrigger asChild>
        <button type="button" className="momentum-card" onClick={() => props.onSelectSymbol(card.symbol)} aria-label={`${card.symbol}: momentum ${card.score.toFixed(2)}, ${card.band}. Open chart`}>
          <ScoreGauge card={card} />
          <span className="momentum-card-body">
            <span className="momentum-card-head">
              <span className="momentum-card-symbol">{card.symbol}</span>
              {news && <Badge className="momentum-news-badge">News</Badge>}
            </span>
            <span className="momentum-card-drivers">
              {card.drivers.map((d) => <Badge key={d} variant="secondary" className="momentum-driver-badge">{d}</Badge>)}
            </span>
          </span>
        </button>
      </HoverCardTrigger>
      <HoverCardContent className="momentum-hover" side="right" align="start" collisionPadding={8}>
        <div className="momentum-hover-head">
          <span className="momentum-hover-symbol">{card.symbol}</span>
          <span className="dim">Confirmed {formatEtTime(card.timestamp)}</span>
        </div>
        <FactorChart card={card} />
        {news && <p className="momentum-hover-news">News: {news}</p>}
        <p className="dim momentum-hover-hint">Click to open chart</p>
      </HoverCardContent>
    </HoverCard>
  );
}

export function MomentumPanel(props: {
  confirmations: MomentumUpdate[];
  pricesBySymbol: Map<string, number>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const cards = momentumCards(props.confirmations, props.pricesBySymbol);
  const legend = (
    <span className="momentum-legend">
      {MOMENTUM_BANDS.map((b) => (
        <span key={b.band} className="momentum-legend-item">
          <span className="momentum-legend-swatch" style={{ background: b.color }} />
          {b.band} <span className="dim">{b.range}</span>
        </span>
      ))}
    </span>
  );
  return (
    <PanelShell title="Bullish Momentum" subtitle="Prices < $20 · strongest first · hover a card for details" count={cards.length} className={props.className}>
      {cards.length === 0 ? (
        <EmptyState>No confirmed momentum stocks with an available price under $20.</EmptyState>
      ) : (
        <div className="momentum-panel">
          {legend}
          <div className="momentum-grid">
            {cards.map((card) => (
              <MomentumCardView key={card.symbol} card={card} catalyst={props.catalystsBySymbol.get(card.symbol)} onSelectSymbol={props.onSelectSymbol} />
            ))}
          </div>
        </div>
      )}
    </PanelShell>
  );
}
