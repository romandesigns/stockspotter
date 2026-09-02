# Pi deploy setup (one-time)

Run once on the Pi (`ssh stockspotter-pi`), after `origin/master` has at
least one commit — the repo is already cloned at
`/home/pi/stacks/stockspotter` via the read-only `github-stockspotter`
deploy key.

```sh
cd /home/pi/stacks/stockspotter
git pull
chmod +x deploy.sh

sudo cp ops/pi/stockspotter-deploy.service ops/pi/stockspotter-deploy.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now stockspotter-deploy.timer

# First run happens within 30s (OnBootSec) or immediately if you trigger it:
sudo systemctl start stockspotter-deploy.service
journalctl -u stockspotter-deploy.service -f
```

`docker compose` needs `proxy` (the wavystack fleet's shared network) to
already exist — it does, since caddy-docker-proxy and the other apps are
already running on it.

## Backend secrets (Alpaca)

`apps/client` needs no server-side secrets (static build). The Rust WS
backend (`crates/ws-server`, `docker-compose.yml`'s `ws` service) does need
real credentials — create `/home/pi/stacks/stockspotter/.env` **on the Pi**
(never committed, same `ALPACA_*`/`FMP_API_KEY` variable names as the
dev-machine `.env`) before the first `docker compose up`; it's referenced via
that service's `env_file: .env`. `deploy.sh`'s `git reset --hard` never
touches this file since it's untracked. Minimum required: `ALPACA_API_KEY`,
`ALPACA_API_SECRET`, `ALPACA_FEED`, `ALPACA_MARKET_WS`, `ALPACA_DATA_BASE`,
`ALPACA_TRADING_BASE`. `FMP_API_KEY` is optional (float lookups fail closed
without it, same as dev). `QUALIFY_SERVICE_URL` is also optional — the
Python qualitative layer (`python/`) has no Pi deployment yet, so catalyst
lookups will log a harmless "unreachable" warning and the Catalysts panel
just won't populate until that's set up too.

`ws` **is** routed through caddy now (`ws.stockspotter.wavystack` /
`api.stockspotter.wavystack`, `docker-compose.yml`'s `caddy_0`/`caddy_1`
labels — caddy-docker-proxy's real mechanism for multiple site blocks off
one container), on top of its own published `8787`/`8788` ports (kept for
direct-tailnet/debug access, not required for the deployed site anymore).
This isn't just tidiness: a bare `ws://<tailnet-ip>:8787` URL would still
get blocked by any browser as mixed content once `stockspotter.wavystack`
loads over `https://` — proxying through the same Caddy instance, same
`tls=internal` cert, same scheme family, is what actually makes it
reachable from the deployed site. `apps/client/src/lib/config.ts` is the
client-side half: it targets these two hostnames automatically whenever
it isn't running against `localhost`, no build-time env var required.

## Checking it worked

```sh
docker compose -p stockspotter ps
curl -k https://stockspotter.wavystack       # web frontend, from inside the tailnet
curl -k https://ws.stockspotter.wavystack    # realtime WS backend (upgrade required for a real handshake)
curl -k https://api.stockspotter.wavystack/markets/today   # HTTP backfill endpoints
docker compose -p stockspotter logs -f ws    # watch it connect to Alpaca live
```
