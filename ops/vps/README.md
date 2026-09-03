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

## Backend secrets (Alpaca/FMP)

Same discipline as the Pi: `apps/client` needs no server-side secrets
(static build). `crates/ws-server` does -- create `/opt/apps/stockspotter/.env`
**on the VPS** (never committed, never copied from the dev machine's own
gitignored `.env` -- a fresh file with the same real `ALPACA_*`/`FMP_API_KEY`
variable names), referenced via `ops/vps/docker-compose.yml`'s
`env_file: ../../.env`. `deploy.sh`'s `git reset --hard` never touches it
since it's untracked (and lives at the repo root, outside `ops/vps/`
entirely, so a stray `git clean` inside that subdirectory couldn't touch
it either). Minimum required: `ALPACA_API_KEY`, `ALPACA_API_SECRET`,
`ALPACA_FEED`, `ALPACA_MARKET_WS`, `ALPACA_DATA_BASE`, `ALPACA_TRADING_BASE`.
`FMP_API_KEY` optional (float lookups fail closed without it, same as
dev/Pi). `QUALIFY_SERVICE_URL` optional too -- the Python qualitative
layer has no deploy here either yet, same known gap as the Pi.

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
curl https://stockspotter.wavystyle.io/api/markets/today   # HTTP backfill
docker compose -p stockspotter-vps -f ops/vps/docker-compose.yml logs -f ws   # watch it connect to Alpaca live
```

## Other real services already on this box

`scout`/`stockhunter` (a separate project, Docker Compose at
`/opt/apps/scout/compose.yaml`) was stopped (not removed) to free
resources -- `docker compose -p scout up -d` from that directory brings
it back if ever needed. A Convex backend/dashboard/Postgres stack
(`~/convex/docker-compose.yml`) and an older, separately-managed
`scout-ntfy` container are untouched and still running -- nothing here
should ever interact with either.
