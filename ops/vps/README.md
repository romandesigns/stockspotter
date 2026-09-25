# VPS deploy setup (srv1170872, one-time)

Real, second deploy target alongside the Pi -- reached over Roman's own
tailnet (`100.88.87.41`), running a genuine public-facing Hostinger VPS
with its own domain (`stockspotter.wavystyle.io`) rather than the Pi's
tailnet-only, self-signed-cert setup. Different reverse proxy too:
native systemd Caddy (`/etc/caddy/Caddyfile`), not caddy-docker-proxy --
this machine already runs other real projects (scout, a Convex stack)
under that same native Caddy instance, so stockspotter is one more site
block in the same Caddyfile, not a separate proxy mechanism.

Repo already cloned at `/opt/apps/stockspotter` via a dedicated, read-only
deploy key (`stockspotter-vps-deploy`, added to the GitHub repo's deploy
keys, SSH config alias `github-stockspotter` in `~/.ssh/config` on the VPS
itself -- separate key from the Pi's own `wavystack-pi` deploy key, so
either can be revoked independently).

## One-time setup

```sh
# On the VPS, as wavystack:
cd /opt/apps/stockspotter
chmod +x ops/vps/deploy.sh

sudo cp ops/vps/stockspotter-deploy.service ops/vps/stockspotter-deploy.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now stockspotter-deploy.timer

# First run happens within 30s (OnBootSec) or immediately if triggered:
sudo systemctl start stockspotter-deploy.service
journalctl -u stockspotter-deploy.service -f
```

## Deploy timer state (2026-09-25)

The timer has been **stopped since 2026-09-23 12:47Z** (`inactive`, still
`enabled`, so it starts again on the next reboot via `OnBootSec=30s`).
Nobody has re-enabled it on purpose: backend and web lines are being
consolidated on `integration/stockspotter-20260925`, and nothing should
deploy until that branch is reviewed.

It is safe for the timer to come back on reboot, and this is why, not a
hope: `deploy.sh` never advances a `release/*` checkout. It fetches only to
prove HEAD exists on origin, then deploys whatever HEAD is checked out,
and only when HEAD differs from `ops/vps/.deployed-commit`. Both are
`7e36586` today, so every timer run is a no-op -- even if someone pushes
to `origin/release/operating-run-20260907`. A deploy happens only when an
operator moves `/opt/apps/stockspotter` to a new commit by hand.

What that no-op does NOT protect against: moving the production checkout
to any commit on the old release branch. A full deploy from
`release/operating-run-20260907` rebuilds `web` from that branch's
`apps/client` and silently reverts the live web client (7cb2ba0, deployed
out-of-band by `deploy-chart-web.sh`). The next full deploy must come from
a branch that contains the live web lineage -- the integration branch.

Stopping or disabling the timer needs `sudo` (the `wavystack` account has
no passwordless sudo); do it from an interactive session if wanted:
`sudo systemctl disable --now stockspotter-deploy.timer`.

## Backend secrets (Alpaca/FMP)

Same discipline as the Pi: `apps/client` needs no server-side secrets
(static build). `crates/ws-server` does -- create `/opt/apps/stockspotter/.env`
**on the VPS** (never committed, never copied from the dev machine's own
gitignored `.env` -- a fresh file with the same real `ALPACA_*`/`FMP_API_KEY`
variable names), referenced via `ops/vps/docker-compose.yml`'s
`env_file: ../../.env`. Deployment now refuses tracked local changes and
only fast-forwards the checkout. Minimum required: `ALPACA_API_KEY`, `ALPACA_API_SECRET`,
`ALPACA_FEED`, `ALPACA_MARKET_WS`, `ALPACA_DATA_BASE`, `ALPACA_TRADING_BASE`.
Also set `STOCKSPOTTER_API_TOKEN` to a randomly generated private value
of at least 32 characters. Network listeners refuse to start without it.
Web, desktop, and mobile users enter this key at the new sign-in screen;
it stays in app memory for that session. Never place it in public build
variables or a URL. Release compatible clients before enabling the key
on an existing service. Deployment checks the key before replacing
containers and checks HTTP authentication, WebSocket authentication,
the qualitative service, and web delivery before recording success.

`FMP_API_KEY` optional (float lookups fail closed without it, same as
dev/Pi). The Python qualitative layer (`python/app`) is deployed here too
now, as its own `qualify` service (`ops/vps/docker-compose.yml`, built from
`python/Dockerfile`) -- no published port, `ws` reaches it by service name
via `QUALIFY_SERVICE_URL=http://qualify:8000`. It reads the same `.env`,
so no separate credential to create; the Catalysts panel populates as soon
as this stack is up.

## Auto-trader (dry-run paper-trading journal)

`crates/auto-trader` (`ops/vps/docker-compose.yml`'s `auto-trader` service)
reads the same `.env`: `STOCKSPOTTER_API_TOKEN` authenticates its feed,
and Alpaca market-data credentials let it reconcile missed completed
bars for open simulated positions after a restart or disconnect.
It reaches `ws` over the compose network
(`AUTO_TRADER_WS_URL=ws://ws:8787`, set inline in the compose file). Every
tunable (`AUTO_TRADER_POSITION_SIZE_USD`, `AUTO_TRADER_MAX_CONCURRENT_POSITIONS`,
`AUTO_TRADER_JOURNAL_PATH`) has a safe hardcoded default; add overrides to
this box's `.env` only if you actually want to tune them. It places no
real orders in this version -- watch its simulated entries/exits/skips via
`docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml logs -f auto-trader`
or by tailing `data/auto_trader_journal.jsonl` directly.

## Caddy (real HTTPS, not self-signed)

Append `ops/vps/Caddyfile.snippet`'s contents to `/etc/caddy/Caddyfile`,
then reload:

```sh
sudo tee -a /etc/caddy/Caddyfile < ops/vps/Caddyfile.snippet
sudo systemctl reload caddy
```

This gets a real, automatic Let's Encrypt certificate the moment DNS
resolves and Caddy reloads -- no `tls internal` directive, no
manually-trusting-a-CA step the way the Pi's `stockspotter.wavystack`
needed. `stockspotter.wavystyle.io` DNS (A record -> this VPS's public
IP) already confirmed live before this was written.

Single domain, path-based routing (`/ws`, `/api/*`, everything else) --
not the Pi's three-subdomain split. See the snippet's own comments for
why `handle_path` is what makes that work cleanly against ws-server's
own root-path routing on each of its two ports.

## Checking it worked

```sh
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml ps
curl https://stockspotter.wavystyle.io                    # web frontend -- real cert, no -k needed
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml exec -T qualify python /app/check_health.py
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml logs -f ws   # watch it connect to Alpaca live
```

The probes confirm service and authentication availability. During the
next trading session, separately check fresh market timestamps, wildcard
subscription acceptance, official LULD readings, reconnect recovery, and
mobile background notifications. The automatic probes do not send alerts.

## Other real services already on this box

`scout`/`stockhunter` (a separate project, Docker Compose at
`/opt/apps/scout/compose.yaml`) was stopped (not removed) to free
resources -- `docker compose -p scout up -d` from that directory brings
it back if ever needed. A Convex backend/dashboard/Postgres stack
(`~/convex/docker-compose.yml`) and an older, separately-managed
`scout-ntfy` container are untouched and still running -- nothing here
should ever interact with either.
