// Builds the browser fixture with Bun's own bundler (already present in
// the locked toolchain -- no new dependency, no Vite config change, no
// copy of apps/client anywhere).
//
// Usage:
//   bun tools/chart-recovery/build.ts
//   bun tools/chart-recovery/build.ts --source-root <path-to-apps/client/src>
//
// --source-root is the baseline switch: point it at another worktree's
// apps/client/src (for example the clean deployed-web release) and the
// SAME harness, the same stubs and the same tests run against that
// source instead. Nothing else about the build changes, so a pass/fail
// difference between two roots is a difference in the application code.
//
// Exactly three application modules are replaced (fixture/stubs/):
// superChartEngine (recorded), useAssessment and useWakeLock (inert),
// plus config (so every URL stays same-origin). Everything else --
// ChartPanel, ChartSlot, SuperChart, useHistoricalBackfill, derive,
// chartIndicators, the Radix/shadcn toolbar -- is the real module.

import { existsSync, rmSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { BunPlugin } from "bun";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const clientDir = join(repoRoot, "apps", "client");
const clientRequire = createRequire(join(clientDir, "package.json"));

function argValue(name: string): string | undefined {
  const flag = `--${name}`;
  const index = Bun.argv.indexOf(flag);
  if (index !== -1) return Bun.argv[index + 1];
  const inline = Bun.argv.find((a) => a.startsWith(`${flag}=`));
  return inline?.slice(flag.length + 1);
}

const sourceRoot = resolve(argValue("source-root") ?? join(repoRoot, "apps", "client", "src"));
const outDir = resolve(argValue("out") ?? join(here, "dist"));

if (!existsSync(join(sourceRoot, "components", "panels", "ChartPanel.tsx"))) {
  throw new Error(`--source-root does not look like apps/client/src: ${sourceRoot}`);
}

const posix = (p: string) => p.replaceAll("\\", "/");
const STUBS = new Set(["superChartEngine", "useAssessment", "useWakeLock", "config"]);
const EXTENSIONS = ["", ".tsx", ".ts", ".jsx", ".js", "/index.tsx", "/index.ts", "/index.js"];

function resolveFile(base: string): string {
  for (const extension of EXTENSIONS) {
    const candidate = base + extension;
    if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  }
  throw new Error(`harness build: cannot resolve ${base}`);
}

/** Workspace dependencies that must resolve from THIS worktree's
 * apps/client no matter who imported them. Bun installs them into
 * apps/client/node_modules, so nothing under tools/ can resolve them on
 * its own, and a --source-root in another worktree would otherwise pull
 * that worktree's own copy -- two React runtimes in one bundle, which
 * breaks hooks before any test gets to assert anything. */
const CLIENT_DEPS = /^(react|react-dom|@stockspotter\/shared-types)(\/|$)/;

function resolveClientDep(specifier: string): string {
  // Bun's resolver first (honours the same "exports"/browser conditions
  // Vite applies to the app itself); createRequire is the fallback.
  try {
    return Bun.resolveSync(specifier, clientDir);
  } catch {
    return clientRequire.resolve(specifier);
  }
}

const harnessPlugin: BunPlugin = {
  name: "chart-recovery-harness",
  setup(build) {
    // Applied to every importer -- the fixture, the source root under
    // test and the packages themselves -- so there is exactly one
    // react/react-dom/shared-types instance in the bundle.
    build.onResolve({ filter: CLIENT_DEPS }, (args) => ({ path: resolveClientDep(args.path) }));

    // The fixture's own handle on the source root under test.
    build.onResolve({ filter: /^app-src\// }, (args) => ({
      path: resolveFile(join(sourceRoot, args.path.slice("app-src/".length))),
    }));

    // The app's own "@/..." alias (shadcn components use it everywhere).
    build.onResolve({ filter: /^@\// }, (args) => ({
      path: resolveFile(join(sourceRoot, args.path.slice(2))),
    }));

    // Stub redirection, applied only to imports made from the source
    // root under test or from the fixture itself -- never to anything
    // in node_modules that happens to import a module of the same name.
    build.onResolve({ filter: /(^|\/)(superChartEngine|useAssessment|useWakeLock|config)$/ }, (args) => {
      const importer = posix(args.importer ?? "");
      if (!importer.startsWith(posix(sourceRoot)) && !importer.startsWith(posix(here))) return undefined;
      const name = args.path.split("/").pop();
      if (!name || !STUBS.has(name)) return undefined;
      return { path: join(here, "fixture", "stubs", `${name}.ts`) };
    });

    // No stylesheet is loaded on purpose: the app's index.css needs the
    // Tailwind pipeline, and this harness asserts lifecycle, never
    // appearance. A component-level CSS import (none today) must not
    // break the build if one is added later.
    build.onResolve({ filter: /\.css$/ }, (args) => ({ path: args.path, namespace: "harness-empty-css" }));
    build.onLoad({ filter: /.*/, namespace: "harness-empty-css" }, () => ({ contents: "export default {};", loader: "js" }));
  },
};

