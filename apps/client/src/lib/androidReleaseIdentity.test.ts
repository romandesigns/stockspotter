import { expect, test } from "bun:test";
import { nativeSourceCommit, sourceCommit, backendSourceCommit } from "../../../mobile/src/releaseIdentity";
import { decodeAlertMemory } from "../../../mobile/src/attentionMemory";
import { singleFlightPermission } from "../../../mobile/src/notificationPermission";
import { previousSession } from "../../../mobile/src/tradingDays";
const sha = "a".repeat(40);
test("previous snapshot date uses New York calendar day at UTC rollover and DST", () => {
  expect(previousSession(new Date("2026-10-10T03:30:00Z"))).toBe("2026-10-08");
  expect(previousSession(new Date("2026-10-12T12:00:00Z"))).toBe("2026-10-09");
  expect(previousSession(new Date("2026-01-10T04:30:00Z"))).toBe("2026-01-08");
});
test("native source is read only from a full stamped APK version", () => {
  expect(nativeSourceCommit(`1.0.0+git.${sha}`)).toBe(sha);
  for (const value of [null, "1.0.0", `1.0.0+git.${sha.slice(0,7)}`, `1.0.0+git.${sha}.dirty`]) expect(nativeSourceCommit(value)).toBe(null);
  expect(sourceCommit(sha)).toBe(sha); expect(sourceCommit("unknown")).toBe(null);
  expect(backendSourceCommit({ report: { commit: sha } })).toBe(sha);
  for (const value of [null, {}, { commit: sha }, { report: { commit: "latest" } }]) expect(backendSourceCommit(value)).toBe(null);
});
test("permission callers share a prompt; failures/timeout allow future retries", async () => {
  let calls = 0; let finish!: (value:boolean)=>void;
  const gate = singleFlightPermission(() => { calls++; return new Promise(resolve => { finish = resolve; }); }, 30);
  const first = gate(); const second = gate(); expect(first).toBe(second);
  await Promise.resolve(); expect(calls).toBe(1); finish(true); expect(await first).toBe("granted");
  const hanging = gate(); await Promise.resolve(); expect(calls).toBe(2); expect(await hanging).toBe("unknown");
  expect(await gate()).toBe("unknown"); expect(calls).toBe(2); finish(false); await new Promise(resolve => setTimeout(resolve,0));
  const retry = gate(); await Promise.resolve(); expect(calls).toBe(3); finish(false); expect(await retry).toBe("denied");
  const failing = singleFlightPermission(async () => { throw new Error("fixture"); }); expect(await failing()).toBe("unknown");
});
test("persisted alert memory expires, rejects malformed times, and keeps cooldowns", () => {
  const now = 2_000_000;
  const result = decodeAlertMemory(JSON.stringify({ version: 1, seen: [["fresh",now-160000], ["expired",now-160001], ["future",now+30001], ["invalid","not-time"]], cooldown: [["X",now-900000],["OLD",now-900001]] }),now);
  expect([...result.seen.keys()]).toEqual(["fresh"]); expect([...result.cooldown.keys()]).toEqual(["X"]);
  for (const raw of [null, "bad-json", '{}', 'x'.repeat(131073)]) expect(decodeAlertMemory(raw,now).seen.size).toBe(0);
});
