// Static file server for the built client, run inside the production
// container (see Dockerfile). Bun-based to match the wavystack convention
// of every app being a single `oven/bun:1-slim` container on port 3000,
// routed by the shared caddy-docker-proxy.
import { join } from "node:path";

const dist = join(import.meta.dir, "dist");
const port = Number(process.env.PORT ?? 3000);

Bun.serve({
  port,
  async fetch(req) {
    const url = new URL(req.url);
    const path = url.pathname === "/" ? "/index.html" : url.pathname;

    let file = Bun.file(join(dist, path));
    let isIndexShell = path === "/index.html";
    if (!(await file.exists())) {
      // Client-side router owns unknown paths — fall back to the SPA
      // shell. isIndexShell has to be re-set here too, not just above:
      // a route like /some-client-path never matches path === "/index.html"
      // but still ends up serving that exact file, so it needs the same
      // never-cache treatment, not the hashed-assets long-cache one.
      file = Bun.file(join(dist, "index.html"));
      isIndexShell = true;
    }

    // The one real cache bug this deploy surfaced: index.html was served
    // with no cache-control at all, so a browser's own default heuristics
    // could keep serving a stale copy (still referencing the OLD hashed
    // JS bundle) after a redeploy -- a hard refresh was needed to see any
    // fix at all, confirmed live (a fresh, cache-less browser session got
    // the new code immediately; a real browser tab open from before the
    // deploy didn't). index.html's only job is to point at the CURRENT
    // content-hashed asset filenames, so it must never be cached. The
    // hashed assets themselves (isIndexShell false below) are the
    // opposite case -- their filename changes whenever content does, so a
    // long, immutable cache is correct and safe for those.
    const headers = isIndexShell
      ? { "Cache-Control": "no-cache, no-store, must-revalidate" }
      : { "Cache-Control": "public, max-age=31536000, immutable" };

    return new Response(file, { headers });
  },
});

console.log(`stockspotter web listening on :${port}`);
