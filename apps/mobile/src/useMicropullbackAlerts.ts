import type { ConsolidationEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { useNativeAttentionAlerts } from "./useNativeAttentionAlerts";
export function useMicropullbackAlerts(events: ConsolidationEvent[], momentum: Map<string, MomentumUpdate>) { useNativeAttentionAlerts("micropullback", events, momentum); }
