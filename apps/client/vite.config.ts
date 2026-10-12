import path from "node:path";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Build identity describes this embedded frontend, not the remote server.
let sourceCommit = "unknown";
let sourceTree = "unknown";
try {
  const root = path.resolve(import.meta.dirname, "../..");
  const git = (...args: string[]) => execFileSync("git", ["-c", `safe.directory=${root.replaceAll("\\", "/")}`, "-C", root, ...args], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  const head = git("rev-parse", "HEAD");
  if (/^[0-9a-f]{40}$/.test(head)) {
    sourceCommit = head;
    // Only the exact untracked continuous-build signing override is exempt.
    // A tracked override or altered version/identity/config must count as dirty.
    const override = path.resolve(import.meta.dirname, "ci-build.json");
    const relativeOverride = path.relative(root, override).replaceAll("\\", "/");
    const generatedOverride = existsSync(override)
      && !git("ls-files", "--", relativeOverride)
      && JSON.stringify(JSON.parse(readFileSync(override, "utf8").replace(/^\uFEFF/, ""))) === '{"bundle":{"createUpdaterArtifacts":false}}';
    sourceTree = git("status", "--porcelain", "--untracked-files=all", "--", ".", ...(generatedOverride ? [`:(exclude)${relativeOverride}`] : [])) ? "dirty" : "clean";
  }
} catch { /* A source archive without Git must remain explicitly unknown. */ }

// https://vite.dev/config/
export default defineConfig({
  define: {
    __STOCKSPOTTER_BUILD__: JSON.stringify({ sourceCommit, sourceTree }),
  },
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },
});
