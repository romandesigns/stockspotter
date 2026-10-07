import { useEffect, useState, type ReactNode } from "react";
import { View, Text, TextInput, Pressable } from "react-native";
import { configureAccessKeyStorage, restoreAccessKey, setAccessKey } from "@stockspotter/shared-types";
import { secureAccessKeyStorage } from "./accessStorage";
import { HTTP_URL } from "./config";

// Registered at module scope so a key persisted by a previous launch is
// reachable before the first render -- see accessStorage.ts.
configureAccessKeyStorage(secureAccessKeyStorage);

type Verdict = "accepted" | "rejected" | "unknown";

// "unknown" is deliberately distinct from "rejected": only an explicit 401
// means the stored key is wrong. A phone that wakes on a dead connection, or
// hits the backend mid-restart, must not be signed out for it.
async function verifyKey(key: string): Promise<Verdict> {
  try {
    const response = await fetch(`${HTTP_URL}/health`, { headers: { Authorization: `Bearer ${key}` } });
    if (response.ok) return "accepted";
    return response.status === 401 ? "rejected" : "unknown";
  } catch {
    return "unknown";
  }
}

export function AccessGate({ children }: { children: ReactNode }) {
  const [allowed, setAllowed] = useState(false);
  const [restoring, setRestoring] = useState(true);
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const stored = await restoreAccessKey();
      if (cancelled) return;
      if (stored) {
        const verdict = await verifyKey(stored);
        if (cancelled) return;
        // Rotating STOCKSPOTTER_API_TOKEN invalidates every stored copy at
        // once; clearing turns that into a normal sign-in prompt rather than
        // an app that silently 401s on every request.
        if (verdict === "rejected") setAccessKey("");
        else setAllowed(true);
      }
      setRestoring(false);
    })();
    return () => { cancelled = true; };
  }, []);

  // Blank (not the form) while the keystore read and check are in flight --
  // otherwise an already-signed-in phone flashes a password prompt on every
  // cold start, which is the exact annoyance persistence exists to remove.
  if (restoring) return <View style={{ flex: 1, backgroundColor: "#101210" }} />;
  if (allowed) return children;
  return <View style={{ flex: 1, backgroundColor: "#101210", justifyContent: "center", padding: 32, gap: 20 }}>
    <Text style={{ color: "white", fontSize: 28 }}>Stockspotter</Text>
    <Text style={{ color: "#ddd" }}>Enter your private access key</Text>
    <TextInput accessibilityLabel="Private access key" secureTextEntry autoCapitalize="none" autoCorrect={false}
      value={key} onChangeText={setKey} style={{ backgroundColor: "#242824", color: "white", padding: 16, borderRadius: 8 }} />
    <Pressable accessibilityRole="button" disabled={busy || !key.trim()} style={{ backgroundColor: "#86ce8d", padding: 16, borderRadius: 8 }} onPress={async () => {
      setBusy(true); setError("");
      try {
        const response = await fetch(`${HTTP_URL}/health`, { headers: { Authorization: `Bearer ${key.trim()}` } });
        if (!response.ok) throw new Error(response.status === 401 ? "Access key not recognized." : "Service unavailable. Try again shortly.");
        setAccessKey(key.trim()); setKey(""); setAllowed(true);
      } catch { setError("Unable to sign in. Check your access key and connection."); }
      finally { setBusy(false); }
    }}><Text>{busy ? "Connecting…" : "Continue"}</Text></Pressable>
    {error ? <Text accessibilityRole="alert" style={{ color: "#ffbbbb" }}>{error}</Text> : null}
  </View>;
}
