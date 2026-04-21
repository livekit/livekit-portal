"""livekit-portal. Python bindings.

Thin ergonomic wrapper over the UniFFI-generated `livekit_portal_ffi`
module. The generated module already exposes `Portal`, `PortalConfig`, the
record types (`Observation`, `Action`, `State`, `VideoFrame`, `PortalMetrics`
and nested submetrics), the `PortalError` exception, and the
`PortalCallbacks` foreign trait. This module:

  * Renames `VideoFrame` to `VideoFrameData` for backwards API parity with
    the old protobuf-based wrapper (consumers import `VideoFrameData`).
  * Adds `Portal.on_action / on_state / on_observation / on_video_frame /
    on_drop` convenience registrations, routed through an internal dispatcher
    that implements `PortalCallbacks`. Callbacks run on the asyncio event
    loop of the thread that registered them (not on the tokio worker that
    fires the event), matching the previous wrapper's semantics.
  * Adds frame-normalization on `send_video_frame` (accept bytes or
    `np.ndarray(H, W, 3)` uint8 and infer W/H from the array).

Frame formats on the wire are unchanged: sends take RGB24, receives deliver
I420 planar. Use `livekit.portal.i420_bytes_to_numpy_rgb` to convert received
frames for display.
"""
from __future__ import annotations

import asyncio
import threading
import traceback
from typing import Any, Callable, Dict, List, Optional

from . import _frame
from . import livekit_portal_ffi as _ffi
from ._frame import i420_bytes_to_numpy_rgb

# Re-export generated types. The UniFFI module is the source of truth for
# class identity — wrapping them here would force duplicate isinstance checks.
Role = _ffi.Role
Observation = _ffi.Observation
Action = _ffi.Action
State = _ffi.State
VideoFrameData = _ffi.VideoFrame
PortalMetrics = _ffi.PortalMetrics
SyncMetrics = _ffi.SyncMetrics
TransportMetrics = _ffi.TransportMetrics
BufferMetrics = _ffi.BufferMetrics
RttMetrics = _ffi.RttMetrics
PortalError = _ffi.PortalError


# --- Dispatcher -------------------------------------------------------------

