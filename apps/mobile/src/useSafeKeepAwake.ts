// Defensive wrapper around expo-keep-awake -- REQUIRED because it's a
// real native module (Kotlin/Swift, see node_modules/expo-keep-awake/
// android|ios), and this app is distributed as a compiled native binary
// via EAS Build, updated between real builds via OTA (expo-updates).
// An OTA push only ever delivers JS -- it cannot add native code to an
// already-installed binary. expo-keep-awake's own `ExpoKeepAwake.ts`
// calls expo-modules-core's `requireNativeModule('ExpoKeepAwake')` at
// top-level import time, which THROWS synchronously
// ("Cannot find native module 'ExpoKeepAwake'") if the native module
// isn't compiled in -- confirmed by reading requireNativeModule's own
// source. Found and fixed 2026-09-03, same day this dependency was
// added: a static `import { useKeepAwake } from "expo-keep-awake"` at
// the top of ChartScreen.tsx (which App.tsx imports eagerly, so this
// runs at app startup, not just when a chart opens) would have crashed
// every install that received this OTA before its next real native
// build -- caught before confirming it live, not after a real report.
//
// Fix: a runtime-guarded `require()` inside try/catch (module load, not
// component render, so this branch is decided once and stays stable for
// the whole session -- safe with React's Rules of Hooks despite looking
// conditional). Falls back to a true no-op hook until a real EAS Build
// ships this native module and the app is reinstalled/updated from the
// store -- keep-awake simply won't activate on such a device until
// then, which is a real but honest degradation, not a crash.
import { useEffect } from "react";

let realUseKeepAwake: (() => void) | null = null;
try {
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  realUseKeepAwake = require("expo-keep-awake").useKeepAwake;
} catch {
  realUseKeepAwake = null;
}

function noopKeepAwake() {
  useEffect(() => {}, []);
}

export const useSafeKeepAwake: () => void = realUseKeepAwake ?? noopKeepAwake;
