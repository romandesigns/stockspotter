import { useEffect, useRef, useState } from "react";
import { View } from "react-native";
import * as Application from "expo-application";
import Constants from "expo-constants";
import * as Updates from "expo-updates";
import { authenticatedFetch } from "@stockspotter/shared-types";
import { HTTP_URL, WS_URL } from "../config";
import { backendSourceCommit, nativeSourceCommit, sourceCommit } from "../releaseIdentity";
import { Button } from "./ui/button";
import { Text } from "./ui/text";

export function ReleaseDiagnostics({ visible }: { visible: boolean }) {
  const { currentlyRunning } = Updates.useUpdates();
  const release = Constants.expoConfig?.extra?.stockspotterRelease;
  const jsCommit = release?.tree === "modified" ? null : sourceCommit(release?.commit);
  const [backend, setBackend] = useState<{ commit: string | null; checkedAt: string | null; status: string }>({ commit: null, checkedAt: null, status: "Not checked" });
  const [attempt, setAttempt] = useState(0);
  const loading = useRef(false);
  const [checking, setChecking] = useState(false);
  useEffect(() => {
    if (!visible) return;
    let cancelled = false;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 8000);
    loading.current = true;
    setChecking(true);
    setBackend({ commit: null, checkedAt: null, status: "Checking backend…" });
    authenticatedFetch(`${HTTP_URL}/research/completeness`, { signal: controller.signal, cache: "no-store" })
      .then(async response => {
        if (!response.ok) throw new Error("Backend unavailable");
        const length = response.headers.get("Content-Length");
        if (length !== null && (!/^\d+$/.test(length) || Number(length) > 128 * 1024)) throw new Error("Unexpected backend response");
        const text = await response.text();
        if (text.length > 128 * 1024) throw new Error("Unexpected backend response");
        const commit = backendSourceCommit(JSON.parse(text));
        if (!commit) throw new Error("Backend source is unknown");
        if (!cancelled) setBackend({ commit, checkedAt: new Date().toISOString(), status: "Backend replied" });
      })
      .catch(() => { if (!cancelled) setBackend({ commit: null, checkedAt: null, status: "Backend source unavailable — retry" }); })
      .finally(() => { clearTimeout(timer); if (!cancelled) { loading.current = false; setChecking(false); } });
    return () => { cancelled = true; loading.current = false; clearTimeout(timer); controller.abort(); };
  }, [visible, attempt]);
  function row(label: string, value: string | null | undefined) {
    return <View className="py-1"><Text variant="muted" className="text-[11px]">{label}</Text><Text selectable mono className="text-[11px]">{value || "Unknown"}</Text></View>;
  }
  return <View className="px-4 pb-4">
    <Text className="text-[13px] font-semibold">Version and connection</Text>
    {row("Installed APK version / build", `${Application.nativeApplicationVersion ?? "Unknown"} / ${Application.nativeBuildVersion ?? "Unknown"}`)}
    {row("Installed APK source commit", nativeSourceCommit(Application.nativeApplicationVersion))}
    {row("Running JavaScript source commit", jsCommit)}
    {row("Source identity origin", release?.origin)}
    {row("Source tree verification", release?.tree)}
    {row("Running code", Updates.isEnabled ? (currentlyRunning.isEmbeddedLaunch ? "Bundled in APK" : "Downloaded Expo update") : "Development or updates unavailable")}
    {row("Running update ID", currentlyRunning.updateId)}
    {row("Update creation time", currentlyRunning.createdAt?.toISOString())}
    {row("Runtime / channel", `${currentlyRunning.runtimeVersion ?? "Unknown"} / ${currentlyRunning.channel ?? "Unknown"}`)}
    {currentlyRunning.isEmergencyLaunch && <Text variant="warning" className="text-xs">Expo used its emergency fallback.</Text>}
    {row("Backend address", HTTP_URL)}
    {row("WebSocket address", WS_URL)}
    {row("HTTP backend compiled source commit", backend.commit)}
    {row("Backend checked at", backend.checkedAt)}
    <Text variant="muted" className="text-[11px]">{backend.status}. Source identities alone do not prove feature parity or device test results.</Text>
    <Button variant="outline" size="sm" disabled={checking} accessibilityLabel="Refresh backend version" onPress={() => { if (!loading.current) setAttempt(value => value + 1); }}><Text className="text-xs">Refresh backend version</Text></Button>
  </View>;
}
