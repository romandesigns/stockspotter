// Confirmed bullish momentum panel (doc Panels #2) -- one card per symbol
// that crossed into qualifying momentum, showing its CURRENT reading:
// a radial gauge with the score and strength band (Mild / Solid / Strong,
// dim to bright green), the ticker, price and day change, status badges
// (Faded, Pre-market / After hours, Shortlist #N, News) and the one or
// two factors driving it. Details -- the four factors as a small bar
// chart, how past crossings of the same band turned out, and timing --
// open on hover, or from the info button for touch and keyboard. All the
// logic lives in lib/momentumChart.ts and lib/momentumTrackRecord.ts.

import type { CatalystUpdate, FunnelSignal, MomentumUpdate, ShortlistResponse } from "@stockspotter/shared-types";
import { SCAN_PRICE_LABEL } from "@stockspotter/shared-types";
import { Info } from "lucide-react";
import { useEffect, useState } from "react";
import { Bar, BarChart, LabelList, PolarAngleAxis, RadialBar, RadialBarChart, XAxis } from "recharts";
import { Badge } from "@/components/ui/badge";
import { ChartContainer, type ChartConfig } from "@/components/ui/chart";
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { EmptyState, PanelShell } from "../PanelShell";
import { TradabilityDot } from "../shortlist/TradabilityDot";
import { formatPct, formatPrice } from "../../lib/format";
import {
  bandColor,
  confirmedAgo,
  formatMinutes,
  hiddenSummary,
  MOMENTUM_BANDS,
  momentumCards,
  SESSION_BADGE,
  type MomentumCard,
} from "../../lib/momentumChart";
import { bandTrackRecordLine, legendTrackRecordLine, type MomentumTrackRecord } from "../../lib/momentumTrackRecord";
import { formatEtTime } from "../../lib/shortlist";
import { useMomentumReference } from "../../lib/useMomentumReference";

const GAUGE_CONFIG: ChartConfig = { score: { label: "Momentum score" } };
const FACTOR_CONFIG: ChartConfig = { value: { label: "Factor" } };
const GAUGE_TRACK = "#2c2e3a";
const BAND_NAMES = MOMENTUM_BANDS.map((b) => b.band);

/** Re-renders every 30 s so ages, staleness and fade expiry keep moving without new data. */
function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

