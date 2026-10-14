#!/usr/bin/env python3
"""A fake Alpaca endpoint for the isolated staging rehearsal. Never production.

Answers only the read-only paths `ops/release_preflight.py` and the
auto-trader's status probe actually call, from the standard library alone, with
fault switches driven by its own environment:

    STAGING_BROKER_POSITIONS=2      /v2/positions returns two positions
    STAGING_BROKER_OPEN_ORDERS=1    /v2/orders?status=open returns one order
    STAGING_BROKER_BLOCKED=1        the account reports trading_blocked
    STAGING_BROKER_MARKET_OPEN=1    the clock reports the market open
    STAGING_BROKER_UNREACHABLE=1    every request fails with 503

This exists so a staging stack can be exercised without reaching the real
broker, and so the controller's refusal paths (exposure not provably zero) can
be rehearsed deliberately. Two honest limitations:

* It serves plain HTTP. `ops/release_preflight.py` sets
  `paper_endpoint_configured` only when `ALPACA_TRADING_BASE` is exactly
  `https://paper-api.alpaca.markets`, so against this broker that field is
  `false` and the controller's `exposure_zero()` is correspondingly `false`.
  A staging stack therefore rehearses the exposure gate's *refusal* direction
  only; the passing direction can only be exercised on the real host with real
  zero exposure. This is deliberate -- the alternative would be editing the
  preserved preflight to accept a fake endpoint, which would weaken the real
  gate to make a test convenient.
* It is reachable only inside the staging compose project's internal network.
  It publishes no port and must never be given one.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import ssl
import sys
from urllib.parse import urlparse

PORT = int(os.environ.get("STAGING_BROKER_PORT", "9999"))


def _count(name):
    try:
        return max(0, int(os.environ.get(name, "0")))
    except ValueError:
        return 0


def _flag(name):
    return os.environ.get(name, "") not in ("", "0", "false", "False", "off")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, payload, status=200):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802 - BaseHTTPRequestHandler's own naming
        if _flag("STAGING_BROKER_UNREACHABLE"):
            self._send({"message": "staging broker is injecting a failure"}, status=503)
            return
        path = urlparse(self.path).path
        if path == "/v2/account":
            self._send({
                "status": "ACTIVE",
                "trading_blocked": _flag("STAGING_BROKER_BLOCKED"),
                "account_blocked": False,
                "id": "staging-account-not-a-real-account",
            })
        elif path == "/v2/clock":
            self._send({"is_open": _flag("STAGING_BROKER_MARKET_OPEN"),
                        "next_open": "2026-10-15T09:30:00-04:00"})
        elif path == "/v2/positions":
            self._send([{"symbol": f"STG{index}", "qty": "1"}
                        for index in range(_count("STAGING_BROKER_POSITIONS"))])
        elif path == "/v2/orders":
            self._send([{"id": f"staging-order-{index}", "status": "new"}
                        for index in range(_count("STAGING_BROKER_OPEN_ORDERS"))])
        else:
            self._send({"message": "not implemented by the staging broker"}, status=404)

    def log_message(self, format, *args):  # noqa: A002 - signature is fixed
        # One line per request on stderr, no credentials: the handler never
        # reads the APCA-* headers at all.
        sys.stderr.write(f"staging-broker {self.command} {self.path}\n")


def main():
    server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    cert = os.environ.get("STAGING_BROKER_TLS_CERT")
    key = os.environ.get("STAGING_BROKER_TLS_KEY")
    if bool(cert) != bool(key):
        raise SystemExit("staging broker needs both TLS fixture files")
    if cert:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
    sys.stderr.write(f"staging-broker listening on {PORT} (internal network only, tls={bool(cert)})\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
