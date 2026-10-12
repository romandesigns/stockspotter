import { describe, expect, test } from "bun:test";
import { DesktopUpdateSession, type DesktopUpdate, type DesktopUpdateState } from "./desktopUpdateSession";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
function fixture(install: () => Promise<void> = async () => {}) {
  let closes = 0;
  let installs = 0;
  const update: DesktopUpdate = { version: "0.9.3", close: async () => { closes++; }, downloadAndInstall: async () => { installs++; await install(); } };
  return { update, closes: () => closes, installs: () => installs };
}

describe("desktop native update ownership", () => {
  test("a never-settling close cannot wedge a subsequent check", async () => {
    const pending = deferred<void>();
    let checks = 0;
    const states: DesktopUpdateState[] = [];
    const item = fixture();
    item.update.close = () => pending.promise;
    const session = new DesktopUpdateSession(async () => ++checks === 1 ? item.update : null, (state) => states.push(state));
    await session.check();
    await session.check();
    expect(checks).toBe(2);
    expect(states.at(-1)).toEqual({ kind: "none" });
    pending.resolve();
  });

  test("observer failure cannot leak or wedge native installation ownership", async () => {
    const item = fixture();
    const session = new DesktopUpdateSession(async () => item.update, () => { throw new Error("observer failed"); });
    await session.check();
    await session.install();
    session.dispose();
    expect(item.installs()).toBe(1);
    expect(item.closes()).toBe(1);
  });

  test("startup/check overlap is single-flight and never installs automatically", async () => {
    const pending = deferred<DesktopUpdate | null>();
    const item = fixture();
    let checks = 0;
    const states: DesktopUpdateState[] = [];
    const session = new DesktopUpdateSession(() => { checks++; return pending.promise; }, (state) => states.push(state));
    const first = session.check();
    await session.check();
    pending.resolve(item.update);
    await first;
    expect(checks).toBe(1);
    expect(item.installs()).toBe(0);
    expect(states.at(-1)).toEqual({ kind: "available", version: "0.9.3" });
    session.dispose();
    expect(item.closes()).toBe(1);
  });

  test("late result after unmount is closed and not published", async () => {
    const pending = deferred<DesktopUpdate | null>();
    const started = deferred<void>();
    const item = fixture();
    const states: DesktopUpdateState[] = [];
    const session = new DesktopUpdateSession(() => { started.resolve(); return pending.promise; }, (state) => states.push(state));
    const first = session.check();
    await started.promise;
    session.dispose();
    pending.resolve(item.update);
    await first;
    expect(item.closes()).toBe(1);
    expect(states).toEqual([{ kind: "checking" }]);
  });

  test("a superseding check closes old resources and clears availability", async () => {
    const item = fixture();
    const states: DesktopUpdateState[] = [];
    let checks = 0;
    const session = new DesktopUpdateSession(async () => ++checks === 1 ? item.update : null, (state) => states.push(state));
    await session.check();
    await session.check();
    expect(item.closes()).toBe(1);
    expect(states.at(-1)).toEqual({ kind: "none" });
    await session.install();
    expect(item.installs()).toBe(0);
  });

  test("offline check is an error, never a current-version assertion, and retries", async () => {
    let checks = 0;
    const states: DesktopUpdateState[] = [];
    const session = new DesktopUpdateSession(async () => { if (++checks === 1) throw new Error("offline"); return null; }, (state) => states.push(state));
    await session.check();
    expect(states.at(-1)?.kind).toBe("error");
    await session.check();
    expect(states.at(-1)).toEqual({ kind: "none" });
  });

  test("installation excludes overlapping actions and survives unmount without early close", async () => {
    const pending = deferred<void>();
    const item = fixture(() => pending.promise);
    const states: DesktopUpdateState[] = [];
    let checks = 0;
    const session = new DesktopUpdateSession(async () => { checks++; return item.update; }, (state) => states.push(state));
    await session.check();
    const install = session.install();
    await session.install();
    await session.check();
    session.dispose();
    expect(item.installs()).toBe(1);
    expect(checks).toBe(1);
    expect(item.closes()).toBe(0);
    pending.resolve();
    await install;
    expect(item.closes()).toBe(1);
    expect(states.at(-1)).toEqual({ kind: "installing" });
  });

  test("signature/install failure clears the resource and requires a new check", async () => {
    const item = fixture(async () => { throw new Error("signature rejected"); });
    const states: DesktopUpdateState[] = [];
    const session = new DesktopUpdateSession(async () => item.update, (state) => states.push(state));
    await session.check();
    await session.install();
    expect(states.at(-1)?.kind).toBe("error");
    expect(item.closes()).toBe(1);
    await session.install();
    expect(item.installs()).toBe(1);
  });
});
