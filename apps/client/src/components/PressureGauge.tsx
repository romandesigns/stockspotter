// Radial "pressure" gauge for the Halt Early-Warning panel's
// proximityRatio (how close the current move is to the LULD halt band)
// -- a real Recharts component (RadialBarChart, same shadcn ChartContainer
// wrapper MarketsTodayPanel's sparklines already use for this app's
// "small non-interactive chart" jobs), not a re-derived hand-rolled
// gauge, and not the flat linear bar this replaces. A circular gauge is
// the more intuitive shape for "pressure toward a threshold" than a
// linear fill -- it reads like a speedometer/pressure dial, matching
// the "momentum pressure" framing directly, and the same calm/amber/red
// escalation colors this app already uses everywhere else carry over
// (no new color language introduced).

import { Label, PolarRadiusAxis, RadialBar, RadialBarChart } from "recharts";
import type { HaltAlertLevel } from "@stockspotter/shared-types";
import { ChartContainer, type ChartConfig } from "./ui/chart";

const LEVEL_COLOR: Record<HaltAlertLevel, string> = {
  calm: "var(--good)",
  amber: "var(--warning)",
  red: "var(--critical)",
};

const CHART_CONFIG: ChartConfig = { pressure: { label: "Pressure" } };

export function PressureGauge(props: { proximityRatio: number; level: HaltAlertLevel }) {
  // proximityRatio can exceed 1 (price at/past the band) -- clamped to
  // 100 for the dial's own sake, same clamp the old linear gauge used.
  const pct = Math.min(100, Math.round(props.proximityRatio * 100));
  const color = LEVEL_COLOR[props.level];
  const data = [{ pressure: pct, fill: color }];

  return (
    <ChartContainer config={CHART_CONFIG} className="pressure-gauge">
      <RadialBarChart data={data} startAngle={90} endAngle={-270} innerRadius="70%" outerRadius="100%" barSize={6}>
        <PolarRadiusAxis type="number" domain={[0, 100]} tick={false} tickLine={false} axisLine={false}>
          <Label
            content={({ viewBox }) => {
              if (!viewBox || !("cx" in viewBox) || viewBox.cx == null || viewBox.cy == null) return null;
              return (
                <text x={viewBox.cx} y={viewBox.cy} textAnchor="middle" dominantBaseline="middle" className="pressure-gauge-label" fill={color}>
                  {pct}%
                </text>
              );
            }}
          />
        </PolarRadiusAxis>
        <RadialBar dataKey="pressure" background={{ fill: "var(--bg)" }} cornerRadius={3} isAnimationActive={false} />
      </RadialBarChart>
    </ChartContainer>
  );
}
