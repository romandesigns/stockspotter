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


@dataclass
class AnthropicConfig:
    """Chart Page AI assessment feature (2026-09-03) -- see assess.py.
    Same required-vs-optional convention as AlpacaConfig above: the key
    itself has no sane default (raises via `os.environ[...]` if unset),
    the API base does.
    """

    api_key: str
    api_base: str

    @classmethod
    def from_env(cls) -> "AnthropicConfig":
        return cls(
            api_key=os.environ["ANTHROPIC_API_KEY"],
            api_base=os.environ.get("ANTHROPIC_API_BASE", "https://api.anthropic.com"),
        )
