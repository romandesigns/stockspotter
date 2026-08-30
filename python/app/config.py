"""Loads Alpaca connection config from the environment (repo-root `.env`,
via python-dotenv — main.py loads it before this runs). Same env var names
the Rust crates use, so there's one `.env` for the whole project, not a
Python-specific copy.
"""

import os
from dataclasses import dataclass


@dataclass
class AlpacaConfig:
    api_key: str
    api_secret: str
    data_base: str

    @classmethod
    def from_env(cls) -> "AlpacaConfig":
        return cls(
            api_key=os.environ["ALPACA_API_KEY"],
            api_secret=os.environ["ALPACA_API_SECRET"],
            data_base=os.environ.get("ALPACA_DATA_BASE", "https://data.alpaca.markets"),
        )
