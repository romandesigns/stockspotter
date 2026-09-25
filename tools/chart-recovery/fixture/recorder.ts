// Every observable thing the stubbed chart engine is asked to do, in
// commit order. This is the whole evidence base of the harness: the
// tests assert on what React's real lifecycle actually pushed into the
// engine boundary (mount/destroy/setBars/applyOptions/fitContent), not
// on the shape of the source that produced it.
//
// Deliberately NOT a chart: nothing here draws, measures or renders
// anything, so a passing run proves React lifecycle behaviour only. The
// real-engine twelve-series parity work is separate and stays separate.

export interface EngineEvent {
  /** Global order across all instances -- how "did the mount happen
   * before that setBars" questions get answered. */
  seq: number;
  instance: number;
  event:
    | "mount"
    | "setBars"
    | "destroy"
    | "chart.remove"
    | "fitContent"
    | "setChartType"
    | "setSignalMarkers"
    | "series.applyOptions"
    | "priceScale.applyOptions"
    | "chart.applyOptions"
    | "resize"
    | "wireTooltip"
    | "unwireTooltip";
  /** Symbol read back out of the REAL committed DOM header that owns the
   * container this engine was mounted into -- the independent check that
   * bar payloads and the rendered identity agree. */
  symbolFromDom: string | null;
  barCount?: number;
  /** Distinct symbols the bar payload's timestamps belong to (see
   * data.ts -- each fixture symbol owns its own disjoint time block). */
  barSymbols?: string[];
  times?: number[];
  firstTime?: number;
  lastTime?: number;
  lastClose?: number;
  width?: number;
  height?: number;
  detail?: Record<string, unknown>;
}

const events: EngineEvent[] = [];
const liveInstances = new Set<number>();
let seq = 0;
let instanceCounter = 0;

export function nextInstanceId(): number {
  instanceCounter += 1;
  return instanceCounter;
}

export function record(event: Omit<EngineEvent, "seq">): void {
  events.push({ seq: seq++, ...event });
}

export function engineLog(): EngineEvent[] {
  return events.map((e) => ({ ...e }));
}

export function clearEngineLog(): void {
  events.length = 0;
}

export function markLive(instance: number): void {
  liveInstances.add(instance);
}

export function markDead(instance: number): void {
  liveInstances.delete(instance);
}

/** Instances that were mounted and never destroyed -- an orphaned engine
 * shows up here, and an orphaned canvas shows up in the DOM snapshot. */
export function liveInstanceIds(): number[] {
  return [...liveInstances].sort((a, b) => a - b);
}
