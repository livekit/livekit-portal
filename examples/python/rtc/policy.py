"""RTC policy: toy chunker that answers `request_chunk` RPCs.

Sits on the operator side. For each request:
  1. Parse `{current_step, active_chunk_id, d_hint}`.
  2. Synthesize a chunk of HORIZON future actions starting at
     `current_step + d_hint`. The "policy" is a pair of sines — visible in
     the state stream on the robot side.
  3. Cache the emitted chunk so a real RTC model could slice
     `prev_chunk_left_over = cached[d_hint:d_hint+d_hint]` on the next call
     (not done here, this is a pure toy).
  4. Push the chunk to the robot via `send_action_chunk`. Return "ok" from
     the RPC so the robot knows the request landed.

A real deployment would run this behind a GPU inference server (Modal,
Runpod, or local). Swap `_synthesize` for a forward pass of your policy.
"""
from __future__ import annotations

import asyncio
import json
import struct
import time
from typing import Optional

import numpy as np

from livekit.portal import (
    ActionChunk,
    ChunkDtype,
    Portal,
    PortalConfig,
    Role,
    RpcError,
    RpcInvocationData,
)
from _common import env_float, env_int, load_env, mint_token, required_env

IDENTITY = "policy"
ACTION_DIM = 7
STATE_FIELDS = [f"j{i}" for i in range(ACTION_DIM)]


def _synthesize(step_start: int, horizon: int, control_hz: int) -> np.ndarray:
    """Toy policy. Emits `horizon` future actions as superposed sines per
    joint. Guarantees smooth continuity across chunks by conditioning on the
    absolute step index, so overlapping windows match.
    """
    t = (step_start + np.arange(horizon, dtype=np.float32)) / control_hz
    chunk = np.zeros((horizon, ACTION_DIM), dtype=np.float32)
    for j in range(ACTION_DIM):
        freq = 0.4 + 0.1 * j
        phase = j * 0.5
        chunk[:, j] = 0.6 * np.sin(2 * np.pi * freq * t + phase)
    return chunk


async def main() -> None:
    load_env()
    url = required_env("LIVEKIT_URL")
    room = required_env("LIVEKIT_ROOM")
    token = mint_token(IDENTITY, room)

    control_hz = env_int("RTC_CONTROL_HZ", 30)
    horizon = env_int("RTC_HORIZON", 32)
    simulated_inference_ms = env_float("RTC_INFERENCE_MS", 40.0)

    cfg = PortalConfig(room, Role.OPERATOR)
    cfg.add_state(STATE_FIELDS)
    cfg.set_fps(control_hz)
    portal = Portal(cfg)

    # Cache of the last emitted chunk, keyed by chunk id. A real RTC head
    # would read this to build `prev_chunk_left_over` for the next inference.
    last_chunk: Optional[np.ndarray] = None
    last_chunk_start: int = 0
    chunks_emitted = 0

    async def request_chunk_handler(data: RpcInvocationData) -> str:
        nonlocal last_chunk, last_chunk_start, chunks_emitted
        try:
            req = json.loads(data.payload)
        except json.JSONDecodeError as e:
            raise RpcError.Error(code=4000, message=f"bad json: {e}", data=None)
        current_step = int(req["current_step"])
        d_hint = int(req.get("d_hint", 1))

        start_step = current_step + d_hint

        # Simulate GPU inference latency so the scheduling logic gets tested.
        if simulated_inference_ms > 0:
            await asyncio.sleep(simulated_inference_ms / 1000.0)

        chunk = _synthesize(start_step, horizon, control_hz)
        last_chunk = chunk
        last_chunk_start = start_step

        payload = chunk.tobytes()  # little-endian f32, H * K * 4 bytes
        ac = ActionChunk(
            horizon=horizon,
            action_dim=ACTION_DIM,
            dtype=ChunkDtype.F32,
            # Demo shortcut: stash the chunk's intended start step in
            # captured_at_us so the robot can derive the splice point from
            # what Portal already carries. A real integration would add
            # start_step as part of the caller's own RPC-ack or as chunk
            # attributes alongside captured_at_us.
            captured_at_us=start_step,
            payload=payload,
        )
        # Send to whoever called us; the RPC caller identity is in `data`.
        await portal.send_action_chunk(ac, destination=data.caller_identity)
        chunks_emitted += 1
        if chunks_emitted % 5 == 0:
            print(
                f"[policy] served {chunks_emitted} chunks;"
                f" latest start_step={start_step} horizon={horizon}"
            )
        return "ok"

    portal.register_rpc_method("request_chunk", request_chunk_handler)

    # Observe the robot's state stream just to log progress. Not required
    # by the RTC protocol.
    def on_state(s) -> None:
        # Print once per second.
        if int(s.timestamp_us) % 1_000_000 < 33_000:
            joints = [f"{s.values[f]:+.2f}" for f in STATE_FIELDS]
            print(f"[policy] robot state: [{' '.join(joints)}]")

    portal.on_state(on_state)

    print(f"[policy] connecting to {url} as '{IDENTITY}' in room '{room}' ...")
    await portal.connect(url, token)
    print(
        f"[policy] connected; horizon={horizon}"
        f" simulated_inference_ms={simulated_inference_ms:.1f}"
    )

    duration_s = env_float("RTC_DURATION_SECONDS", 20.0)
    try:
        await asyncio.sleep(duration_s + 5.0)
    except asyncio.CancelledError:
        pass
    finally:
        print(f"[policy] emitted {chunks_emitted} chunks; shutting down")
        await portal.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
