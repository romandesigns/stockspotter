// Order-flow strip under the chart: who is aggressive, whether it is
// accelerating, and whether it is actually moving price -- drawn as small
// charts and meters rather than a table of numbers, per the order-flow
// workspace design (the brief was "understood in about two seconds").
//
// RESEARCH DATA. Every value here is an unvalidated observational feature
// (see ../lib/orderFlow.ts); the strip carries a permanent "RESEARCH"
// chip and nothing in it feeds the momentum score, any alert, any ranking
// or the auto-trader.
//
// Three things the strip is required to say out loud, because each of them
// looks identical to its opposite on screen otherwise:
//
// - **coverage** -- a summary covering ten minutes looks exactly like one
//   covering the session;
// - **uncorrected tape** -- correction and cancel frames are counted, not
//   applied, so volume and the profile are pre-correction. The chip shows
//   the counts;
// - **chart-array age** -- the delta pane and the profile come from the
//   last full fetch and are carried forward across lite updates, so they
//   can be minutes older than the numbers beside them.
//
// Colour follows the app's existing rule: --good / --critical mean buy /
// sell, --warning means "look at this", and no accent is spent because
// nothing here is a selected or primary state. Buy and sell are never
// told apart by colour alone -- the sell bar is hatched, sides carry
// text labels, and levels carry their name next to the meter.
import type { ReactNode } from "react";
import {
  chartArrayLagSecs,
  cumulativeDeltaSeries,
  formatShares,
  formatSignedShares,
  formatWindowLabel,
  levelSegments,
  orderFlowWindowFor,
  sideShares,
  type OrderFlowDeltaBucket,
  type OrderFlowLevel,
} from "../lib/orderFlow";
import type { OrderFlowState } from "../lib/useOrderFlow";

const LEVEL_LABEL: Record<OrderFlowLevel, string> = { none: "NONE", low: "LOW", moderate: "MODERATE", high: "HIGH", extreme: "EXTREME" };

const COVERAGE_TITLE = {
  session: "The earlier trades of this session were fetched and replayed into this engine. Quotes were fetched for the recent window only, so older trades are classified from upticks and downticks alone.",
  session_pending: "Fetching the earlier part of this session; totals will extend back towards the open shortly",
  partial: "Covers only the time this server has had an engine for this symbol, not the whole session",
} as const;

const EMPTY_MESSAGE: Record<Exclude<OrderFlowState["status"], "ready" | "loading">, string> = {
  unavailable: "not provided by this server yet",
  disabled: "switched off on this server",
  waiting: "waiting for the first trades on this symbol",
};

