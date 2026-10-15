// The order-flow protocol is web-only by design, and this is the test
// that keeps it that way.
//
// Its types, decoder and watch rules live in apps/client/src/lib rather
// than in @stockspotter/shared-types because the shared package is the
// contract the mobile app's handshake and panels read, and mobile changes
// are behind the Android advisory gate. Putting `subscribe_order_flow` or
// `OrderFlowSummary` there would commit both apps to a message only this
// client speaks, and would shadow the ordinary shared protocol that every
// panel already depends on.
//
// Mobile parity is deferred, not abandoned: when that gate opens the move
// is to lift these three files into shared-types unchanged. Until then an
// import creeping back the other way is the regression to catch, because
// it would compile and pass every other test.
import { expect, test } from "bun:test";

const WEB_ONLY = ["src/lib/orderFlow.ts", "src/lib/orderFlowWatch.ts", "src/components/OrderFlowStrip.tsx"];

async function importsOf(path: string): Promise<string[]> {
  const source = await Bun.file(new URL(`../../${path}`, import.meta.url)).text();
  return [...source.matchAll(/^import[^;]*?from\s+"([^"]+)"/gms)].map((m) => m[1]);
}

test("the order-flow protocol does not reach into the shared package", async () => {
  for (const path of WEB_ONLY) {
    const specifiers = await importsOf(path);
    expect(
      specifiers.filter((s) => s.startsWith("@stockspotter/")),
      path,
    ).toEqual([]);
  }
});

test("the hook is the only place the shared authenticated fetch is reached", async () => {
  // `orderFlowWatch` takes `fetch` as an option precisely so it stays
  // free of the shared package; `useOrderFlow` is the one seam that
  // supplies the real one. The injected-fetch signature itself is not
  // asserted here: `tsc -b` checks it, and a source-text match on a type
  // annotation only breaks when the annotation is reformatted.
  expect(await importsOf("src/lib/useOrderFlow.ts")).toContain("@stockspotter/shared-types");
  // And the watcher it wraps reaches the network only through what it is
  // given: no global `fetch`, no shared client of its own.
  const watch = await Bun.file(new URL("../../src/lib/orderFlowWatch.ts", import.meta.url)).text();
  expect(watch).not.toMatch(/\bwindow\.fetch\b|(?<![.\w])fetch\s*\(/);
});
