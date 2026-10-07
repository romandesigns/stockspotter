import { afterEach, expect, test } from "bun:test";
import { authenticatedFetch, configureAccessKeyStorage, getAccessKey, restoreAccessKey, setAccessKey, type AccessKeyStorage } from "@stockspotter/shared-types";

const originalFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = originalFetch; setAccessKey(""); configureAccessKeyStorage(null); });

function fakeStorage(initial: string | null = null): AccessKeyStorage & { value: string | null } {
  return {
    value: initial,
    async get() { return this.value; },
    async set(key: string) { this.value = key; },
    async clear() { this.value = null; },
  };
}

test("authenticated requests carry the session key in a header and preserve request options", async () => {
  let received: { input: RequestInfo | URL; init?: RequestInit } | undefined;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    received = { input, init };
    return new Response("{}");
  }) as typeof fetch;
  setAccessKey("private-session-key");
  await authenticatedFetch("https://example.invalid/assess", {
    method: "POST", body: "{}", headers: { "Content-Type": "application/json" },
  });
  expect(received?.input).toBe("https://example.invalid/assess");
  expect(received?.init?.method).toBe("POST");
  expect(received?.init?.body).toBe("{}");
  const headers = new Headers(received?.init?.headers);
  expect(headers.get("Authorization")).toBe("Bearer private-session-key");
  expect(headers.get("Content-Type")).toBe("application/json");
});

test("clearing the session key removes it from subsequent requests", async () => {
  let headers: Headers | undefined;
  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
    headers = new Headers(init?.headers);
    return new Response("{}");
  }) as typeof fetch;
  setAccessKey("old-key");
  setAccessKey("");
  await authenticatedFetch("https://example.invalid/health");
  expect(headers?.has("Authorization")).toBe(false);
});

test("a key persisted on this device is restored without re-entry", async () => {
  configureAccessKeyStorage(fakeStorage("remembered-key"));
  expect(getAccessKey()).toBe("");
  await expect(restoreAccessKey()).resolves.toBe("remembered-key");
  expect(getAccessKey()).toBe("remembered-key");
});

test("signing in persists the key so the next launch skips the gate", async () => {
  const storage = fakeStorage();
  configureAccessKeyStorage(storage);
  setAccessKey("newly-entered-key");
  await Promise.resolve();
  expect(storage.value).toBe("newly-entered-key");
});

test("clearing a rejected key also removes it from the device", async () => {
  const storage = fakeStorage("stale-key");
  configureAccessKeyStorage(storage);
  await restoreAccessKey();
  setAccessKey("");
  await Promise.resolve();
  expect(storage.value).toBeNull();
  expect(getAccessKey()).toBe("");
});

test("with no storage registered the key stays memory-only", async () => {
  await expect(restoreAccessKey()).resolves.toBe("");
  setAccessKey("session-only");
  expect(getAccessKey()).toBe("session-only");
});

test("a storage backend that throws never blocks sign-in", async () => {
  configureAccessKeyStorage({
    get() { throw new Error("keystore locked"); },
    set() { throw new Error("keystore locked"); },
    clear() { throw new Error("keystore locked"); },
  } as unknown as AccessKeyStorage);
  await expect(restoreAccessKey()).resolves.toBe("");
  expect(() => setAccessKey("still-works")).not.toThrow();
  expect(getAccessKey()).toBe("still-works");
});
