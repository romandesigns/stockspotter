// Mobile half of "sign in once per device" -- see
// packages/shared-types/src/access.ts for why the store is injected rather
// than imported there (expo-secure-store has no web support at all, and that
// module is shared with the web/desktop bundle).
//
// expo-secure-store, not AsyncStorage: AsyncStorage is already a dependency
// here and would have been less work, but it writes plaintext into the app
// sandbox. This value is a credential to the whole live feed, so it belongs in
// the platform keystore -- Android Keystore / iOS Keychain, encrypted at rest
// and out of reach of a filesystem-level backup.
//
// Android caveat worth knowing (SDK 57 docs): unlike iOS, Android does NOT
// preserve SecureStore entries across an uninstall/reinstall, so reinstalling
// the APK signs this device out and the key gets entered once more.
import * as SecureStore from "expo-secure-store";
import type { AccessKeyStorage } from "@stockspotter/shared-types";

const STORAGE_KEY = "stockspotter.accessKey";

export const secureAccessKeyStorage: AccessKeyStorage = {
  async get() {
    try {
      // Resolves to null when there's no entry or the entry was invalidated.
      return await SecureStore.getItemAsync(STORAGE_KEY);
    } catch {
      return null;
    }
  },
  async set(key: string) {
    try {
      await SecureStore.setItemAsync(STORAGE_KEY, key);
    } catch {
      // Signed in for this launch only; never worth blocking sign-in over.
    }
  },
  async clear() {
    try {
      await SecureStore.deleteItemAsync(STORAGE_KEY);
    } catch {
      // Unreadable is equivalent to cleared for our purposes.
    }
  },
};
