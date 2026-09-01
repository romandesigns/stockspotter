// Left nav rail's launcher for a real, in-app Backtest Replay -- the
// Super Chart engine's `backtest` context (stockspotter-super-chart-
// prototype memory), wired to real historical data instead of the
// prototype's embedded demo bars. Opens inside a real shadcn Dialog per
// Roman's explicit ask ("I don't want it to redirect me to a different
// page... migrate this into a modal") -- an earlier version of this
// component just linked out to the published Artifact; that's gone now,
// this dialog IS the real feature.
//
// Real pieces reused, not re-derived:
// - ReplayChart.tsx -> superChartEngine.ts's mountSuperChart(el,
//   'backtest', ...) -- the actual chart engine, already had this
//   preset, just never wired to real data before now.
// - ReplayRangePicker.tsx -> the prototype's real two-endpoint range
//   calendar + session-count presets (lib/tradingDays.ts).
// - Sessions filter -> real ET session classification (sessionClassify.ts)
//   of each bar's actual timestamp, not a fixed UTC offset.
// - ws-server's new GET /replay/bars/:symbol?start&end (real Alpaca
//   historical data for ANY symbol, not the prototype's curated 5).
//
// One deliberate, flagged scope reduction from the prototype (per
// Roman's own choice when asked): playback advances whole bars, not the
// prototype's tick-by-tick sub-bar interpolation animation -- that was
// purely cosmetic (no new information), and porting its exact timing
// math was a lot of added complexity for a visual flourish.

import { useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { ChartIcon } from "./ChartIcon";
import { ReplayChart } from "./ReplayChart";
import { ReplayRangePicker } from "./ReplayRangePicker";
import { formatPct, formatPrice } from "../lib/format";
import { classifySession, formatBarDateTime, SESSION_LABEL, type Session } from "../lib/sessionClassify";
import { dateKey, lastNSessions, TODAY } from "../lib/tradingDays";
import { useReplayBars } from "../lib/useReplayBars";

const DEFAULT_SYMBOL = "SWVL"; // the prototype's own real, proven-working demo symbol
const SESSION_KEYS: Session[] = ["pre", "regular", "after"];
const SESSION_ROW_LABEL: Record<Session, string> = { pre: "Pre-market", regular: "Regular hours", after: "After-hours", closed: "" };
const SPEEDS = [
  { label: "1x", ms: 200 },
  { label: "2x", ms: 100 },
  { label: "6x", ms: 34 },
  { label: "20x", ms: 10 },
] as const;

function defaultRange(): { start: string; end: string } {
  const days = lastNSessions(5, TODAY);
  return { start: dateKey(days[0]), end: dateKey(days[days.length - 1]) };
}

export function ReplayLauncher() {
  const [dialogOpen, setDialogOpen] = useState(false);
  const [symbolDraft, setSymbolDraft] = useState(DEFAULT_SYMBOL);
  const [symbol, setSymbol] = useState(DEFAULT_SYMBOL);
  const [range, setRange] = useState(defaultRange);
  const [sessions, setSessions] = useState<Record<Session, boolean>>({ pre: true, regular: true, after: true, closed: false });
  const [visibleCount, setVisibleCount] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [speedMs, setSpeedMs] = useState<number>(SPEEDS[2].ms);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Only fetches while the dialog is actually open -- no reason to hit
  // the backend for a feature the user hasn't opened yet.
  const { bars, loading, error } = useReplayBars(dialogOpen ? symbol : null, range.start, range.end);

  const filteredBars = useMemo(() => bars.filter((b) => sessions[classifySession(b.time)]), [bars, sessions]);
  const chartKey = `${symbol}:${range.start}:${range.end}:${sessions.pre}:${sessions.regular}:${sessions.after}`;

  // Fresh series (new symbol/range/session filter) -- starts fully
  // revealed, like a normal static chart, matching Scanner Detail's own
  // "show everything immediately" default. Play restarts from the
  // beginning when pressed at/past the end (see togglePlay below).
  //
  // Real bug caught here during verification: depending on `chartKey`
  // alone fired this once at mount, while the real fetch was still in
  // flight and filteredBars.length was still 0 -- visibleCount got stuck
  // at 0 forever once the real bars actually arrived (chartKey itself
  // never changes just because a fetch resolved), so the chart mounted
  // with zero visible bars. `filteredBars.length` has to be a real
  // dependency so this re-fires the moment data actually shows up.
  useEffect(() => {
    setPlaying(false);
    setVisibleCount(filteredBars.length);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chartKey, filteredBars.length]);

  useEffect(() => {
    if (!playing) return;
    timerRef.current = setInterval(() => {
      setVisibleCount((n) => {
        if (n >= filteredBars.length) {
          setPlaying(false);
          return n;
        }
        return n + 1;
      });
    }, speedMs);
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [playing, speedMs, filteredBars.length]);

  function togglePlay() {
    if (!playing && visibleCount >= filteredBars.length && filteredBars.length > 0) setVisibleCount(1);
    setPlaying((p) => !p);
  }

  function stepBack() {
    setPlaying(false);
    setVisibleCount((n) => Math.max(1, n - 1));
  }

  function stepForward() {
    setPlaying(false);
    setVisibleCount((n) => Math.min(filteredBars.length, n + 1));
  }

  function toggleSession(key: Session) {
    setSessions((prev) => {
      const activeCount = SESSION_KEYS.filter((k) => prev[k]).length;
      if (prev[key] && activeCount === 1) return prev; // always keep at least one session visible
      return { ...prev, [key]: !prev[key] };
    });
  }

  function commitSymbol() {
    const s = symbolDraft.trim().toUpperCase();
    if (s) setSymbol(s);
  }

  const current = filteredBars[Math.max(0, visibleCount - 1)];
  const first = filteredBars[0];
  const changePct = current && first && first.open > 0 ? ((current.close - first.open) / first.open) * 100 : 0;
  const currentSession = current ? classifySession(current.time) : null;

  return (
    <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" className="app-rail-btn" aria-label="Backtest Replay" title="Backtest Replay">
          <ChartIcon name="replay" />
        </Button>
      </DialogTrigger>
      <DialogContent className="replay-dialog">
        <DialogHeader>
          <DialogTitle>Backtest Replay</DialogTitle>
        </DialogHeader>

        <div className="replay-toolbar">
          <div className="replay-symbol">
            <Input
              value={symbolDraft}
              onChange={(e) => setSymbolDraft(e.target.value.toUpperCase())}
              onKeyDown={(e) => e.key === "Enter" && commitSymbol()}
              placeholder="Symbol"
              className="replay-symbol-input"
            />
            <Button variant="outline" size="xs" onClick={commitSymbol}>
              Load
            </Button>
          </div>

          <ReplayRangePicker start={range.start} end={range.end} onChange={(start, end) => setRange({ start, end })} />

          <Popover>
            <PopoverTrigger asChild>
              <Button variant="outline" size="icon-sm" aria-label="Sessions" title="Filter by trading session">
                <ChartIcon name="daynight" />
              </Button>
            </PopoverTrigger>
            <PopoverContent align="end" className="chart-popover-content">
              <div className="chart-popover-title">Sessions</div>
              {SESSION_KEYS.map((key) => (
                <label key={key} className="chart-switch-row">
                  <span>{SESSION_ROW_LABEL[key]}</span>
                  <Switch size="sm" checked={sessions[key]} onCheckedChange={() => toggleSession(key)} className="chart-switch-row-control" />
                </label>
              ))}
            </PopoverContent>
          </Popover>

          <div className="replay-toolbar-spacer" />

          {current && (
            <div className="replay-readout">
              <span className="index-card-value">{formatPrice(current.close)}</span>
              <span className={changePct >= 0 ? "pct-up" : "pct-down"}>{formatPct(changePct)}</span>
            </div>
          )}
        </div>

        <div className="replay-chart-wrap">
          {loading && <div className="empty-state">Loading real historical bars…</div>}
          {error && (
            <div className="empty-state">
              Couldn't load bars for {symbol} in that range -- check the symbol is real and tradable.
            </div>
          )}
          {!loading && !error && filteredBars.length > 0 && (
            <ReplayChart chartKey={chartKey} bars={filteredBars} visibleCount={visibleCount} height={340} />
          )}
          {!loading && !error && filteredBars.length === 0 && bars.length > 0 && (
            <div className="empty-state">No bars in the selected sessions.</div>
          )}
        </div>

        <div className="playback">
          <div className="transport">
            <Button variant="outline" size="icon-sm" aria-label="Step back one bar" title="Step back one bar" onClick={stepBack}>
              <ChartIcon name="back" />
            </Button>
            <Button variant="default" size="icon-sm" aria-label={playing ? "Pause" : "Play"} title={playing ? "Pause" : "Play"} onClick={togglePlay}>
              <ChartIcon name={playing ? "pause" : "play"} />
            </Button>
            <Button variant="outline" size="icon-sm" aria-label="Step forward one bar" title="Step forward one bar" onClick={stepForward}>
              <ChartIcon name="fwd" />
            </Button>
          </div>

          <div className="scrub">
            <span className="scrub-time">
              {current ? `${formatBarDateTime(current.time)}${currentSession ? ` · ${SESSION_LABEL[currentSession]}` : ""}` : "—"}
            </span>
            <input
              type="range"
              min={1}
              max={Math.max(1, filteredBars.length)}
              value={Math.max(1, visibleCount)}
              onChange={(e) => {
                setPlaying(false);
                setVisibleCount(Number(e.target.value));
              }}
            />
            <span className="scrub-time">
              bar {visibleCount} / {filteredBars.length}
            </span>
          </div>

          <Select value={String(speedMs)} onValueChange={(v) => setSpeedMs(Number(v))}>
            <SelectTrigger size="sm" className="replay-speed-select">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SPEEDS.map((s) => (
                <SelectItem key={s.ms} value={String(s.ms)}>
                  {s.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </DialogContent>
    </Dialog>
  );
}
