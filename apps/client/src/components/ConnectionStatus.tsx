import type { ConnectionStatus as Status } from "../lib/useRealtimeFeed";

const LABEL: Record<Status, string> = {
  connecting: "connecting…",
  open: "live",
  closed: "disconnected — retrying",
};

export function ConnectionStatus(props: { status: Status; wsUrl: string }) {
  return (
    <div className={`connection-status connection-${props.status}`} title={props.wsUrl}>
      <span className="connection-dot" />
      {LABEL[props.status]}
    </div>
  );
}
