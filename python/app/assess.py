"""Chart Page AI assessment (2026-09-03, Roman's own ask) -- a brief,
Claude-generated read on a symbol's current momentum picture, shown
inside the same momentum-score card both frontends already render
(apps/mobile/src/components/MomentumScoreRow.tsx, the inline
MomentumScoreRow in apps/client/src/components/SuperChart.tsx).

Deliberately scoped like news.py's own catalyst tagging: this calls a
real external API (Anthropic's Messages API) with the server-side web
search tool enabled, so Claude can look up genuinely current context on
the symbol rather than only reasoning over the momentum numbers handed
to it -- but the summary itself stays short (a handful of bullet
points), matching Roman's explicit "brief, to the point" ask and this
project's own real cost-consciousness about API spend (a Claude call
costs real money and takes a few real seconds, unlike the free/cheap
Alpaca REST calls everywhere else in this codebase).

Caching (Roman's chosen policy): a simple in-memory dict keyed by
symbol, TTL-bounded -- re-requesting the same symbol soon after reuses
the cached answer instead of re-spending a real API call. Process-local,
not shared across replicas/restarts -- acceptable at this scale (one
`qualify` container, not a fleet), same reasoning `log.rs`'s flat-file
choice already documents elsewhere in this project for "modest scale,
revisit if it stops being modest."
"""

import time
from dataclasses import dataclass, field

import httpx

from .config import AnthropicConfig

CACHE_TTL_SECONDS = 10 * 60
MODEL = "claude-sonnet-5"
# Real, verified tuning (2026-09-03, live-tested against the real API
# before shipping) -- 220 truncated a genuine 4-bullet response mid-
# sentence; the model's own thinking + a web-search round trip already
# consumes real tokens before the visible answer even starts, so the
# visible-answer budget needs real headroom, not just "make it small
# because the output should be short." The system prompt is what
# actually enforces brevity; this cap exists to prevent runaway length,
# not to hit it as the normal case.
MAX_TOKENS = 500
# Bounds how many real web searches one assessment can trigger -- keeps
# both latency and cost predictable per request rather than letting a
# stubborn query spiral into many searches.
MAX_WEB_SEARCHES = 3

SYSTEM_PROMPT = (
    "You are a terse trading-desk assistant embedded in a live stock scanner. "
    "Given a symbol and its current momentum-scanner readings, use web search if "
    "it would meaningfully change the read (recent news, halts, offerings, "
    "earnings, etc.), then respond with EXACTLY 3-4 bullet points, one per line, "
    "each line starting with a hyphen. No preamble, no closing summary, no "
    "disclaimers about not being financial advice -- the trader reading this "
    "already knows that. Each bullet under ~15 words. Lead with the single most "
    "decision-relevant fact."
)


@dataclass
class _CacheEntry:
    summary: list[str]
    generated_at: str
    cached_at: float = field(default_factory=time.monotonic)


_cache: dict[str, _CacheEntry] = {}


def _cache_get(symbol: str) -> _CacheEntry | None:
    entry = _cache.get(symbol)
    if entry is None:
        return None
    if time.monotonic() - entry.cached_at > CACHE_TTL_SECONDS:
        del _cache[symbol]
        return None
    return entry


def _extract_text_blocks(content: list[dict]) -> list[str]:
    """Claude's response `content` is a list of typed blocks -- text,
    server_tool_use (the web search call itself), web_search_tool_result
    (the raw search results). Only `text` blocks are the actual answer;
    everything else is Claude's own intermediate tool-use trace, not
    part of what we show the trader.
    """
    return [block["text"] for block in content if block.get("type") == "text" and block.get("text")]


def _join_text_blocks(text_blocks: list[str]) -> str:
    """Real, verified behavior (2026-09-03): with web search enabled,
    Claude's answer arrives split across MULTIPLE `text` blocks -- a
    citation-annotated block interrupts the answer wherever it cites a
    search result, then a fresh `text` block continues the same
    sentence/bullet. The model places its own `\n` line breaks WITHIN
    these blocks at the right points, so concatenating with NO separator
    is usually correct -- joining with "\n" instead (the first version
    of this) injected a spurious extra line break at every citation
    boundary, corrupting bullets mid-sentence.

    One real exception, also confirmed live: after a citation
    interruption the model occasionally *resumes* by restating the point
    as a fresh bullet ("- ...") with no leading `\n` of its own, running
    it onto the end of the previous line instead of starting a new one.
    Any block that starts with a bullet marker forces a newline before
    it regardless of what preceded it -- targets exactly that failure
    mode without altering genuinely-continuous prose (a block that
    doesn't start with a marker joins exactly as the model wrote it).
    """
    out = ""
    for block in text_blocks:
        if block.lstrip()[:1] in ("-", "*", "•") and out and not out.endswith("\n"):
            out += "\n"
        out += block
    return out


def _parse_bullets(text_blocks: list[str]) -> list[str]:
    joined = _join_text_blocks(text_blocks).strip()
    if not joined:
        return []
    lines = [line.strip().lstrip("-*•").strip() for line in joined.splitlines()]
    bullets = [line for line in lines if line]
    return bullets if bullets else [joined]


async def get_assessment(cfg: AnthropicConfig, symbol: str, momentum_summary: str, force_refresh: bool = False) -> tuple[list[str], str]:
    """Returns (summary bullets, generated_at ISO timestamp). Serves a
    cached answer within CACHE_TTL_SECONDS unless `force_refresh` (the
    UI's manual "regenerate" affordance bypasses the cache on purpose).
    """
    if not force_refresh:
        cached = _cache_get(symbol)
        if cached is not None:
            return cached.summary, cached.generated_at

    user_message = f"Symbol: {symbol}\nCurrent momentum-scanner readings: {momentum_summary}"

    async with httpx.AsyncClient(timeout=30.0) as client:
        resp = await client.post(
            f"{cfg.api_base}/v1/messages",
            headers={
                "x-api-key": cfg.api_key,
                "anthropic-version": "2023-06-01",
                "content-type": "application/json",
            },
            json={
                "model": MODEL,
                "max_tokens": MAX_TOKENS,
                "system": SYSTEM_PROMPT,
                "messages": [{"role": "user", "content": user_message}],
                "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": MAX_WEB_SEARCHES}],
            },
        )
        resp.raise_for_status()
        data = resp.json()

    text_blocks = _extract_text_blocks(data.get("content", []))
    bullets = _parse_bullets(text_blocks)
    generated_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

    _cache[symbol] = _CacheEntry(summary=bullets, generated_at=generated_at)
    return bullets, generated_at
