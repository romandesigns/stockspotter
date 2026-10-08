# Stockspotter

A day-trading scanner: a Rust workspace that ingests market data and detects
candidate moves, a React web client that is also packaged as a Tauri desktop
app, an Expo mobile app, and a small Python service.

`master` is the canonical source. Merging to `master` runs validation only; it
does not publish, build for production or deploy.

## Where to start

| Document | What it covers |
|---|---|
| [docs/MASTER-RELEASE-FLOW.md](docs/MASTER-RELEASE-FLOW.md) | How changes reach `master`, the required checks, and how desktop, mobile and server artifacts are produced |
| [docs/consolidation-index-2026-10-08.md](docs/consolidation-index-2026-10-08.md) | Status of each inventoried branch and research finding: what landed, what is held and why, and which earlier findings were withdrawn |
| [docs/trading-scanner-architecture-part-1.md](docs/trading-scanner-architecture-part-1.md) | Architecture, continued in parts 2 and 3 |
| [ops/vps/README.md](ops/vps/README.md) | Server deployment |

## Layout

| Path | Contents |
|---|---|
| `crates/` | Rust workspace: market data, detectors, scoring, the WebSocket/HTTP server, the paper auto-trader |
| `apps/client/` | Web client and Tauri desktop shell |
| `apps/mobile/` | Expo mobile app |
| `packages/shared-types/` | Types and rules shared by the clients |
| `python/` | Catalyst tagging and assessment service |
| `ops/` | Deployment, CI gates and observation tooling |
| `docs/`, `research-reports/` | Design documents, preregistrations and reports |

Nothing in this repository is investment advice, and no document here claims
that a strategy is profitable.
