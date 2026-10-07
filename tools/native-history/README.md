# Native history hook regression tests

Run from a checkout with locked workspace dependencies installed. Requires Bun, Node, playwright or playwright-core, and a compatible Chromium executable. Set PLAYWRIGHT_MODULE to the installed module and CHROME_PATH to the browser executable. No dependency downloads are performed by this harness.

Candidate:

```
bun tools/native-history/build.ts
node tools/native-history/run.cjs candidate.json
```

Baseline (same harness and installed React runtime):

```
bun tools/native-history/build.ts --source-root <baseline-checkout>/apps/mobile/src --deps-root <candidate-checkout>/apps/mobile
node tools/native-history/run.cjs baseline.json
```

Rebuild before every run when switching source. Default source and dependencies are the containing checkout's mobile app. Explicitly use the same --deps-root for comparisons. The build resolves one React/ReactDOM instance for fixture and hook. Eight identical assertions observe every React commit through useLayoutEffect, before passive effects can hide history leakage. The real hook and resampler run; authenticatedFetch and HTTP configuration are replaced for deterministic deferred responses.

Expected historical baseline7cb2ba0:3/8 pass; N1 candidate8/8 pass. Nonzero runner exit means a failed assertion or page error. Browser React coverage is not native bridge, WebView, touch, network transport, authentication or physical-device proof. No reconnect/finality claims. Generated dist and result JSON should not be bundled in release source.
