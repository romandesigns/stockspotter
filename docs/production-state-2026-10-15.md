# Production state, 2026-10-15

This records what the VPS (`/opt/apps/stockspotter`, compose project
`stockspotter-vps`) was actually running on 2026-10-15. It was read with
`docker inspect` on the running containers. Before this record, git could not
tell you what was live: the checkout, the deploy marker and the compose file
each describe something different.

## Checkout and marker

- Branch `release/mover-provenance-20261011` @ `2472e0f94186d59409e8dcbf0b0bdf168362bcd0`
- `ops/vps/.deployed-commit` = `2472e0f94186d59409e8dcbf0b0bdf168362bcd0`
- `stockspotter-deploy.timer` active. Each run is a no-op because HEAD equals
  the marker.

## Running services

Two services were replaced by hand. Each was started with `docker compose -f
ops/vps/docker-compose.yml -f <override>`, where the override lives outside
the repository and only changes the image (and, for `ws`, the environment).

| Service | Image ID | Source commit | Started (UTC) | Override |
|---|---|---|---|---|
| `ws` | `sha256:ad4b33bb1234…` (`stockspotter-iac-ws:c28fb19`) | `c28fb194849a` (`release/iac-extreme-disabled-20261013`, PR #59) | 2026-10-13 16:32 | `/home/wavystack/stockspotter-iac-coverage-20261013/release-evidence/active-compose.json` |
| `web` | `sha256:ab3395c4146d…` | `27e3bc8f279b` (master, PR #67 merge) | 2026-10-14 22:59 | `/home/wavystack/stockspotter-release-evidence/master-web-20261014-27e3bc8/active-web-compose.json` |
| `qualify` | `sha256:5a7a9f8457d0…` | not labelled (built 2026-10-06, `1a57989` era) | 2026-10-06 11:29 | none |
| `auto-trader` | `sha256:5d96af96716c…` | not labelled (built 2026-10-06) | 2026-10-06 11:29 | none |
| `discovery-review` | `sha256:90d5563ba177…` | not labelled (built 2026-10-06) | 2026-10-06 11:29 | none |

What the overrides contain, in full and with no secrets:

```json
// active-compose.json (ws; its web entry was superseded on 2026-10-14)
{"services":{"web":{"image":"stockspotter-iac-web:c28fb19-active"},
 "ws":{"image":"stockspotter-iac-ws:c28fb19",
       "environment":{"IAC_ENABLED":"true","EXTREME_MOMENTUM_ENABLED":"true"}}}}

// active-web-compose.json (web)
{"services":{"web":{"image":"sha256:ab3395c4146d22cc54666bb661a7d3b0398c658d31ba2a18b3dd38cb37b9358a"}}}
```

**IAC and Extreme Momentum run in production.** Both flags default to off in
code, but the `ws` override turns them on. Their output goes only to the
authenticated research channel. Clients show it in an "Experimental" panel
that is off by default for each user, and it never triggers push
notifications.

## Rollback tags

These tags were added on 2026-10-15 so that a rebuild cannot overwrite the
running images. Every tag points at the exact image ID above.

```
stockspotter-prod-20261015:ws-c28fb19
stockspotter-prod-20261015:web-27e3bc8
stockspotter-prod-20261015:qualify-20261006
stockspotter-prod-20261015:auto-trader-20261006
stockspotter-prod-20261015:discovery-review-20261006
```

## The hazard this creates

`deploy.sh` rebuilds all five services from the checkout and uses only
`ops/vps/docker-compose.yml`, with no override. If the checkout or marker
changes, the next timer run rebuilds from that commit and **drops both
overrides**. Concretely, starting from today's checkout:
- `ws` would go from `c28fb19` back to `2472e0f`;
- IAC and Extreme Momentum would stop;
- `web` would go from `27e3bc8` back to `2472e0f`.

Until the transactional deploy (PR #69) replaces the timer, do not move the
checkout, edit the marker or run `deploy.sh` without first deciding what
happens to the overrides. To restore exactly this state, run `docker compose`
with the same two `-f` files, or retag from `stockspotter-prod-20261015:*`.

## Housekeeping done the same day

- **Docker build cache:** pruned entries unused for more than 48 h, freeing
  135 GB. The root filesystem went from 97% to 64% used (141 GB free). No
  images, containers or volumes were removed.
- **Stopped** `stockspotter-stage-20261009-web-1`, a staging candidate that
  had been left running for 5 days.
- **Stopped the buildx builder** `stockspotter-ui-guarded-20261012`, a leftover
  that was still running. It can be restarted with `docker buildx inspect --bootstrap`.
