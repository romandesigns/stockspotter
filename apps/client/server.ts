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
    if (!(await file.exists())) {
      // Client-side router owns unknown paths — fall back to the SPA shell.
      file = Bun.file(join(dist, "index.html"));
    }

    return new Response(file);
  },
});

console.log(`stockspotter web listening on :${port}`);
