"""Shared helpers for the RTC example scripts.

Loads .env from the script dir, mints LiveKit JWTs, and exposes env-var
helpers. Copied from examples/python/basic so the example stays
self-contained (uv sync from this directory works with no workspace setup).
"""
from __future__ import annotations

import datetime
import os
import pathlib
import sys
from typing import Optional

try:
    from dotenv import load_dotenv
except ImportError as exc:
    print(
        "examples require python-dotenv. Install with:\n"
        "    uv pip install livekit-api python-dotenv",
        file=sys.stderr,
    )
    raise SystemExit(1) from exc

try:
    from livekit import api
except ImportError as exc:
    print(
        "examples require livekit-api. Install with:\n"
        "    uv pip install livekit-api python-dotenv",
        file=sys.stderr,
    )
    raise SystemExit(1) from exc


def load_env(search_from: Optional[pathlib.Path] = None) -> None:
    start = search_from or pathlib.Path(__file__).parent
    search_dirs = [start, start.parent, pathlib.Path.cwd()]
    loaded_any = False
    for d in search_dirs:
        env = d / ".env"
        if env.exists():
            load_dotenv(env, override=False)
            loaded_any = True
        env_local = d / ".env.local"
        if env_local.exists():
            load_dotenv(env_local, override=True)
            loaded_any = True
        if loaded_any:
            return


def mint_token(identity: str, room: str, ttl_hours: int = 6) -> str:
    key = os.environ.get("LIVEKIT_API_KEY")
    secret = os.environ.get("LIVEKIT_API_SECRET")
    if not key or not secret:
        raise RuntimeError(
            "LIVEKIT_API_KEY and LIVEKIT_API_SECRET must be set (see .env.example)"
        )
    grants = api.VideoGrants(
        room_join=True, room=room, can_publish=True, can_subscribe=True
    )
    token = (
        api.AccessToken(key, secret)
        .with_identity(identity)
        .with_grants(grants)
        .with_ttl(datetime.timedelta(hours=ttl_hours))
    )
    return token.to_jwt()


def required_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"{name} must be set (see .env.example)")
    return value


def env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    return int(raw) if raw else default


def env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    return float(raw) if raw else default
