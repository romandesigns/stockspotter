# Build context is the repo root — this app needs the whole Bun workspace
# so @stockspotter/shared-types resolves via `workspace:*`.
# (see ../../docker-compose.yml: build.context: .)

FROM oven/bun:1-slim AS build
WORKDIR /repo

# Install first, from just the manifests, so this layer caches across
# builds that only touch app source.
COPY package.json bun.lock ./
COPY apps/client/package.json apps/client/package.json
COPY apps/mobile/package.json apps/mobile/package.json
COPY packages/shared-types/package.json packages/shared-types/package.json
# patches/ is part of the manifest set, not app source: root package.json
# declares `patchedDependencies`, and `bun install --frozen-lockfile`
# resolves those paths during install, so a missing patches/ fails the
# build outright with "Couldn't find patch file" rather than degrading.
# Found the hard way on 2026-09-20 -- the advisory remediation added the
# first patched dependency and this image was the only thing in the repo
# that installs from a partial copy of the tree, so nothing else noticed.
# CI runs `vite build`, never `docker build`, so it stayed green.
# Keep this beside the manifests: moving it below invalidates the cached
# install layer on every source change.
COPY patches patches
RUN bun install --frozen-lockfile

COPY apps/client apps/client
COPY packages/shared-types packages/shared-types
RUN bun --cwd=apps/client run test && bun --cwd=apps/client run build

# Runtime — matches the wavystack fleet convention: Bun-based, port 3000
# internal, no build tooling shipped.
FROM oven/bun:1-slim
WORKDIR /app
COPY --from=build /repo/apps/client/dist ./dist
COPY apps/client/server.ts ./server.ts

ENV PORT=3000
EXPOSE 3000
CMD ["bun", "run", "server.ts"]