/** Clock time for an epoch-seconds value, in the viewer's own zone. */
function clock(secs: number): string {
  return new Date(secs * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function OrderFlowStrip(props: {
  state: OrderFlowState;
  chartBucketMinutes: number;
  /** Header only: the tiles are hidden to give the chart their height. */
  collapsed: boolean;
  onToggleCollapsed: () => void;
}) {
  const { state } = props;
  if (state.status === "loading") return null;
  if (state.status !== "ready") {
    return (
      <div className="of-strip">
        <div className="of-header">
          <StripTitle />
          <span className="of-empty">{EMPTY_MESSAGE[state.status]}</span>
          <span className="of-header-spacer" />
        </div>
      </div>
    );
  }
  const s = state.summary;
  const w = orderFlowWindowFor(s, props.chartBucketMinutes);
  if (!w) return null;
  const shares = sideShares(w);
  const pressure = s.interpretation;
  const arrayLag = chartArrayLagSecs(s);
  const corrections = state.tape ? state.tape.correctionsSeen + state.tape.cancelsSeen : null;

  return (
    <div className="of-strip">
      <div className="of-header">
        <StripTitle />
        <span className="of-window">{formatWindowLabel(w.windowSecs)} window</span>
        {/* How far back cumulative delta and the profile reach. Said out
            loud because a partial session looks exactly like a full one. */}
        <span className="chip" title={COVERAGE_TITLE[state.coverage]}>
          {state.coverage === "session"
            ? "since open"
            : state.coverage === "session_pending"
              ? "loading session…"
              : s.profile
                ? `since ${clock(s.profile.sinceSecs)}`
                : "partial session"}
        </span>
        {/* Corrections and cancels are counted, never applied, so the
            volume and profile here are pre-correction. Shown with the
            counts rather than as a vague warning. */}
        <span
          className={corrections !== null && corrections > 0 ? "chip of-warning" : "chip"}
          title={
            corrections === null
              ? "Tape correction telemetry is unavailable. No zero-correction or official-tape reconciliation claim can be made."
              : corrections > 0
              ? `Uncorrected tape. ${state.tape!.correctionsSeen} correction and ${state.tape!.cancelsSeen} cancel frames have been received process-wide and counted, not applied: volume, delta and the profile are pre-correction, for this symbol and every other.`
              : "Uncorrected tape. Correction and cancel frames are counted, never applied; none has been received yet. Nothing here is reconciled against the official tape."
          }
        >
          {corrections === null ? "tape telemetry unavailable" : `uncorrected tape${corrections > 0 ? ` · ${corrections}` : ""}`}
        </span>
        {!s.hasQuotes && (
          <span className="chip" title="No quote has arrived for this symbol yet, so buy/sell is inferred from upticks and downticks only">
            tick rule only
          </span>
        )}
        <span className="of-header-spacer" />
        <button type="button" className="of-collapse" onClick={props.onToggleCollapsed} aria-expanded={!props.collapsed} title={props.collapsed ? "Show order-flow detail" : "Hide order-flow detail"}>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path fill="none" stroke="currentColor" strokeWidth={2.4} strokeLinecap="round" strokeLinejoin="round" d={props.collapsed ? "M6 9l6 6 6-6" : "M6 15l6-6 6 6"} />
          </svg>
          <span className="of-collapse-label">{props.collapsed ? "Show order-flow detail" : "Hide order-flow detail"}</span>
        </button>
        {s.exhaustion.level !== "none" && (
          <span className="of-read of-read-warning" title="Price is still extending while volume, delta and large trades fade">
            ! Exhaustion {s.exhaustion.level}
          </span>
        )}
      </div>

      {/* Collapsed keeps the header -- window, coverage and provenance --
          so the state is still readable in one line while the chart gets
          the rest of the height. */}
      {props.collapsed ? null : (
        <div className="of-body">
          <div className="of-hero">
            <div
              className="of-ring"
              style={{
                background: shares
                  ? `conic-gradient(var(--good) 0 ${shares.buy}%, var(--critical) ${shares.buy}% ${shares.buy + shares.sell}%, var(--border) ${shares.buy + shares.sell}% 100%)`
                  : "var(--border)",
              }}
            >
              <div className="of-ring-hole">
                <b>{shares ? `${shares.buy}%` : "—"}</b>
                <span>BUY</span>
              </div>
            </div>
            <div className="of-hero-body">
              <div className="of-tile-label">{pressure.pressureSide === "sell" ? "Sell pressure" : pressure.pressureSide === "buy" ? "Buy pressure" : "Pressure"}</div>
              <div className={`of-state ${pressure.pressureSide === "buy" ? "of-buy" : pressure.pressureSide === "sell" ? "of-sell" : ""}`}>
                {pressure.pressureSide === "balanced" ? "BALANCED" : LEVEL_LABEL[pressure.pressureLevel]}
              </div>
              <Meter level={pressure.pressureLevel} tone={pressure.pressureSide === "buy" ? "buy" : pressure.pressureSide === "sell" ? "sell" : "neutral"} />
              <div className="of-legend">
                <span className="of-sell">SELL {shares ? shares.sell : "—"}</span> · ? {shares ? shares.unknown : "—"}
              </div>
            </div>
          </div>

          <div className="of-tiles">
            <Tile icon="delta" label="Delta" stale={arrayLag}>
              <Value text={formatSignedShares(w.delta)} direction={w.delta} />
              <Sparkline values={cumulativeDeltaSeries(s.deltaBuckets.slice(-40))} />
            </Tile>

            <Tile icon="ratio" label="Delta ratio">
              <Value text={w.deltaRatio === null ? "—" : `${w.deltaRatio > 0 ? "+" : ""}${Math.round(w.deltaRatio * 100)}%`} direction={w.deltaRatio ?? 0} />
              <RatioBar ratio={w.deltaRatio} />
            </Tile>

            <Tile icon="velocity" label="Velocity" stale={arrayLag}>
              <Value text={w.shareVelocityAcceleration === null ? "—" : `${w.shareVelocityAcceleration.toFixed(1)}x`} direction={(w.shareVelocityAcceleration ?? 1) - 1} />
              <VolumeBars buckets={s.deltaBuckets.slice(-8)} />
            </Tile>

            {/* `threshold === null` means the detector has not seen enough
                prints on this symbol to say what "large" means here, so it
                has not run at all. That is not "no large trades": rendering
                it as NONE with two empty bars -- and an aria-label reading
                "Large buy volume 0" -- stated a measurement nobody made,
                for every symbol's first prints and for a whole session on a
                thin name. Same distinction the tape chip makes. */}
            <Tile
              icon="large"
              label="Large trades"
              title={
                s.large.threshold === null
                  ? "Large-trade detection has not started: it needs more prints on this symbol before it can say what counts as large here. This is not a reading of zero large trades."
                  : `Large is at or above ${formatShares(s.large.threshold)} shares, relative to this symbol's own recent prints.`
              }
            >
              {s.large.threshold === null ? (
                <>
                  <div className="of-state">COLLECTING</div>
                  <div
                    className="of-state of-unavailable"
                    role="img"
                    aria-label="Large buy and sell volume unavailable: detection has not collected enough prints on this symbol yet"
                  >
                    —
                  </div>
                </>
              ) : (
                <>
                  <div className={`of-state ${s.interpretation.largeTrades === "buy_dominant" ? "of-buy" : s.interpretation.largeTrades === "sell_dominant" ? "of-sell" : ""}`}>
                    {{ buy_dominant: "BUY DOMINANT", sell_dominant: "SELL DOMINANT", balanced: "BALANCED", none: "NONE" }[s.interpretation.largeTrades]}
                  </div>
                  <SideBars buy={s.large.buyVolume} sell={s.large.sellVolume} />
                </>
              )}
            </Tile>

            <Tile
              icon="absorption"
              label="Absorption"
              title={
                s.absorption.side === "sell_side"
                  ? "Aggressive buying is not lifting price: the buying is being absorbed here. This is liquidity that was hit, not liquidity known to be resting."
                  : s.absorption.side === "buy_side"
                    ? "Aggressive selling is not dropping price: the selling is being absorbed here. This is liquidity that was hit, not liquidity known to be resting."
                    : "Heavy one-sided volume that fails to move price"
              }
            >
              <div className={`of-state ${s.absorption.level === "none" || s.absorption.level === "low" ? "" : "of-warning"}`}>
                {LEVEL_LABEL[s.absorption.level]}
                {s.absorption.side && <small>{s.absorption.side === "sell_side" ? " by sellers" : " by buyers"}</small>}
              </div>
              <Meter level={s.absorption.level} tone={s.absorption.level === "none" || s.absorption.level === "low" ? "neutral" : "warning"} />
            </Tile>

            <Tile
              icon="sweeps"
              label="Sweeps"
              title="Fast same-side runs in the last 30 seconds: through three or more price levels, or across three or more lit exchanges. ×N venue = how many of them crossed exchanges."
            >
              <div className={`of-state ${s.sweeps.state === "none" || s.sweeps.state === "low" ? "" : "of-warning"}`}>
                {LEVEL_LABEL[s.sweeps.state]}
                {s.sweeps.count > 0 && (
                  <small>
                    {" "}
                    {s.sweeps.buyCount}▲ {s.sweeps.sellCount}▼
                    {(s.sweeps.multiVenueCount ?? 0) > 0 && ` · ${s.sweeps.multiVenueCount}× venue`}
                  </small>
                )}
              </div>
              <Meter level={s.sweeps.state} tone={s.sweeps.state === "none" || s.sweeps.state === "low" ? "neutral" : "warning"} />
            </Tile>

            <Tile icon="touch" label="Bid / ask size" title="Size at the best bid and ask over the last few minutes. ONE level only: this is the touch from the national best bid and offer, not the order book — the feed carries no depth." stale={arrayLag}>
              <TouchBars buckets={s.deltaBuckets.slice(-16)} />
            </Tile>
          </div>
        </div>
      )}

      {!props.collapsed && arrayLag !== null && (
        <div className="of-note">
          Delta pane, bid/ask sizes and profile are from {clock(s.chartArraysAsOfSecs ?? s.asOfSecs)} ({Math.round(arrayLag)}s older than the numbers above) — they refresh on a slower cadence than the rest of the strip.
        </div>
      )}

      {!props.collapsed && (s.book.ask.possiblePersistentLiquidity || s.book.bid.possiblePersistentLiquidity) && (
        // Wording is deliberate: the best bid/ask alone can show size
        // returning at a held price, never prove a hidden order.
        <div className="of-note">
          Possible persistent liquidity at the {s.book.ask.possiblePersistentLiquidity ? "ask" : "bid"}
          {s.book.ask.possiblePersistentLiquidity && s.book.bid.possiblePersistentLiquidity ? " and bid" : ""} — size keeps refilling at one price.
        </div>
      )}
    </div>
  );
}

function StripTitle() {
  return (
    <>
      <svg className="of-title-icon" viewBox="0 0 24 24" aria-hidden="true">
        <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" d="M3 12h4l3-7 4 14 3-7h4" />
      </svg>
      <span className="of-title">Order flow</span>
      <span className="chip" title="Observational research features. Not validated against outcomes, and not part of the momentum score, any alert, any ranking or the auto-trader.">
        RESEARCH
      </span>
    </>
  );
}

const TILE_ICONS = {
  delta: <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" d="M3 12h18M6 12V6M10 12V8M14 12v5M18 12v3" />,
  ratio: <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" d="M12 4v16M4 9h16M4 9l-1.5 5h5zM20 9l-2.5 5h5z" />,
  velocity: <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" d="M4 17a8 8 0 1 1 16 0M12 17l4-6" />,
  large: <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" d="M4 8l8-4 8 4-8 4zM4 13l8 4 8-4" />,
  absorption: <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" d="M12 3l7 3v6c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6z" />,
  sweeps: <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round" d="M7 11l5-5 5 5M7 18l5-5 5 5" />,
  touch: <path fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" d="M3 12h18M7 12V7M12 12V5M17 12V8M7 12v4M12 12v6M17 12v3" />,
};

/** `stale` is the chart-array lag in seconds, for the tiles whose small
 * chart is drawn from those arrays. Marked per tile rather than once for
 * the strip, because the number above each one is current and only the
 * chart under it is not. */
function Tile(props: { icon: keyof typeof TILE_ICONS; label: string; title?: string; stale?: number | null; children: ReactNode }) {
  return (
    <div className="of-tile" title={props.title}>
      <div className="of-tile-label">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          {TILE_ICONS[props.icon]}
        </svg>
        {props.label}
        {props.stale != null && (
          <span className="of-stale" title={`This chart is from the last full fetch, ${Math.round(props.stale)}s before the figures above it`}>
            {Math.round(props.stale)}s old
          </span>
        )}
      </div>
      {props.children}
    </div>
  );
}

function Value(props: { text: string; direction: number }) {
  return (
    <div className="of-value">
      <b>{props.text}</b>
      {props.direction > 0 && <span className="of-buy">▲</span>}
      {props.direction < 0 && <span className="of-sell">▼</span>}
    </div>
  );
}

function Meter(props: { level: OrderFlowLevel; tone: "buy" | "sell" | "warning" | "neutral" }) {
  const filled = levelSegments(props.level);
  return (
    <div className="of-meter" role="img" aria-label={`${LEVEL_LABEL[props.level]} (${filled} of 4)`}>
      {[1, 2, 3, 4].map((n) => (
        <span key={n} className={n <= filled ? `of-meter-on of-meter-${props.tone}` : undefined} />
      ))}
    </div>
  );
}

/** Best-ask size above the line, best-bid size below, one column per
 * 30-second bucket. A thick ask that will not thin out, or a bid being
 * pulled, shows as the bars growing or vanishing on one side. This is ONE
 * level -- the touch -- and is labelled that way; it is not depth, and
 * the feed this comes from carries none. */
function TouchBars(props: { buckets: OrderFlowDeltaBucket[] }) {
  const quoted = props.buckets.filter((b) => b.bidSize !== null && b.askSize !== null);
  if (quoted.length === 0) return <div className="of-state">—</div>;
  const max = Math.max(1, ...quoted.map((b) => Math.max(b.bidSize ?? 0, b.askSize ?? 0)));
  const latest = quoted[quoted.length - 1];
  return (
    <>
      <div className="of-value">
        <b>
          {latest.bidSize} × {latest.askSize}
        </b>
      </div>
      <div className="of-touch" role="img" aria-label={`Best bid size ${latest.bidSize}, best ask size ${latest.askSize}, in round lots`}>
        {quoted.map((b) => (
          <span key={b.startSecs}>
            <i className="of-touch-ask" style={{ height: `${((b.askSize ?? 0) / max) * 50}%` }} />
            <i className="of-touch-bid" style={{ height: `${((b.bidSize ?? 0) / max) * 50}%` }} />
          </span>
        ))}
      </div>
    </>
  );
}

/** Line through `values`, scaled to its own min/max. A flat or one-point
 * series draws a centred flat line rather than nothing. */
function Sparkline(props: { values: number[] }) {
  const v = props.values;
  if (v.length === 0) return <div className="of-spark" />;
  const lo = Math.min(...v);
  const hi = Math.max(...v);
  const span = hi - lo;
  const points = v.map((value, i) => `${v.length === 1 ? 50 : (i / (v.length - 1)) * 100},${span === 0 ? 50 : 92 - ((value - lo) / span) * 84}`).join(" ");
  const rising = v[v.length - 1] >= v[0];
  return (
    <svg className="of-spark" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
      <polyline points={v.length === 1 ? `0,50 100,50` : points} fill="none" stroke={rising ? "var(--good)" : "var(--critical)"} strokeWidth={1.8} vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

/** Fills from a centre line: right for net buying, left for net selling. */
function RatioBar(props: { ratio: number | null }) {
  const r = props.ratio ?? 0;
  const width = Math.min(50, Math.abs(r) * 50);
  return (
    <div className="of-ratio" role="img" aria-label={props.ratio === null ? "No classified volume" : `${Math.round(r * 100)} percent net ${r >= 0 ? "buying" : "selling"}`}>
      <span className={r >= 0 ? "of-ratio-buy" : "of-ratio-sell"} style={r >= 0 ? { left: "50%", width: `${width}%` } : { right: "50%", width: `${width}%` }} />
      <i />
    </div>
  );
}

/** Total shares per recent 30-second bucket; the latest is highlighted. */
function VolumeBars(props: { buckets: OrderFlowDeltaBucket[] }) {
  const totals = props.buckets.map((b) => b.buyVolume + b.sellVolume + b.unknownVolume);
  const max = Math.max(1, ...totals);
  return (
    <div className="of-bars" aria-hidden="true">
      {totals.map((total, i) => (
        <span key={props.buckets[i].startSecs} className={i === totals.length - 1 ? "of-bars-latest" : undefined} style={{ height: `${Math.max(6, (total / max) * 100)}%` }} />
      ))}
    </div>
  );
}

function SideBars(props: { buy: number; sell: number }) {
  const max = Math.max(1, props.buy, props.sell);
  return (
    <div className="of-sidebars" role="img" aria-label={`Large buy volume ${formatShares(props.buy)}, large sell volume ${formatShares(props.sell)}`}>
      <div>
        <span className="of-sidebar-buy" style={{ width: `${(props.buy / max) * 100}%` }} />
      </div>
      <div>
        <span className="of-sidebar-sell" style={{ width: `${(props.sell / max) * 100}%` }} />
      </div>
    </div>
  );
}
