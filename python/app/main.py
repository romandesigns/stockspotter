"""stockspotter's qualitative scoring layer — architecture doc section
4.4. Receives only the small shortlist the Rust fast funnel already
qualified (never the full market, never raw tick data — that's the doc's
explicit boundary) and runs the final qualitative pass on it.

IPC choice: plain HTTP from Rust to this service (see
`crates/market-data/src/qualify.rs`). Simplest option that keeps the two
languages fully decoupled — either side can be restarted, redeployed, or
tested independently, and there's no shared serialization format to keep
in sync beyond this one JSON contract.

Run with: `uvicorn app.main:app --reload` (from `python/`, so `.env` at
the repo root is found one directory up).
"""

from pathlib import Path

from dotenv import load_dotenv
from fastapi import FastAPI
from pydantic import BaseModel

from .config import AlpacaConfig
from .news import fetch_recent_news, tag_catalysts

# Repo-root .env, same file every Rust crate reads — one source of truth
# for credentials, not a Python-specific copy.
load_dotenv(Path(__file__).resolve().parent.parent.parent / ".env")

app = FastAPI(title="stockspotter qualitative layer")
_cfg = AlpacaConfig.from_env()


class QualifyRequest(BaseModel):
    symbols: list[str]
    news_limit: int = 10


class SymbolQualification(BaseModel):
    symbol: str
    catalyst_tags: list[str]
    headline_count: int
    most_recent_headline: str | None = None
    most_recent_published_at: str | None = None
    error: str | None = None


class QualifyResponse(BaseModel):
    results: list[SymbolQualification]


@app.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/qualify", response_model=QualifyResponse)
async def qualify(req: QualifyRequest) -> QualifyResponse:
    results: list[SymbolQualification] = []
    for symbol in req.symbols:
        try:
            news_items = await fetch_recent_news(_cfg, symbol, limit=req.news_limit)
        except Exception as e:  # noqa: BLE001 — one symbol's news failure
            # shouldn't fail the whole batch; report it per-symbol instead.
            results.append(
                SymbolQualification(
                    symbol=symbol,
                    catalyst_tags=[],
                    headline_count=0,
                    error=str(e),
                )
            )
            continue

        tags: set[str] = set()
        for item in news_items:
            tags.update(tag_catalysts(f"{item.get('headline', '')} {item.get('summary', '')}"))

        results.append(
            SymbolQualification(
                symbol=symbol,
                catalyst_tags=sorted(tags),
                headline_count=len(news_items),
                most_recent_headline=news_items[0]["headline"] if news_items else None,
                most_recent_published_at=news_items[0]["created_at"] if news_items else None,
            )
        )

    return QualifyResponse(results=results)
