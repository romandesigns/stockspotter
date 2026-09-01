// Radial "pressure" gauge for the Halt Early-Warning panel's
// proximityRatio (how close the current move is to the LULD halt band)
// -- a real Recharts component (RadialBarChart, same shadcn ChartContainer
// wrapper MarketsTodayPanel's sparklines already use for this app's
// "small non-interactive chart" jobs), not a re-derived hand-rolled
// gauge, and not the flat linear bar this replaces. A circular gauge is
// the more intuitive shape for "pressure toward a threshold" than a
// linear fill -- it reads like a speedometer/pressure dial, matching
// the "momentum pressure" framing directly.
//
// Color is keyed to direction (bullish/bearish), not calm/amber/red risk
// level -- the level is already the card's own background tint, and the
// dial's job is showing pressure + direction, not a second copy of the
// risk badge. Bullish -> --good, bearish -> --critical, matching the
// same price-text/left-border direction colors already on the card.
//
// Sweep shape ported from Roman's own mockup (two mirrored half-dials.
// each starting at the bottom center and filling up its own side): here,
// one dial carries both directions, so it starts at the bottom (6
// o'clock) and sweeps UP THE LEFT side (clockwise from bottom, toward 9
// o'clock) when bullish, or UP THE RIGHT side (counterclockwise from
// bottom, toward 3 o'clock) when bearish -- the same left/right split the
// mockup's two separate dials showed, just carried by one dial's
// direction of travel instead of which of two charts is drawn. A full
// 360° sweep either way at 100%, proportional at any other pct.
//
// The *unfilled* remainder of the ring is #16171d (--bg, the app's own
// base background) per Roman's explicit color pick for the mockup's gray
// track -- reads as "the rest of the dial, empty" rather than a second
// colored object; only the filled arc is meant to draw the eye, and its
// length is always proximityRatio's real share of the full ring,
// independent of color or direction.

import { Label, PolarAngleAxis, PolarRadiusAxis, RadialBar, RadialBarChart } from "recharts";
import { ChartContainer, type ChartConfig } from "./ui/chart";

const CHART_CONFIG: ChartConfig = { pressure: { label: "Pressure" } };
const BOTTOM = -90; // 6 o'clock, in recharts' polar-angle convention (0 = 3 o'clock, clockwise = decreasing)

export function PressureGauge(props: { proximityRatio: number; bullish: boolean }) {
  // proximityRatio can exceed 1 (price at/past the band) -- clamped to
  // 100 for the dial's own sake, same clamp the old linear gauge used.
  const pct = Math.min(100, Math.round(props.proximityRatio * 100));
  const color = props.bullish ? "var(--good)" : "var(--critical)";
  const data = [{ pressure: pct, fill: color }];
  // Bullish sweeps clockwise from the bottom (decreasing angle, toward
  // 9 o'clock first); bearish sweeps counterclockwise (increasing angle,
  // toward 3 o'clock first). Either way, a full revolution back to BOTTOM
  // at pct=100.
  const endAngle = props.bullish ? BOTTOM - 360 : BOTTOM + 360;

  return (
    <ChartContainer config={CHART_CONFIG} className="pressure-gauge">
      <RadialBarChart data={data} startAngle={BOTTOM} endAngle={endAngle} innerRadius="70%" outerRadius="100%" barSize={6}>
        {/* This is the actual value-to-sweep domain control -- NOT
            PolarRadiusAxis below (that scales radius, for concentric
            multi-ring charts; it silently did nothing useful here for
            the sweep). A single-item RadialBarChart with no
            PolarAngleAxis defaults its angle domain to [0, dataMax], and
            dataMax of a lone data point is that point's own value -- so
            every dial always filled the entire configured sweep
            regardless of pct until this was added (caught via isolated
            pct=5/50/95 comparisons; the label text was always right, the
            arc itself never was). No visible axis line/ticks -- purely
            here for its domain. */}
        <PolarAngleAxis type="number" domain={[0, 100]} tick={false} axisLine={false} />
        {/* This one IS still needed, but only for the centered %
            label's positioning (its viewBox gives the dial's real
            center) -- it carries no domain responsibility of its own. */}
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
