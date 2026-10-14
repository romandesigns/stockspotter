import { useState } from "react";
import { createRoot } from "react-dom/client";
import { ExperimentalStrategies } from "../../apps/client/src/components/ExperimentalStrategies";
import { useRealtimeFeed } from "../../apps/client/src/lib/useRealtimeFeed";
import { useExperimentalStrategies } from "../../apps/client/src/lib/useExperimentalStrategies";

const sockets: FakeSocket[] = [];
const sent: unknown[] = [];
class FakeSocket extends EventTarget {
  static OPEN = 1;
  readyState = 1;
  constructor(_url: string) { super(); sockets.push(this); setTimeout(() => this.dispatchEvent(new Event("open")), 0); }
  send(value: string) { sent.push(JSON.parse(value)); }
  close() { this.readyState = 3; this.dispatchEvent(new Event("close")); }
}
Object.assign(window, {
  WebSocket: FakeSocket,
  fetch: async () => new Response("[]", { status: 200, headers: { "Content-Type": "application/json" } }),
  push: (value: unknown) => sockets.at(-1)!.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(value) })),
  pushLegacy: (value: unknown) => sockets[0].dispatchEvent(new MessageEvent("message", { data: JSON.stringify(value) })),
  disconnectResearch: () => sockets.at(-1)!.close(),
  sent,
});
function Fixture() {
  const feed = useRealtimeFeed();
  const experimental = useExperimentalStrategies(true);
  const [selected, setSelected] = useState("none");
  return <div className="app"><header className="app-topbar">Stockspotter fixture</header>
    <ExperimentalStrategies rows={experimental.rows} status={experimental.status} gap={experimental.gap} connection={experimental.connection} enabled={experimental.enabled} onSelectSymbol={setSelected} />
    <main><p id="selected">Selected {selected}</p><p id="legacy">Legacy events {feed.events.length}</p></main>
  </div>;
}
createRoot(document.getElementById("root")!).render(<Fixture />);
