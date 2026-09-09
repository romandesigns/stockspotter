// Web/desktop half of "sign in once per device" -- see
// packages/shared-types/src/access.ts for why the store is injected rather
// than imported there.
//
// localStorage rather than sessionStorage: the whole point is surviving a
// browser restart, which sessionStorage explicitly doesn't. This is the same
// tradeoff every "stay signed in" checkbox makes -- the key becomes readable
// by any script on this origin, which for a single-origin static bundle with
// no third-party scripts is the exposure we already accept for the key being
// in the page's memory at all.
//
// Every access is wrapped: Safari private mode, a browser configured to block
// site data, and the artifact/thumbnail renderers all throw on the accessor
// itself rather than returning null.
import type { AccessKeyStorage } from "@stockspotter/shared-types";

const STORAGE_KEY = "stockspotter:accessKey";

export const browserAccessKeyStorage: AccessKeyStorage = {
  async get() {
    try {
      return window.localStorage.getItem(STORAGE_KEY);
    } catch {
      return null;
    }
  },
  async set(key: string) {
    try {
      window.localStorage.setItem(STORAGE_KEY, key);
    } catch {
      // Signed in for this session only; nothing further to do.
    }
  },
  async clear() {
    try {
      window.localStorage.removeItem(STORAGE_KEY);
    } catch {
      // Already unreachable, so already effectively cleared.
    }
  },
};
