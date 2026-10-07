"""COUNT-ONLY trade-condition characterization (Step 4B-main.2 §3-5).

Subscribes to live SIP trades for all symbols and keeps, in memory, ONLY:

    (tape, exact condition list as received) -> count

plus, per tape, the count of trades whose condition list is empty and whose
condition field is absent. Symbol, price, size, timestamp, exchange, trade id
and every other field are read by the JSON parser and immediately discarded;
nothing linkable to a symbol, a time or a price is retained or written.

Control messages (auth/subscription/error) are recorded by their `T` and `msg`
/ `code` fields only -- they never carry market data.

Exception text is never stored (it could echo payload); only its type name.

Usage (keys from the environment, never printed):
    APCA_KEY=... APCA_SECRET=... python cond_count.py <label> <duration_s> <out.json>
"""
import asyncio, collections, datetime, json, os, sys, time

import websockets

URL = "wss://stream.data.alpaca.markets/v2/sip"


def now_iso():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


async def run(label, duration, out):
    counts = collections.Counter()      # (tape, json(conditions)) -> n
    empty = collections.Counter()       # tape -> n   (c == [])
    absent = collections.Counter()      # tape -> n   (no "c" field)
    control = []
    result = {"schema": "condition-characterization-v1", "label": label, "feed": "sip", "url": URL,
              "startedAt": now_iso(), "plannedSeconds": duration, "complete": False}
    try:
        async with websockets.connect(URL, max_size=None, ping_interval=20) as ws:
            async def expect():
                for m in json.loads(await asyncio.wait_for(ws.recv(), 15)):
                    control.append({k: m.get(k) for k in ("T", "msg", "code") if k in m})
            await expect()  # connected
            await ws.send(json.dumps({"action": "auth", "key": os.environ["APCA_KEY"], "secret": os.environ["APCA_SECRET"]}))
            await expect()
            if not any(c.get("msg") == "authenticated" for c in control):
                raise RuntimeError("not authenticated")
            await ws.send(json.dumps({"action": "subscribe", "trades": ["*"]}))
            start = time.monotonic()
            result["countingFrom"] = now_iso()
            while time.monotonic() - start < duration:
                try:
                    raw = await asyncio.wait_for(ws.recv(), max(0.1, duration - (time.monotonic() - start)))
                except asyncio.TimeoutError:
                    break
                for m in json.loads(raw):
                    kind = m.get("T")
                    if kind == "t":
                        tape = m.get("z", "<none>")
                        if "c" not in m or m["c"] is None:
                            absent[tape] += 1
                            key = None
                        else:
                            key = json.dumps(m["c"], ensure_ascii=False)
                            if not m["c"]:
                                empty[tape] += 1
                        counts[(tape, key)] += 1
                    elif kind in ("success", "error", "subscription"):
                        control.append({k: m.get(k) for k in ("T", "msg", "code") if k in m})
                    # anything else: dropped unread
                    del m
            result["complete"] = True
    except Exception as e:  # noqa: BLE001 -- type only, never text
        result["error"] = type(e).__name__
    result["endedAt"] = now_iso()
    tapes = sorted({t for t, _ in counts})
    result["control"] = control
    result["perTape"] = {
        t: {
            "totalTrades": sum(n for (tt, _), n in counts.items() if tt == t),
            "emptyConditionList": empty[t],
            "absentConditionField": absent[t],
            "tradesContainingSpace": sum(n for (tt, k), n in counts.items() if tt == t and k and " " in json.loads(k)),
            "tradesContainingAt": sum(n for (tt, k), n in counts.items() if tt == t and k and "@" in json.loads(k)),
            "conditionSets": [
                {"conditions": (json.loads(k) if k is not None else None), "count": n}
                for (tt, k), n in sorted(counts.items(), key=lambda kv: (-kv[1], str(kv[0])))
                if tt == t
            ],
        }
        for t in tapes
    }
    with open(out, "w", encoding="utf-8") as f:
        json.dump(result, f, indent=1, ensure_ascii=False)
    print(json.dumps({"label": label, "complete": result["complete"], "error": result.get("error"),
                      "control": control, "totals": {t: v["totalTrades"] for t, v in result["perTape"].items()}}))


if __name__ == "__main__":
    asyncio.run(run(sys.argv[1], int(sys.argv[2]), sys.argv[3]))
