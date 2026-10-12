import path from "node:path";
import { execFileSync } from "node:child_process";
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
    // The continuous desktop workflow writes this exact non-source override.
    // All other untracked inputs count as dirty; ignored build outputs do not.
    sourceTree = git("status", "--porcelain", "--untracked-files=all", "--", ".", ":(exclude)apps/client/ci-build.json") ? "dirty" : "clean";
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
