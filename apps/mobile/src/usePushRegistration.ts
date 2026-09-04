// Real server-side push registration for the ignition-confirmed alert
// (2026-09-04, Roman: "I want to be notified on my phone even if my
// phone is locked and I'm not looking at the screen or the chart
// directly. I want to be able to turn this feature off on the phone if
// I want to"). The client-side alert (useIgnitionAlerts.ts) only fires
// while this app's own process is alive and connected -- a locked phone
// gets its background JS suspended by the OS within a short window
// (especially iOS), so a purely client-side alert can silently miss the
// exact kind of move it exists to catch. This hook instead registers
// this device's real Expo push token with ws-server (crates/ws-server/
// src/push.rs), which sends the notification itself via Expo's push
// service straight to Apple/Google's push infrastructure -- reaches the
// device even fully backgrounded or closed.
//
// Deliberately no new native dependency: no expo-device (to check
// Device.isDevice before requesting a token) -- wrapped in try/catch
// instead and treated as a silent no-op on failure, same
// requireOptionalNativeModule-style safety net this project already
// established for exactly this "don't crash an already-shipped OTA
// build" concern (see useSafeKeepAwake.ts's own header comment). No
// expo-constants either -- the EAS project id is a small, effectively-
// static value, hardcoded below rather than pulling in a dependency just
// to read it back out of app.json at runtime.
import { useEffect, useRef, useState } from "react";
import * as Notifications from "expo-notifications";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { HTTP_URL } from "./config";

const EAS_PROJECT_ID = "3d68c458-5291-4beb-b919-3ada3acbc2f7"; // apps/mobile/app.json's own expo.extra.eas.projectId
const STORAGE_KEY = "stockspotter:ignitionPushEnabled";

let androidChannelReady = false;
async function ensureIgnitionChannel() {
  if (androidChannelReady) return;
  androidChannelReady = true;
  await Notifications.setNotificationChannelAsync("ignition-alerts", {
    name: "Ignition alerts",
    importance: Notifications.AndroidImportance.HIGH,
    sound: "default",
  }).catch(() => {});
}

export function usePushRegistration(): { enabled: boolean; setEnabled: (v: boolean) => void; loaded: boolean } {
  // Real default is ON -- Roman's own ask was "I want to be notified...
  // I want to be able to turn this feature off", i.e. opt-out, not
  // opt-in. loaded/setEnabledState below only ever narrows this to
  // whatever was actually saved before, never the reverse.
  const [enabled, setEnabledState] = useState(true);
  const [loaded, setLoaded] = useState(false);
  // The token this device last successfully registered -- needed at
  // unregister time (the server call takes the actual token, not just
  // "this device"), kept in a ref since re-fetching it isn't free and
  // nothing needs to re-render off it.
  const tokenRef = useRef<string | null>(null);

  useEffect(() => {
    AsyncStorage.getItem(STORAGE_KEY)
      .then((raw) => {
        if (raw !== null) setEnabledState(raw === "1");
      })
      .finally(() => setLoaded(true));
  }, []);

  useEffect(() => {
    if (!loaded) return; // don't act on the transient default before the real saved value loads
    AsyncStorage.setItem(STORAGE_KEY, enabled ? "1" : "0").catch(() => {});

    if (!enabled) {
      // The real mechanism behind "turn this feature off on the phone"
      // -- explicitly unregister whatever token this device last
      // registered, not just stop re-registering it. The server then
      // simply excludes this device from every future send.
      if (tokenRef.current) {
        fetch(`${HTTP_URL}/push/unregister`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ token: tokenRef.current }),
        }).catch(() => {});
      }
      return;
    }

    let cancelled = false;
    ensureIgnitionChannel();
    Notifications.requestPermissionsAsync()
      .then(({ status }) => {
        if (status !== "granted" || cancelled) return null;
        return Notifications.getExpoPushTokenAsync({ projectId: EAS_PROJECT_ID });
      })
      .then((result) => {
        if (!result || cancelled) return;
        tokenRef.current = result.data;
        return fetch(`${HTTP_URL}/push/register`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ token: result.data }),
        });
      })
      .catch(() => {
        // Real, expected failure modes: no physical device (a simulator
        // can't get a real push token), permission denied, no network
        // yet at launch -- none worth surfacing; this device just won't
        // receive server pushes until a retry (next app launch, or the
        // toggle being flipped off and back on) succeeds.
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, loaded]);

  return { enabled, setEnabled: setEnabledState, loaded };
}
