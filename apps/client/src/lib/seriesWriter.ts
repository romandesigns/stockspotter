/** Lightweight Charts 4.1 permits update() only at/after its latest time.
 * Keep complete replacement for corrections, eviction, reset and timeframe changes.
 * Callers supply immutable, sorted arrays, as the indicator functions already do.
 */
export function createSeriesWriter<T extends { time: unknown }>(series: {
  setData: (data: T[]) => void;
  update: (item: T) => void;
}) {
  let previous: T[] = [];
  let initialized = false;
  const equal = (a: T, b: T) => {
    const keys = Object.keys(a) as (keyof T)[];
    return keys.length === Object.keys(b).length && keys.every((key) => a[key] === b[key]);
  };
  return (data: T[]) => {
    let tailOnly = previous.length > 0 && data.length >= previous.length;
    if (tailOnly) {
      for (let i = 0; i < previous.length - 1; i++) {
        if (!equal(previous[i], data[i])) { tailOnly = false; break; }
      }
      if (previous.at(-1)!.time !== data[previous.length - 1]?.time) tailOnly = false;
    }
    if (tailOnly) {
      for (let i = previous.length - 1; i < data.length; i++) {
        if (!previous[i] || !equal(previous[i], data[i])) series.update(data[i]);
      }
    } else if (!initialized || data.length || previous.length) {
      series.setData(data);
    }
    previous = data;
    initialized = true;
  };
}
