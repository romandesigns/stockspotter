"""News catalyst detection — architecture doc section 4.4's "News catalyst
detection/confirmation (earnings, FDA, etc.)" bullet, taken literally
rather than expanded into something bigger.

Scope, deliberately: this is keyword-based catalyst *tagging*, not
sentiment analysis or any kind of ML scoring. The doc's other 4.4 bullet
("higher-level qualitative trend/sentiment confirmation") is real future
scope but a much larger, separate undertaking (a real NLP/sentiment model,
with its own accuracy questions) — not something to fake with a keyword
list dressed up as "sentiment". This module only ever claims to detect
catalyst *categories* from headline/summary text.

News itself comes from Alpaca's own `/v1beta1/news` REST endpoint — no
separate news provider needed, same account already in use everywhere
else in this project.
"""

from typing import Any

import httpx

from .config import AlpacaConfig

# Keyword -> catalyst category. Deliberately simple substring matching on
# lowercased text, not real NLP — a headline either mentions one of these
# phrases or it doesn't. False negatives (a catalyst worded unusually)
# are expected and fine; the point is to flag the *common* cases cheaply,
# not to be exhaustive.
CATALYST_KEYWORDS: dict[str, list[str]] = {
    "earnings": [
        "earnings", "eps", "quarterly results", "quarterly report",
        "revenue guidance", "q1 results", "q2 results", "q3 results", "q4 results",
    ],
    "fda": [
        "fda", "clinical trial", "phase 1", "phase 2", "phase 3",
        "phase i", "phase ii", "phase iii", "clearance", "fda approval",
    ],
    "merger_acquisition": [
        "merger", "acquisition", "to acquire", "to be acquired",
        "buyout", "takeover", "definitive agreement to merge",
    ],
    "offering_dilution": [
        "private placement", "public offering", "registered direct",
        "dilution", "warrant", "convertible note", "shelf offering",
    ],
    "halt_resumption": [
        "trading halt", "halted", "resumes trading", "halt lifted",
    ],
    "analyst_action": [
        "upgrade", "downgrade", "price target", "initiates coverage",
        "reiterates rating",
    ],
    "partnership_contract": [
        "partnership", "collaboration agreement", "contract award",
        "signs agreement", "strategic alliance",
    ],
}


def tag_catalysts(text: str) -> list[str]:
    """Returns every catalyst category whose keywords appear in `text`
    (case-insensitive substring match). Order matches CATALYST_KEYWORDS'
    definition order, not alphabetical or relevance — callers that need a
    stable display order should sort themselves.
    """
    lowered = text.lower()
    return [
        category
        for category, keywords in CATALYST_KEYWORDS.items()
        if any(keyword in lowered for keyword in keywords)
    ]


async def fetch_recent_news(cfg: AlpacaConfig, symbol: str, limit: int = 10) -> list[dict[str, Any]]:
    """Most recent news items for one symbol, newest first (Alpaca's own
    default ordering). Raises on a request/HTTP failure — callers decide
    how to handle that (the qualify endpoint treats a per-symbol failure
    as "no news" rather than failing the whole batch, see main.py).
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            f"{cfg.data_base}/v1beta1/news",
            headers={
                "APCA-API-KEY-ID": cfg.api_key,
                "APCA-API-SECRET-KEY": cfg.api_secret,
            },
            params={"symbols": symbol, "limit": limit},
        )
        resp.raise_for_status()
        return resp.json().get("news", [])
