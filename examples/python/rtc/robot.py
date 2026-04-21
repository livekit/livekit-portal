"""RTC robot: toy 7-joint sim driven by chunks pushed from the policy side.

Protocol:
  1. Sim tick at CONTROL_HZ. Each tick pulls the next action from a local
     chunk queue and applies it (toy: state <- action with a low-pass).
  2. When the queue has <= trigger_remaining actions left AND no request is
     already in flight, fire an RPC `request_chunk` to the policy. The
     payload carries the robot's current step index and an `inference_delay`
     hint so the policy's RTC head can anchor its `prev_chunk_left_over`.
  3. The policy responds to the RPC with "ok" and separately pushes a new
     chunk via `send_action_chunk`. `on_action_chunk` fires here, and we
     splice the chunk into the queue at `real_delay = current_step -
     chunk.start_step` to drop already-executed actions.
  4. On disconnect, print a short summary.

Portal is pure transport. All RTC state (active_chunk_id, current_step,
queue, splicing) lives in this script.
"""
from __future__ import annotations

import asyncio
import json
import math
import time
from dataclasses import dataclass, field
from typing import List, Optional

import numpy as np

from livekit.portal import (
    ActionChunk,
    ChunkDtype,
    Portal,
    PortalConfig,
    Role,
    State,
)
from _common import env_float, env_int, load_env, mint_token, required_env

IDENTITY = "robot"
ACTION_DIM = 7
STATE_FIELDS = [f"j{i}" for i in range(ACTION_DIM)]
ROBOT_ACTION_CHUNK_TOPIC = "portal_action_chunk"


@dataclass
class ChunkQueue:
    """Holds the currently executing chunk plus an index into it.

    `start_step` is the global control-step index the chunk begins at —
    knowing this lets an arriving chunk locate its splice point relative
    to the current step.
    """

    chunk_id: int = 0
    start_step: int = 0
    actions: np.ndarray = field(default_factory=lambda: np.zeros((0, ACTION_DIM)))

    def remaining(self, current_step: int) -> int:
        consumed = max(0, current_step - self.start_step)
        return max(0, len(self.actions) - consumed)

    def pop(self, current_step: int) -> Optional[np.ndarray]:
        idx = current_step - self.start_step
        if 0 <= idx < len(self.actions):
            return self.actions[idx]
        return None


