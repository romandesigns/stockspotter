// Markets Today panel -- the last real placeholder from the target
// layout, now backed by market_data::indices (4 index-proxy ETFs:
// SPY/QQQ/DIA/IWM) plus a real sparkline reusing the existing
// /bars/:symbol backfill endpoint. Per Roman's ask, the sparklines use a
// real shadcn chart (ChartContainer wrapping Recharts' AreaChart) rather
// than a hand-rolled canvas/SVG sparkline -- a new, explicit library
// choice for this panel, distinct from the Super Chart engine reuse
// elsewhere in this app (that's for the full interactive chart; this is
// a tiny non-interactive trend strip, a genuinely different job).

import { Area, AreaChart } from "recharts";
import { ChartContainer, type ChartConfig } from "@/components/ui/chart";
import { formatPct, formatPrice } from "../../lib/format";
import type { MarketIndexReading, SparkPoint } from "../../lib/useMarketsToday";
import { EmptyState, PanelShell } from "../PanelShell";

const CHART_CONFIG: ChartConfig = { price: { label: "Price" } };

function IndexCard(props: { reading: MarketIndexReading; spark: SparkPoint[] }) {
  const up = props.reading.changePct >= 0;
  const color = up ? "var(--good)" : "var(--critical)";
  const gradientId = `spark-${props.reading.symbol}`;

  return (
    <div className="index-card">
      <div className="index-card-head">
        <span className="index-card-symbol">{props.reading.symbol}</span>
        <span className="dim">{props.reading.name}</span>
      </div>
      <div className="index-card-price">
        <span className="index-card-value">{formatPrice(props.reading.price)}</span>
        <span className={up ? "pct-up" : "pct-down"}>{formatPct(props.reading.changePct)}</span>
      </div>
      {props.spark.length > 1 && (
        <ChartContainer config={CHART_CONFIG} className="index-card-spark">
          <AreaChart data={props.spark} margin={{ top: 2, right: 0, bottom: 0, left: 0 }}>
            <defs>
              <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor={color} stopOpacity={0.35} />
                <stop offset="95%" stopColor={color} stopOpacity={0} />
              </linearGradient>
            </defs>
            <Area
              type="monotone"
              dataKey="price"
              stroke={color}
              strokeWidth={1.5}
              fill={`url(#${gradientId})`}
              isAnimationActive={false}
              dot={false}
            />
          </AreaChart>
        </ChartContainer>
      )}
    </div>
  );
}

export function MarketsTodayPanel(props: {
  readings: MarketIndexReading[];
  sparklines: Map<string, SparkPoint[]>;
  className?: string;
}) {
  return (
    <PanelShell title="Markets Today" subtitle="index proxies, live" className={props.className}>
      {props.readings.length === 0 ? (
        <EmptyState>Waiting for index snapshots…</EmptyState>
      ) : (
        <div className="markets-today-grid">
          {props.readings.map((r) => (
            <IndexCard key={r.symbol} reading={r} spark={props.sparklines.get(r.symbol) ?? []} />
          ))}
        </div>
      )}
    </PanelShell>
  );
}
