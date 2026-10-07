// The private access key is held in memory for the app session; never put it
// in URLs or builds.
//
// A client may additionally register a platform-appropriate persistent store
// (`configureAccessKeyStorage`) so a device is signed in ONCE rather than on
// every cold start -- the original in-memory-only behaviour meant retyping a
// 48-character key on a phone keyboard every single launch. Storage is
// injected rather than imported here on purpose: this module is shared by the
// web/desktop bundle and the React Native app, and the right store differs
// per platform (`expo-secure-store` is Keychain/Keystore-backed but has no web
// support at all; `localStorage` doesn't exist in React Native). Registering
// nothing keeps the old memory-only behaviour, which is what the tests and any
// non-browser caller get.
let accessKey = "";

export interface AccessKeyStorage {
  /** Resolves to the stored key, or null when this device isn't signed in. */
  get(): Promise<string | null>;
  set(key: string): Promise<void>;
  clear(): Promise<void>;
}

let storage: AccessKeyStorage | null = null;

export function configureAccessKeyStorage(next: AccessKeyStorage | null): void {
  storage = next;
}

/**
 * Loads a previously-persisted key into memory. Returns the key now in effect
 * ("" when this device isn't signed in). Safe to call before any storage has
 * been registered -- it simply resolves to whatever is already in memory.
 *
 * Callers should still verify the restored key against the server before
 * trusting it: a key persisted on this device says nothing about whether the
 * server still accepts it (rotating STOCKSPOTTER_API_TOKEN invalidates every
 * stored copy at once), and a stale key would otherwise fail every request
 * with no sign-in screen offered.
 */
export async function restoreAccessKey(): Promise<string> {
  if (accessKey) return accessKey;
  try {
    const stored = await storage?.get();
    if (stored) accessKey = stored;
  } catch {
    // A locked keystore, cleared site data, or a browser configured to block
    // storage is an ordinary signed-out state, not an error worth surfacing.
  }
  return accessKey;
}

export function getAccessKey(): string {
  return accessKey;
}

export function setAccessKey(key: string): void {
  accessKey = key;
  if (!storage) return;
  // The synchronous try/catch is not redundant with the .catch(): a platform
  // store can throw on the call itself rather than return a rejected promise
  // (expo-notifications does exactly this in Expo Go), and such a throw would
  // otherwise escape a plain promise chain and take the app down.
  try {
    void (key ? storage.set(key) : storage.clear()).catch(() => {});
  } catch {
    // Persisting is a convenience; failing to persist must never block
    // sign-in, which has already succeeded in memory above.
  }
}

export function authenticatedFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const headers = new Headers(init?.headers);
  if (accessKey) headers.set("Authorization", `Bearer ${accessKey}`);
  return fetch(input, { ...init, headers });
}