function ScoreGauge(props: { card: MomentumCard }) {
  const color = bandColor(props.card.band);
  const bandText = props.card.band ?? "Below";
  return (
    <span className="momentum-gauge" role="img" aria-label={`Momentum score ${props.card.score.toFixed(2)}, ${bandText}`}>
      <ChartContainer config={GAUGE_CONFIG} className="momentum-gauge-chart">
        <RadialBarChart data={[{ score: props.card.score }]} startAngle={90} endAngle={-270} innerRadius="78%" outerRadius="100%" barSize={7}>
          <PolarAngleAxis type="number" domain={[0, 1]} tick={false} axisLine={false} />
          <RadialBar dataKey="score" fill={color} background={{ fill: GAUGE_TRACK }} cornerRadius={8} isAnimationActive={false} />
        </RadialBarChart>
      </ChartContainer>
      <span className="momentum-gauge-label" aria-hidden="true">
        <span className="momentum-gauge-score">{props.card.score.toFixed(2)}</span>
        <span className="momentum-gauge-band" style={{ color }}>{bandText}</span>
      </span>
    </span>
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

/** Shared by the hover card and the info popover. */
function MomentumDetails(props: { card: MomentumCard; news?: string; trackRecord: MomentumTrackRecord | null }) {
  const { card } = props;
  return (
    <div className="momentum-details">
      <div className="momentum-hover-head">
        <span className="momentum-hover-symbol">{card.symbol}</span>
        <span className="dim">Confirmed {formatEtTime(card.confirmedAt)}</span>
      </div>
      <p className="momentum-details-line">
        Crossed at {card.confirmedScore.toFixed(2)} ({card.confirmedBand}); now {card.score.toFixed(2)} as of {formatEtTime(card.readingAt)}.
        {card.status === "faded" && card.fadedAt && ` Fell below 0.60 at ${formatEtTime(card.fadedAt)}.`}
      </p>
      {card.staleMinutes !== null && <p className="momentum-details-line momentum-stale">No update for {formatMinutes(card.staleMinutes)}.</p>}
      <FactorChart card={card} />
      <p className="dim momentum-details-note">Buying = share of volume on up-candles, last 30 min. Trend 0% can mean under 10 minutes of bars.</p>
      <p className="momentum-details-line momentum-track">{bandTrackRecordLine(props.trackRecord, card.confirmedBand)}</p>
      {props.news && <p className="momentum-hover-news">News: {props.news}</p>}
      <p className="dim momentum-hover-hint">Select the card to open the chart</p>
    </div>
  );
}

function MomentumCardView(props: {
  card: MomentumCard;
  catalyst?: CatalystUpdate;
  trackRecord: MomentumTrackRecord | null;
  nowMs: number;
  onSelectSymbol: (symbol: string) => void;
}) {
  const { card, catalyst } = props;
  const [hoverOpen, setHoverOpen] = useState(false);
  const [popoverOpen, setPopoverOpen] = useState(false);
  const news = catalyst && catalyst.catalystTags.length > 0 ? catalyst.catalystTags.join(", ").replaceAll("_", " ") : undefined;
  const sessionBadge = SESSION_BADGE[card.session];
  const details = <MomentumDetails card={card} news={news} trackRecord={props.trackRecord} />;
  const statusText = card.status === "faded" ? ", faded" : "";
  return (
    // The hover card yields to the popover so the same details never show twice.
    <HoverCard open={hoverOpen && !popoverOpen} onOpenChange={setHoverOpen} openDelay={150} closeDelay={80}>
      <HoverCardTrigger asChild>
        <div className="momentum-card" data-status={card.status} data-open={hoverOpen || popoverOpen ? "true" : undefined}>
          <button
            type="button"
            className="momentum-card-main"
            onClick={() => props.onSelectSymbol(card.symbol)}
            aria-label={`${card.symbol}: momentum ${card.score.toFixed(2)}, ${card.band ?? "below 0.60"}${statusText}. Open chart`}
          >
            <ScoreGauge card={card} />
            <span className="momentum-card-body">
              <span className="momentum-card-head">
                <span className="momentum-card-symbol">{card.symbol}</span>
                {card.status === "faded" && <Badge variant="outline" className="momentum-faded-badge">Faded</Badge>}
                {sessionBadge && <Badge variant="outline" className="momentum-session-badge">{sessionBadge}</Badge>}
                {card.shortlist && (
                  <Badge variant="outline" className="momentum-shortlist-badge">
                    <TradabilityDot assessment={card.shortlist.tradability} />
                    Shortlist #{card.shortlist.rank}
                  </Badge>
                )}
                {news && <Badge className="momentum-news-badge">News</Badge>}
              </span>
              <span className="momentum-card-meta">
                {card.price !== undefined && <span className="momentum-card-price">{formatPrice(card.price)}</span>}
                {card.dayChangePct !== undefined && (
                  <span className={card.dayChangePct >= 0 ? "momentum-card-up" : "momentum-card-down"}>{formatPct(card.dayChangePct)}</span>
                )}
                <span className="dim" title={`Confirmed ${formatEtTime(card.confirmedAt)}`}>{confirmedAgo(card.confirmedAt, props.nowMs)}</span>
              </span>
              {card.staleMinutes !== null && <span className="momentum-stale">No update for {formatMinutes(card.staleMinutes)}</span>}
              <span className="momentum-card-drivers">
                {card.drivers.map((d) => <Badge key={d} variant="secondary" className="momentum-driver-badge">{d}</Badge>)}
              </span>
            </span>
          </button>
          <Popover open={popoverOpen} onOpenChange={setPopoverOpen}>
            <PopoverTrigger asChild>
              <button type="button" className="momentum-card-info" aria-label={`Details for ${card.symbol}`}>
                <Info size={15} aria-hidden="true" />
              </button>
            </PopoverTrigger>
            <PopoverContent className="momentum-hover" side="right" align="start" collisionPadding={8}>
              {details}
            </PopoverContent>
          </Popover>
        </div>
      </HoverCardTrigger>
      <HoverCardContent className="momentum-hover" side="right" align="start" collisionPadding={8}>
        {details}
      </HoverCardContent>
    </HoverCard>
  );
}

export function MomentumPanel(props: {
  confirmations: MomentumUpdate[];
  momentumBySymbol: Map<string, MomentumUpdate>;
  fadesBySymbol: Map<string, MomentumUpdate>;
  pricesBySymbol: Map<string, number>;
  funnelBySymbol: Map<string, FunnelSignal>;
  shortlist: ShortlistResponse | null;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const nowMs = useNow();
  const { funds, trackRecord } = useMomentumReference();
  const { cards, hidden } = momentumCards({
    confirmations: props.confirmations,
    current: props.momentumBySymbol,
    fades: props.fadesBySymbol,
    prices: props.pricesBySymbol,
    funnelBySymbol: props.funnelBySymbol,
    funds,
    shortlist: props.shortlist,
    nowMs,
  });
  const hiddenLine = hiddenSummary(hidden);
  const legend = (
    <div className="momentum-legend-block">
      <span className="momentum-legend">
        {MOMENTUM_BANDS.map((b) => (
          <span key={b.band} className="momentum-legend-item" title={bandTrackRecordLine(trackRecord, b.band)}>
            <span className="momentum-legend-swatch" style={{ background: b.color }} />
            {b.band} <span className="dim">{b.range}</span>
          </span>
        ))}
      </span>
      <span className="dim momentum-legend-record" title={trackRecord?.sourceNote || undefined}>{legendTrackRecordLine(trackRecord, BAND_NAMES)}</span>
    </div>
  );
  return (
    <PanelShell title="Bullish Momentum" subtitle={`Prices ${SCAN_PRICE_LABEL} · current reading · strongest first`} count={cards.length} className={props.className}>
      {cards.length === 0 ? (
        <EmptyState>
          No confirmed momentum stocks with an available price in {SCAN_PRICE_LABEL}.
          {hiddenLine && <span className="momentum-hidden-line">{hiddenLine}</span>}
        </EmptyState>
      ) : (
        <div className="momentum-panel">
          {legend}
          <div className="momentum-grid">
            {cards.map((card) => (
              <MomentumCardView
                key={card.symbol}
                card={card}
                catalyst={props.catalystsBySymbol.get(card.symbol)}
                trackRecord={trackRecord}
                nowMs={nowMs}
                onSelectSymbol={props.onSelectSymbol}
              />
            ))}
          </div>
          {hiddenLine && <p className="dim momentum-hidden-line">{hiddenLine}</p>}
        </div>
      )}
    </PanelShell>
  );
}
