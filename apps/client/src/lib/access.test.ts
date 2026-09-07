import { afterEach, expect, test } from "bun:test";
import { authenticatedFetch, setAccessKey } from "@stockspotter/shared-types";

const originalFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = originalFetch; setAccessKey(""); });

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
