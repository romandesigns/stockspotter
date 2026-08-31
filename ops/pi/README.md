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

`ws` is **not** routed through caddy (it's a plain WebSocket, no TLS
termination of its own) — it's reached directly on its published port
(`8787`) over the tailnet, not via `stockspotter.wavystack`. `apps/client`
pointing at the deployed backend means its `useRealtimeFeed` WS URL needs
to target the Pi's tailnet address on 8787, not `localhost` — not yet done
on the client side, since nothing there is configurable per-environment yet.

## Checking it worked

```sh
docker compose -p stockspotter ps
curl -k https://stockspotter.wavystack   # web frontend, from inside the tailnet
docker compose -p stockspotter logs -f ws   # watch it connect to Alpaca live
```
