import type { IgnitionEvent } from "@stockspotter/shared-types";
import { useNativeAttentionAlerts } from "./useNativeAttentionAlerts";
export function useIgnitionAlerts(events: IgnitionEvent[]) { useNativeAttentionAlerts("ignition", events); }
