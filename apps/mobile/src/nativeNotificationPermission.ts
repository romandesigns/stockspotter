import * as Notifications from "expo-notifications";
import { singleFlightPermission } from "./notificationPermission";
export const requestNotificationPermission = singleFlightPermission(async () => {
  const current = await Notifications.getPermissionsAsync();
  if (current.granted) return true;
  if (!current.canAskAgain) return false;
  return (await Notifications.requestPermissionsAsync()).granted;
});
const channels = new Map<string, Promise<boolean>>();
export function ensureNotificationChannel(id: string, name: string) {
  const previous = channels.get(id);
  if (previous) return previous;
  const pending = Notifications.setNotificationChannelAsync(id, {
    name, importance: Notifications.AndroidImportance.HIGH, sound: "default",
  }).then(() => true).catch(() => { channels.delete(id); return false; });
  channels.set(id, pending);
  return pending;
}