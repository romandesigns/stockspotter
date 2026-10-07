"""Read-only deployment/account checks; run inside the existing qualify container."""
import json
import os
import urllib.request


def main():
    base = os.environ.get("ALPACA_TRADING_BASE", "").rstrip("/")
    result = {
        "paper_endpoint_configured": base == "https://paper-api.alpaca.markets",
        "api_token_present_and_long_enough": len(os.getenv("STOCKSPOTTER_API_TOKEN", "")) >= 32,
        "feed": os.getenv("ALPACA_FEED"),
        "execution_mode": os.getenv("AUTO_TRADER_EXECUTION_MODE", "journal"),
        "universe_mode": os.getenv("IGNITION_UNIVERSE_MODE", "off"),
        "discovery_recording_configured": bool(os.getenv("DISCOVERY_AUDIT_DIR")),
    }
    if result["paper_endpoint_configured"]:
        def get(path):
            request = urllib.request.Request(base + path, headers={
                "APCA-API-KEY-ID": os.environ["ALPACA_API_KEY"],
                "APCA-API-SECRET-KEY": os.environ["ALPACA_API_SECRET"],
            })
            with urllib.request.urlopen(request, timeout=15) as response:
                return json.load(response)
        account, clock = get("/v2/account"), get("/v2/clock")
        result.update({"account_active": account["status"] == "ACTIVE",
                       "trading_blocked": account["trading_blocked"] or account["account_blocked"],
                       "market_open": clock["is_open"], "next_open": clock["next_open"],
                       "positions": len(get("/v2/positions")),
                       "open_orders": len(get("/v2/orders?status=open&limit=500"))})
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
