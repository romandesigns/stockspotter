// Order-flow summary for the ONE symbol a chart has open.
//
// The rules -- pushed over the realtime socket when the server does that,
// polled from GET /orderflow/:symbol otherwise, and which of the two
// shapes is asked for when -- live in `orderFlowWatch.ts`. This is the
// React edge of it.
//
// The four non-ready states are kept apart because the UI says something
// different for each, and collapsing them would turn "this server is too
// old to have the feature" into a strip that looks broken (see
// `OrderFlowState`).
//
// **Opt-in, default off.** Without `VITE_ORDER_FLOW=1` in the build,
// `orderFlowEnabled()` is false, no component mounts this hook, nothing
// subscribes on the socket and no `/orderflow` request is ever made. That
// is deliberate: the backend feature is off in every deployment today, so
// a client that polled for it would spend the shared request budget to be
// told "disabled" every five seconds.
import { useEffect, useState } from "react";
import { authenticatedFetch } from "@stockspotter/shared-types";
import { watchOrderFlow, type OrderFlowState } from "./orderFlowWatch";
import { resolveHttpUrl } from "./config";

export type { OrderFlowState };

/** Whether this build shows the order-flow strip at all. Read once per
 * call rather than cached, so it stays a plain build-time flag with no
 * module init order to reason about. */
export function orderFlowEnabled(): boolean {
  const flag = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_ORDER_FLOW;
  return flag === "1" || flag === "true";
}

export function useOrderFlow(symbol: string | null): OrderFlowState {
  const [result, setResult] = useState<{ symbol: string | null; state: OrderFlowState }>({ symbol, state: { status: "loading" } });

  useEffect(() => {
    if (!symbol || !orderFlowEnabled()) return;
    return watchOrderFlow({
      symbol,
      baseUrl: resolveHttpUrl,
      fetch: (input) => authenticatedFetch(input),
      onState: (state) => setResult({ symbol, state }),
    });
  }, [symbol]);

  // A result for a previous symbol is never shown against the new one:
  // until this symbol's own first answer arrives, it is "loading".
  return result.symbol === symbol ? result.state : { status: "loading" };
}