async def main() -> None:
    load_env()
    url = required_env("LIVEKIT_URL")
    room = required_env("LIVEKIT_ROOM")
    token = mint_token(IDENTITY, room)

    control_hz = env_int("RTC_CONTROL_HZ", 30)
    duration_s = env_float("RTC_DURATION_SECONDS", 20.0)
    # When remaining actions in the current chunk fall to this threshold,
    # fire the next RPC. Chosen so inference typically completes before the
    # queue drains.
    trigger_remaining = env_int("RTC_TRIGGER_REMAINING", 12)

    cfg = PortalConfig(room, Role.ROBOT)
    cfg.add_state(STATE_FIELDS)
    cfg.set_fps(control_hz)
    portal = Portal(cfg)

    # --- State exposed to callbacks -----------------------------------------

    current_step = 0
    queue = ChunkQueue()
    chunk_in_flight = asyncio.Event()
    chunk_in_flight.set()  # initially "done" so the first tick triggers
    rpc_in_flight: Optional[asyncio.Task] = None
    last_action = np.zeros(ACTION_DIM, dtype=np.float32)
    latencies_us: List[int] = []
    chunks_received = 0

    # --- Chunk arrival: splice into queue ----------------------------------

    def on_chunk(chunk: ActionChunk) -> None:
        nonlocal queue, chunks_received
        if chunk.dtype != ChunkDtype.F32:
            print(f"[robot] unexpected dtype {chunk.dtype}; ignoring")
            return
        data = np.frombuffer(chunk.payload, dtype=np.float32).reshape(
            chunk.horizon, chunk.action_dim
        )
        # The policy encodes the chunk's intended start step in
        # captured_at_us for simplicity in this demo. A real policy would
        # carry it as its own protocol field (the robot tells it where to
        # start in the RPC payload).
        start_step = int(chunk.captured_at_us)  # demo shortcut
        real_delay = max(0, current_step - start_step)
        if real_delay >= chunk.horizon:
            # Chunk fully outdated: the policy was too slow. Skip it and
            # hold last_action — next request will cover.
            print(
                f"[robot] chunk id=? outdated (real_delay={real_delay} >= H={chunk.horizon}), skipping"
            )
            chunk_in_flight.set()
            return
        queue = ChunkQueue(
            chunk_id=queue.chunk_id + 1,
            start_step=start_step + real_delay,
            actions=np.ascontiguousarray(data[real_delay:]),
        )
        chunks_received += 1
        chunk_in_flight.set()

    portal.on_action_chunk(on_chunk)

    # --- Connect ------------------------------------------------------------

    print(f"[robot] connecting to {url} as '{IDENTITY}' in room '{room}' ...")
    await portal.connect(url, token)
    print(f"[robot] connected; control_hz={control_hz} dof={ACTION_DIM}")

    # --- RTC request -------------------------------------------------------

    async def fire_request() -> None:
        """Send the RPC; swallow errors so a missed round doesn't kill the loop."""
        nonlocal rpc_in_flight
        payload = json.dumps(
            {
                "current_step": current_step,
                "active_chunk_id": queue.chunk_id,
                # d_hint: round-trip p95 in control ticks, minimum 1. Bumps
                # the policy's inference_delay so its soft-guidance anchor
                # lines up with where the robot will actually be.
                "d_hint": max(
                    1,
                    int(
                        (np.percentile(latencies_us, 95) if latencies_us else 30_000)
                        * control_hz
                        / 1_000_000
                    ),
                ),
            }
        )
        sent_at = time.monotonic_ns() // 1_000
        try:
            await portal.perform_rpc(
                "request_chunk", payload=payload, response_timeout_ms=5000
            )
        except Exception as e:
            print(f"[robot] request_chunk rpc failed: {e}")
            chunk_in_flight.set()
            return
        # Record policy-round-trip for d_hint smoothing. Note: this measures
        # RPC round-trip, not chunk arrival. The chunk arrives separately on
        # `on_action_chunk`.
        now_us = time.monotonic_ns() // 1_000
        latencies_us.append(now_us - sent_at)
        if len(latencies_us) > 50:
            del latencies_us[:-50]

    # --- Control loop ------------------------------------------------------

    interval = 1.0 / control_hz
    next_tick = time.monotonic()
    n_ticks = int(duration_s * control_hz)
    print(f"[robot] running for {duration_s:.1f}s ({n_ticks} ticks)")

    try:
        for _ in range(n_ticks):
            # 1. Pick action for this tick.
            a = queue.pop(current_step)
            if a is not None:
                last_action = a.astype(np.float32)
            # 2. Apply to toy sim (state <- action via shallow low-pass).
            # Here we just publish last_action as the joint state so the
            # operator-side monitor can visualize the motion.
            state = {name: float(last_action[i]) for i, name in enumerate(STATE_FIELDS)}
            portal.send_state(state, timestamp_us=int(time.time() * 1_000_000))

            # 3. Trigger next request if running low.
            remaining = queue.remaining(current_step)
            if remaining <= trigger_remaining and chunk_in_flight.is_set():
                chunk_in_flight.clear()
                rpc_in_flight = asyncio.create_task(fire_request())

            current_step += 1
            next_tick += interval
            sleep_for = next_tick - time.monotonic()
            if sleep_for > 0:
                await asyncio.sleep(sleep_for)

        if rpc_in_flight is not None:
            await asyncio.wait_for(rpc_in_flight, timeout=2.0)
    except (asyncio.CancelledError, asyncio.TimeoutError):
        pass
    finally:
        print(
            f"[robot] ran {current_step} ticks, received {chunks_received} chunks,"
            f" final queue remaining={queue.remaining(current_step)}"
        )
        if latencies_us:
            print(
                f"[robot] rpc round-trip p50={np.percentile(latencies_us, 50):.0f}us"
                f" p95={np.percentile(latencies_us, 95):.0f}us"
            )
        await portal.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