class _Dispatcher(_ffi.PortalCallbacks):
    """Sits behind a Portal as its `PortalCallbacks` implementation.

    The foreign-trait methods run on the tokio worker thread that fired the
    event — we must *not* execute user code there (long work would block the
    video/state receive path, and reentering the FFI from there would
    deadlock). Everything is hopped onto the registered asyncio loop via
    `call_soon_threadsafe`.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._action_cb: Optional[Callable[[Action], Any]] = None
        self._state_cb: Optional[Callable[[State], Any]] = None
        self._observation_cb: Optional[Callable[[Observation], Any]] = None
        self._drop_cb: Optional[Callable[[List[Dict[str, float]]], Any]] = None
        # Per-track video callback: track_name → callable(track_name, frame).
        self._video_cbs: Dict[str, Callable[[str, VideoFrameData], Any]] = {}

    def bind_loop(self, loop: asyncio.AbstractEventLoop) -> None:
        with self._lock:
            self._loop = loop

    def _schedule(self, cb: Callable[..., Any], *args: Any) -> None:
        loop = self._loop
        if loop is None:
            _safely_call(cb, *args)
            return
        loop.call_soon_threadsafe(_safely_call, cb, *args)

    # --- PortalCallbacks trait impls (called from Rust/tokio thread) --------

    def on_action(self, action: Action) -> None:
        cb = self._action_cb
        if cb is not None:
            self._schedule(cb, action)

    def on_state(self, state: State) -> None:
        cb = self._state_cb
        if cb is not None:
            self._schedule(cb, state)

    def on_observation(self, observation: Observation) -> None:
        cb = self._observation_cb
        if cb is not None:
            self._schedule(cb, observation)

    def on_video_frame(self, track_name: str, frame: VideoFrameData) -> None:
        cb = self._video_cbs.get(track_name)
        if cb is not None:
            self._schedule(cb, track_name, frame)

    def on_drop(self, dropped: List[Dict[str, float]]) -> None:
        cb = self._drop_cb
        if cb is not None:
            self._schedule(cb, dropped)

    # --- Registration (from Python user thread) -----------------------------

    def set_action(self, cb: Callable[[Action], Any]) -> None:
        self._action_cb = cb

    def set_state(self, cb: Callable[[State], Any]) -> None:
        self._state_cb = cb

    def set_observation(self, cb: Callable[[Observation], Any]) -> None:
        self._observation_cb = cb

    def set_drop(self, cb: Callable[[List[Dict[str, float]]], Any]) -> None:
        self._drop_cb = cb

    def set_video(self, track_name: str, cb: Callable[[str, VideoFrameData], Any]) -> None:
        self._video_cbs[track_name] = cb


def _safely_call(cb: Callable[..., Any], *args: Any) -> None:
    try:
        result = cb(*args)
        # If the user registered `async def`, schedule the coroutine on the
        # current event loop. `call_soon_threadsafe` runs the outer callable
        # inside the loop thread, so `get_event_loop` here is safe.
        if asyncio.iscoroutine(result):
            asyncio.ensure_future(result)
    except BaseException:  # noqa: BLE001
        traceback.print_exc()


# --- PortalConfig -----------------------------------------------------------

class PortalConfig:
    """Builder for a Portal session.

    Mirrors the old protobuf-wrapper API so existing callers keep working.
    State (`video_tracks`, `state_fields`, `action_fields`) is mirrored in
    Python for fast lookup — the Rust side owns the authoritative copy.
    """

    __slots__ = (
        "_inner",
        "_session",
        "_role",
        "_video_tracks",
        "_state_fields",
        "_action_fields",
    )

    def __init__(self, session: str, role: Role) -> None:
        self._inner = _ffi.PortalConfig(session, role)
        self._session = session
        self._role = role
        self._video_tracks: List[str] = []
        self._state_fields: List[str] = []
        self._action_fields: List[str] = []

    @property
    def session(self) -> str:
        return self._session

    @property
    def role(self) -> Role:
        return self._role

    @property
    def video_tracks(self) -> List[str]:
        return list(self._video_tracks)

    @property
    def state_fields(self) -> List[str]:
        return list(self._state_fields)

    @property
    def action_fields(self) -> List[str]:
        return list(self._action_fields)

    def add_video(self, name: str) -> None:
        self._inner.add_video(name)
        self._video_tracks.append(name)

    def add_state(self, fields: List[str]) -> None:
        self._inner.add_state(list(fields))
        self._state_fields.extend(fields)

    def add_action(self, fields: List[str]) -> None:
        self._inner.add_action(list(fields))
        self._action_fields.extend(fields)

    def set_fps(self, fps: int) -> None:
        self._inner.set_fps(fps)

    def set_slack(self, ticks: int) -> None:
        self._inner.set_slack(ticks)

    def set_tolerance(self, ticks: float) -> None:
        self._inner.set_tolerance(ticks)

    def set_state_reliable(self, reliable: bool) -> None:
        self._inner.set_state_reliable(reliable)

    def set_action_reliable(self, reliable: bool) -> None:
        self._inner.set_action_reliable(reliable)

    def set_ping_ms(self, ms: int) -> None:
        self._inner.set_ping_ms(ms)

    def close(self) -> None:
        """No-op: UniFFI releases the Rust-side handle when Python GC drops
        the last reference. Kept for backwards compatibility with callers
        that explicitly `close()`.
        """
        # Drop our reference so the underlying Arc can be released.
        self._inner = None  # type: ignore[assignment]


# --- Portal -----------------------------------------------------------------

class Portal:
    """Main session object.

    Construct with a `PortalConfig`, then `await connect(url, token)`.
    Register push callbacks with `on_action / on_state / on_observation /
    on_video_frame / on_drop`.
    """

    __slots__ = (
        "_inner",
        "_dispatcher",
        "_state_fields",
        "_action_fields",
        "_video_tracks",
    )

    def __init__(self, config: PortalConfig) -> None:
        self._dispatcher = _Dispatcher()
        self._inner = _ffi.Portal(config._inner, self._dispatcher)
        # Snapshot what the Rust side confirmed it was built with.
        self._state_fields: List[str] = list(self._inner.state_fields())
        self._action_fields: List[str] = list(self._inner.action_fields())
        self._video_tracks: List[str] = list(self._inner.video_tracks())

    # -- async lifecycle -----------------------------------------------------

    async def connect(self, url: str, token: str) -> None:
        # Callbacks fire on tokio workers; hop them onto this loop.
        self._dispatcher.bind_loop(asyncio.get_running_loop())
        await self._inner.connect(url, token)

    async def disconnect(self) -> None:
        await self._inner.disconnect()

    # -- send (sync, fire-and-forget) ----------------------------------------

    def send_video_frame(
        self,
        track_name: str,
        frame: Any,
        width: Optional[int] = None,
        height: Optional[int] = None,
        timestamp_us: Optional[int] = None,
    ) -> None:
        rgb, w, h = _frame.normalize_rgb(frame, width, height)
        self._inner.send_video_frame(track_name, rgb, w, h, timestamp_us)

    def send_state(
        self,
        values: Dict[str, float],
        timestamp_us: Optional[int] = None,
    ) -> None:
        self._inner.send_state(values, timestamp_us)

    def send_action(
        self,
        values: Dict[str, float],
        timestamp_us: Optional[int] = None,
    ) -> None:
        self._inner.send_action(values, timestamp_us)

    # -- pull (sync, latest-wins) --------------------------------------------

    def get_observation(self) -> Optional[Observation]:
        return self._inner.get_observation()

    def get_action(self) -> Optional[Action]:
        return self._inner.get_action()

    def get_state(self) -> Optional[State]:
        return self._inner.get_state()

    def get_video_frame(self, track_name: str) -> Optional[VideoFrameData]:
        return self._inner.get_video_frame(track_name)

    # -- push callbacks ------------------------------------------------------

    def on_action(self, callback: Callable[[Action], Any]) -> None:
        self._dispatcher.set_action(callback)

    def on_state(self, callback: Callable[[State], Any]) -> None:
        self._dispatcher.set_state(callback)

    def on_observation(self, callback: Callable[[Observation], Any]) -> None:
        self._dispatcher.set_observation(callback)

    def on_video_frame(
        self,
        track_name: str,
        callback: Callable[[str, VideoFrameData], Any],
    ) -> None:
        self._dispatcher.set_video(track_name, callback)

    def on_drop(
        self,
        callback: Callable[[List[Dict[str, float]]], Any],
    ) -> None:
        self._dispatcher.set_drop(callback)

    # -- metrics -------------------------------------------------------------

    def metrics(self) -> PortalMetrics:
        return self._inner.metrics()

    def reset_metrics(self) -> None:
        self._inner.reset_metrics()

    # -- cleanup -------------------------------------------------------------

    def close(self) -> None:
        """No-op: UniFFI releases the Rust-side handle when Python GC drops
        the last reference. Kept for backwards compatibility."""
        self._inner = None  # type: ignore[assignment]


__all__ = [
    "Role",
    "PortalConfig",
    "Portal",
    "Observation",
    "Action",
    "State",
    "VideoFrameData",
    "PortalMetrics",
    "SyncMetrics",
    "TransportMetrics",
    "BufferMetrics",
    "RttMetrics",
    "PortalError",
    "i420_bytes_to_numpy_rgb",
]
