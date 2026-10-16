// Speed-alert settings for the Halt Early-Warning header (2026-10-16,
// approved Alerts board). A bell button that says what is armed ("Alerts
// ≥ 70%", accent-tinted only while alerts are on: the 10% accent is for
// state that should draw the eye) opening a shadcn Popover. "Phone push"
// from the board is left out here: on web/desktop the toast, notification
// and chime are the delivery. The cooldown is fixed and shared with the
// server's speed_alert.rs, so it is stated rather than offered.
import { useState } from "react";
import { Bell, BellOff } from "lucide-react";
import { SPEED_ALERT_THRESHOLDS } from "@stockspotter/shared-types";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Switch } from "@/components/ui/switch";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import type { ClientSpeedSettings } from "../lib/haltSpeedView";

function notificationPermission(): NotificationPermission | "unsupported" {
  try { return "Notification" in window ? Notification.permission : "unsupported"; } catch { return "unsupported"; }
}

export function SpeedAlertSettings(props: { settings: ClientSpeedSettings; onChange: (patch: Partial<ClientSpeedSettings>) => void }) {
  const { settings } = props;
  const [permission, setPermission] = useState(notificationPermission);
  const toggles: { key: "enabled" | "upOnly" | "premarket" | "sound"; label: string; hint?: string }[] = [
    { key: "enabled", label: "Speed alerts" },
    { key: "upOnly", label: "Up moves only" },
    { key: "premarket", label: "Premarket", hint: "from 4:00" },
    { key: "sound", label: "Sound", hint: "chime" },
  ];
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={`halt-alerts-button${settings.enabled ? " halt-alerts-button-on" : ""}`}
          aria-label={settings.enabled ? `Speed alerts on at ${settings.threshold}%, open alert settings` : "Speed alerts off, open alert settings"}
        >
          {settings.enabled ? <Bell aria-hidden="true" /> : <BellOff aria-hidden="true" />}
          <span>{settings.enabled ? `Alerts ≥ ${settings.threshold}%` : "Alerts off"}</span>
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="speed-alert-settings">
        <h3 className="speed-alert-settings-title">Speed alerts</h3>
        <div className="speed-alert-settings-group">
          <span id="speed-alert-threshold-label">Alert when the gauge passes</span>
          <ToggleGroup
            type="single"
            size="sm"
            spacing={0}
            className="halt-segmented"
            aria-labelledby="speed-alert-threshold-label"
            value={String(settings.threshold)}
            onValueChange={(v) => v && props.onChange({ threshold: Number(v) })}
            disabled={!settings.enabled}
          >
            {SPEED_ALERT_THRESHOLDS.map((t) => (
              <ToggleGroupItem key={t} value={String(t)} className="halt-segmented-item">{t}%</ToggleGroupItem>
            ))}
          </ToggleGroup>
          <span className="speed-alert-settings-hint">Only while it is still rising. Starting values, not yet tested.</span>
        </div>
        {toggles.map((t) => (
          <label key={t.key} className="speed-alert-settings-row">
            <span className="speed-alert-settings-label">{t.label}</span>
            {t.hint && <span className="speed-alert-settings-hint">{t.hint}</span>}
            <Switch
              checked={settings[t.key]}
              disabled={t.key !== "enabled" && !settings.enabled}
              onCheckedChange={(checked) => props.onChange({ [t.key]: checked })}
            />
          </label>
        ))}
        <div className="speed-alert-settings-row">
          <span className="speed-alert-settings-label">Once per stock</span>
          <span className="speed-alert-settings-hint">every 10 min</span>
        </div>
        {permission === "default" && settings.enabled && (
          <button
            type="button"
            className="speed-alert-settings-permission"
            onClick={() => {
              try {
                Notification.requestPermission().then(setPermission, () => setPermission(notificationPermission()));
              } catch { setPermission(notificationPermission()); }
            }}
          >
            Also show desktop notifications
          </button>
        )}
        {permission === "denied" && settings.enabled && (
          <span className="speed-alert-settings-hint">Desktop notifications are blocked for this site; the in-app pop-up still shows.</span>
        )}
      </PopoverContent>
    </Popover>
  );
}