// Clear the output first. Without this, a FAILED build left the previous
// dist/ in place and run-tests.cjs happily ran it -- which once produced a
// clean 13/13 against the wrong --source-root. A missing bundle must fail
// loudly, never fall back to a stale one.
rmSync(outDir, { recursive: true, force: true });

const result = await Bun.build({
  entrypoints: [join(here, "fixture", "harness.tsx")],
  outdir: outDir,
  target: "browser",
  format: "esm",
  naming: "[name].js",
  sourcemap: argValue("sourcemap") === "inline" ? "inline" : "none",
  // React's development build reads this; the dev build is deliberate
  // (StrictMode double-invocation and useSyncExternalStore warnings are
  // part of what these tests are looking at).
  define: { "process.env.NODE_ENV": JSON.stringify("development") },
  plugins: [harnessPlugin],
});

if (!result.success) {
  for (const log of result.logs) console.error(log);
  throw new Error("harness build failed");
}

const html = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Stockspotter chart lifecycle harness</title>
    <style>
      /* Layout only -- just enough for the chart container to have real
         width/height, since the app's own index.css is a Tailwind build
         this harness deliberately does not run. Nothing here is asserted
         on visually. */
      :root { --harness-height: 480px; }
      html, body { margin: 0; padding: 0; height: 100%; font-family: system-ui, sans-serif; }
      #root { width: 900px; height: var(--harness-height); }
      .panel, .panel-body, .chart-slot, .super-chart-panel { display: flex; flex-direction: column; min-height: 0; height: 100%; }
      .chart-multiview-grid { display: grid; flex: 1; min-height: 0; }
      .super-chart-mount { position: relative; flex: 1 1 auto; min-height: 0; }
      .super-chart { width: 100%; height: 100%; }
      .stub-chart-canvas, .stub-chart-overlay { display: block; width: 100%; height: 1px; }
      /* Radix's switches/radios are sized by Tailwind utilities that are
         not compiled here; without a box they would be unclickable for
         reasons that have nothing to do with what is under test. */
      button[role="switch"], button[role="radio"] { display: inline-block; min-width: 18px; min-height: 18px; }
    </style>
  </head>
  <body>
    <script>
      // A few CJS dependencies bundled from node_modules read
      // process.env directly; define handles the literal
      // process.env.NODE_ENV, this covers anything reading the object.
      window.process = window.process || { env: { NODE_ENV: "development" } };
    </script>
    <div id="root"></div>
    <script type="module" src="./harness.js"></script>
  </body>
</html>
`;

await Bun.write(join(outDir, "index.html"), html);
await Bun.write(
  join(outDir, "build-info.json"),
  `${JSON.stringify({ sourceRoot, outDir, builtAt: new Date().toISOString() }, null, 2)}\n`,
);

console.log(`harness built from ${sourceRoot}`);
console.log(`  -> ${join(outDir, "harness.js")}`);
