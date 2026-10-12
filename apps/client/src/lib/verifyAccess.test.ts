import { describe, expect, test } from "bun:test";
import { verifyAccessKey } from "./verifyAccess";

describe("bounded access verification", () => {
  test("only explicit 401 rejects a key; service errors remain unknown", async () => {
    const server = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: (request) => new Response("", { status: Number(new URL(request.url).pathname.slice(1)) }) });
    try {
      for (const [status, verdict] of [[200, "accepted"], [401, "rejected"], [403, "unknown"], [503, "unknown"]] as const) {
        expect(await verifyAccessKey(`${server.url}${status}`, "fixture-only" )).toBe(verdict);
      }
    } finally { server.stop(true); }
  });

  test("a real stalled HTTP response aborts and remains unknown", async () => {
    const server = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new Promise<Response>(() => {}) });
    try {
      expect(await verifyAccessKey(server.url.toString(), "fixture-only", { timeoutMs: 30 })).toBe("unknown");
    } finally { server.stop(true); }
  });

  test("unmount abort and already-aborted requests remain unknown", async () => {
    const server = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new Promise<Response>(() => {}) });
    try {
      const controller = new AbortController();
      const pending = verifyAccessKey(server.url.toString(), "fixture-only", { signal: controller.signal });
      controller.abort();
      expect(await pending).toBe("unknown");
      expect(await verifyAccessKey(server.url.toString(), "fixture-only", { signal: controller.signal })).toBe("unknown");
    } finally { server.stop(true); }
  });
});
