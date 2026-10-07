"""Post-deployment probes, run inside the private qualify container."""
import asyncio
import json
import os
import urllib.error
import urllib.request

import websockets


async def check_websocket(token):
    async with websockets.connect("ws://ws:8787", open_timeout=10) as socket:
        await socket.send(json.dumps({
            "type": "hello", "protocolVersion": 1, "client": "web", "token": token,
        }))
        response = json.loads(await asyncio.wait_for(socket.recv(), timeout=10))
        assert response["type"] == "welcome", "WebSocket authentication failed"


def main():
    token = os.environ["STOCKSPOTTER_API_TOKEN"]
    assert len(token) >= 32, "Configure a strong API token before deployment"
    try:
        urllib.request.urlopen("http://ws:8788/health", timeout=10)
    except urllib.error.HTTPError as error:
        assert error.code == 401, "Unexpected unauthenticated HTTP response"
    else:
        raise AssertionError("HTTP endpoint accepted an unauthenticated request")
    request = urllib.request.Request(
        "http://ws:8788/health", headers={"Authorization": "Bearer " + token},
    )
    assert urllib.request.urlopen(request, timeout=10).status == 200
    assert urllib.request.urlopen("http://localhost:8000/health", timeout=10).status == 200
    assert urllib.request.urlopen("http://web:3000/", timeout=10).status == 200
    asyncio.run(check_websocket(token))
    print("Authenticated HTTP/WS, qualitative service, and web probes passed")


if __name__ == "__main__":
    main()
