import type { TodayMovers } from "../lib/useMovers";
import { moverRefreshLabel } from "../lib/moverProvenance";

export function MoverRefreshStatus({ today }: { today: TodayMovers }) {
  const status = moverRefreshLabel(today);
  return status ? <span className="dim" role="status" title={status.title}>{status.label}</span> : null;
}
