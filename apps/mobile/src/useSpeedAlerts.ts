// Speed alerts on the phone (2026-10-16, Roman: Halt Early-Warning is
// where most of his real profit came from, premarket and regular hours
// alike). Three parts, one hook, mounted once in App.tsx so it runs on
// every tab:
//
// 1. One SpeedTracker per app session (shared-types haltSpeed.ts -- the
//    same module web and desktop use, so the three show identical numbers
//    and decide alerts identically). It is fed the latest halt reading per
//    symbol on every feed update and keeps the 10-minute gauge history the
//    Home hub reads ("+x% in 5 min", "filling fast").
// 2. In-app alert: a banner styled like the Phone.dc.html notification
//    while the app is in the foreground.
// 3. Server push opt-in on its own routes (/push/speed/register and
//    /push/speed/unregister, same body and auth as the ignition push) and
//    its own Android channel, "speed-alerts". The server currently pushes
//    at a fixed 70%, whatever threshold is chosen here; the settings sheet
//    says so.
//
// No double notification. useIgnitionAlerts fires a local notification
// on top of the ignition push; here that would duplicate the server push
// (and the in-app banner), so a local notification is scheduled only when
// the phone push is OFF and the app is not in the foreground (backgrounded
// but not yet suspended). In the foreground the banner is the alert, and a
// speed push for a symbol the banner just showed is kept out of the system
// tray (shouldPresentSpeedPush, read by the handler in usePriceAlerts.ts).
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppState } from "react-native";
import * as Notifications from "expo-notifications";
import * as Haptics from "expo-haptics";
import AsyncStorage from "@react-native-async-storage/async-storage";
import type { HaltWarning, SpeedAlert, SpeedAlertSettings, SpeedRow } from "@stockspotter/shared-types";
import { SPEED_ALERT_DEFAULTS, SpeedTracker, speedRows } from "@stockspotter/shared-types";
import { decodeSpeedSettings } from "./speedSettings";
import { usePushTokenRegistration } from "./usePushRegistration";

const STORAGE_KEY = "stockspotter:speedAlertSettings";
export const SPEED_ALERT_CHANNEL = "speed-alerts";
/** What ws-server's speed push uses today, regardless of this phone's threshold. */
export const SPEED_PUSH_THRESHOLD = 70;
/** How long the in-app banner stays up. */
export const SPEED_BANNER_MS = 8_000;

let channelReady = false;
async function ensureSpeedChannel() {
  if (channelReady) return;
  channelReady = true;
  await Notifications.setNotificationChannelAsync(SPEED_ALERT_CHANNEL, {
    name: "Speed alerts",
    importance: Notifications.AndroidImportance.HIGH,
    sound: "default",
  }).catch(() => {});
}

// Symbols the in-app banner showed recently: a speed push for one of them
// arriving while the app is open would repeat what is already on screen.
const bannerShownAt = new Map<string, number>();
const BANNER_DEDUP_MS = 2 * 60_000;

/** For the global notification handler: false for a speed push whose
 * symbol the in-app banner showed in the last two minutes while the app
 * is in the foreground. Everything else is presented as before. A speed
 * push is recognised by its Android channel, or by its title wording
 * (ws-server's speed_push_text: "SYM ▲ speed 72%") on iOS, which has no
 * channels. */
export function shouldPresentSpeedPush(notification: Notifications.Notification): boolean {
  if (AppState.currentState !== "active") return true;
  const content = notification.request.content;
  const symbol = content.data?.symbol;
  if (typeof symbol !== "string") return true;
  const trigger = notification.request.trigger as { channelId?: string; remoteMessage?: { notification?: { channelId?: string } } } | null;
  const channel = trigger?.channelId ?? trigger?.remoteMessage?.notification?.channelId;
  const isSpeed = channel === SPEED_ALERT_CHANNEL || /▲ speed \d+%/.test(content.title ?? "");
  if (!isSpeed) return true;
  const shown = bannerShownAt.get(symbol);
  return shown === undefined || Date.now() - shown > BANNER_DEDUP_MS;
}

export function speedAlertTitle(alert: SpeedAlert): string {
  const arrow = alert.movePct !== null && alert.movePct < 0 ? "▼" : "▲";
  return `${alert.symbol} ${arrow} speed ${alert.pct}%${alert.fillingFast ? " · filling fast" : ""}`;
}

