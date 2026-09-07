import { useState, type ReactNode } from "react";
import { View, Text, TextInput, Pressable } from "react-native";
import { getAccessKey, setAccessKey } from "@stockspotter/shared-types";
import { HTTP_URL } from "./config";

export function AccessGate({ children }: { children: ReactNode }) {
  const [allowed, setAllowed] = useState(Boolean(getAccessKey()));
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
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
