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

`apps/client` needs no server-side secrets today (static build). Once the
Rust WS backend (`crates/fast-funnel`, not built yet) gets its own
`docker-compose.yml` service, its Alpaca credentials go in
`/home/pi/stacks/stockspotter/.env` **on the Pi** (never committed — same
`ALPACA_*` variable names as the dev-machine `.env`), referenced from that
service's `env_file:`. `deploy.sh`'s `git reset --hard` never touches this
file since it's untracked.

## Checking it worked

```sh
docker compose -p stockspotter ps
curl -k https://stockspotter.wavystack   # from inside the tailnet
```