export function speedAlertBody(alert: SpeedAlert): string {
  const price = `$${alert.price.toFixed(alert.price < 1 ? 4 : 2)}`;
  const move = alert.movePct !== null && Number.isFinite(alert.movePct) ? `${alert.movePct >= 0 ? "+" : "−"}${Math.abs(alert.movePct).toFixed(1)}% in 5 min at ${price}` : `At ${price}`;
  const relVol = alert.relativeVolume !== null && Number.isFinite(alert.relativeVolume) ? ` · rel vol ${alert.relativeVolume.toFixed(1)}x` : "";
  return `${move}${relVol}.`;
}

export interface SpeedAlertsState {
  /** Bullish rows, fullest gauge first (speedRows "up"). */
  upRows: SpeedRow[];
  /** Alerts raised this session, newest first. */
  recentAlerts: SpeedAlert[];
  banner: SpeedAlert | null;
  dismissBanner: () => void;
  settings: SpeedAlertSettings;
  setSettings: (next: SpeedAlertSettings) => void;
  loaded: boolean;
}

export function useSpeedAlerts(readings: HaltWarning[]): SpeedAlertsState {
  const trackerRef = useRef<SpeedTracker | null>(null);
  if (!trackerRef.current) trackerRef.current = new SpeedTracker();
  const tracker = trackerRef.current;

  const [settings, setSettingsState] = useState<SpeedAlertSettings>(SPEED_ALERT_DEFAULTS);
  const [loaded, setLoaded] = useState(false);
  const settingsRef = useRef(settings);
  settingsRef.current = settings;

  useEffect(() => {
    AsyncStorage.getItem(STORAGE_KEY)
      .then((raw) => setSettingsState(decodeSpeedSettings(raw)))
      .catch(() => {})
      .finally(() => setLoaded(true));
  }, []);

  const setSettings = useCallback((next: SpeedAlertSettings) => {
    setSettingsState(next);
    AsyncStorage.setItem(STORAGE_KEY, JSON.stringify(next)).catch(() => {});
  }, []);

  usePushTokenRegistration({
    enabled: settings.push,
    loaded,
    registerPath: "/push/speed/register",
    unregisterPath: "/push/speed/unregister",
    ensureChannel: ensureSpeedChannel,
  });

  // Observed during render (not in an effect) so the rows below already
  // include this update's points. Safe to repeat: observe() ignores a
  // reading it has already seen (same symbol and timestamp), so a second
  // render pass cannot double-count or double-alert.
  const upRows = useMemo(() => {
    for (const reading of readings) tracker.observe(reading, settingsRef.current);
    return speedRows(readings, tracker, "up");
  }, [readings, tracker]);

  const [recentAlerts, setRecentAlerts] = useState<SpeedAlert[]>([]);
  const [banner, setBanner] = useState<SpeedAlert | null>(null);
  const handledRef = useRef(new Set<string>());

  useEffect(() => {
    const raised = tracker.recentAlerts();
    const fresh = raised.filter((a) => !handledRef.current.has(a.id));
    if (fresh.length === 0) return;
    for (const a of fresh) handledRef.current.add(a.id);
    setRecentAlerts(raised.slice());
    const newest = fresh[0];
    const current = settingsRef.current;
    if (AppState.currentState === "active") {
      bannerShownAt.set(newest.symbol, Date.now());
      setBanner(newest);
      if (current.sound) Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success).catch(() => {});
      return;
    }
    if (current.push) return; // the server push covers a phone that is not looking
    ensureSpeedChannel();
    Notifications.getPermissionsAsync()
      .then(({ status }) => {
        if (status !== "granted") return;
        return Notifications.scheduleNotificationAsync({
          content: {
            title: speedAlertTitle(newest),
            body: `${speedAlertBody(newest)} Unproven signal, not advice.`,
            data: { symbol: newest.symbol },
            sound: current.sound ? "default" : undefined,
          },
          // Immediate, on the speed channel (ChannelAwareTriggerInput).
          trigger: { channelId: SPEED_ALERT_CHANNEL },
        });
      })
      .catch(() => {});
  }, [upRows, tracker]);

  useEffect(() => {
    if (!banner) return;
    const timer = setTimeout(() => setBanner(null), SPEED_BANNER_MS);
    return () => clearTimeout(timer);
  }, [banner]);

  const dismissBanner = useCallback(() => setBanner(null), []);

  return { upRows, recentAlerts, banner, dismissBanner, settings, setSettings, loaded };
}
