import { useEffect, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { Info } from "lucide-react";
import { Button } from "./ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { DesktopUpdateSession, type DesktopUpdateState } from "../lib/desktopUpdateSession";

export function DesktopReleaseControl() {
  const native = isTauri();
  const [version, setVersion] = useState("unknown");
  const [state, setState] = useState<DesktopUpdateState>({ kind: "idle" });
  const sessionRef = useRef<DesktopUpdateSession | null>(null);

  useEffect(() => {
    if (!native) return;
    let active = true;
    const session = new DesktopUpdateSession(async () => {
      const { check } = await import("@tauri-apps/plugin-updater");
      return check({ timeout: 15000, allowDowngrades: false });
    }, setState);
    sessionRef.current = session;
    // Let StrictMode cancel its replayed first setup before native IPC starts.
    queueMicrotask(() => {
      if (!active) return;
      void import("@tauri-apps/api/app").then(({ getVersion }) => getVersion()).then((value) => {
        if (active) setVersion(value);
      }).catch(() => { if (active) setVersion("unavailable"); });
      void session.check();
    });
    return () => {
      active = false;
      sessionRef.current = null;
      session.dispose();
    };
  }, [native]);

  if (!native) return null;
  const busy = state.kind === "checking" || state.kind === "installing";
  const message = state.kind === "none" ? "No newer published desktop release was found."
    : state.kind === "checking" ? "Checking published desktop releases…"
    : state.kind === "available" ? `Desktop ${state.version} is available.`
    : state.kind === "installing" ? "Downloading and installing. The app will close."
    : state.kind === "handed-off" ? "Installer started. Reopen Stockspotter if needed."
    : state.kind === "error" ? state.message : "Update check has not run.";

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="ghost" size="icon" className="app-rail-btn" aria-label={state.kind === "available" ? "Desktop update available" : "Desktop version and updates"} title={state.kind === "available" ? "Desktop update available" : "Desktop version and updates"}>
          <Info size={18} className={state.kind === "available" ? "text-green-400" : undefined} />
        </Button>
      </PopoverTrigger>
      <PopoverContent side="right" className="w-80">
        <h2 className="font-medium">Desktop version and updates</h2>
        <p>Installed desktop: {version}</p>
        <p className="break-all text-xs">Interface source: {__STOCKSPOTTER_BUILD__.sourceCommit}</p>
        <p className="text-xs">Source tree: {__STOCKSPOTTER_BUILD__.sourceTree}</p>
        <p className="text-xs text-muted-foreground">This identifies the installed interface. Server data can change separately.</p>
        <p role="status">{message}</p>
        <Button variant="outline" disabled={busy || state.kind === "handed-off"} onClick={() => void sessionRef.current?.check()}>Check for updates</Button>
        {state.kind === "available" && <Button onClick={() => void sessionRef.current?.install()}>Install update and close app</Button>}
      </PopoverContent>
    </Popover>
  );
}
