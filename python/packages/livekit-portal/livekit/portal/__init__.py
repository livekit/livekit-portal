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
import logging
import threading
import traceback
from typing import Any, Callable, Dict, List, Optional

_log = logging.getLogger(__name__)

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
ActionChunk = _ffi.ActionChunk
ChunkDtype = _ffi.ChunkDtype
PortalMetrics = _ffi.PortalMetrics
SyncMetrics = _ffi.SyncMetrics
TransportMetrics = _ffi.TransportMetrics
BufferMetrics = _ffi.BufferMetrics
RttMetrics = _ffi.RttMetrics
PortalError = _ffi.PortalError
RpcInvocationData = _ffi.RpcInvocationData
RpcError = _ffi.RpcError


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
        self._action_chunk_cb: Optional[Callable[[ActionChunk], Any]] = None
        # Per-track video callback: track_name → callable(track_name, frame).
        self._video_cbs: Dict[str, Callable[[str, VideoFrameData], Any]] = {}
        # Per-topic byte-stream callback: topic → callable(sender, data).
        self._byte_stream_cbs: Dict[str, Callable[[str, bytes], Any]] = {}

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

    def on_action_chunk(self, chunk: ActionChunk) -> None:
        cb = self._action_chunk_cb
        if cb is not None:
            self._schedule(cb, chunk)

    # --- Registration (from Python user thread) -----------------------------

    def set_action(self, cb: Callable[[Action], Any]) -> None:
        self._action_cb = cb

    def set_state(self, cb: Callable[[State], Any]) -> None:
        self._state_cb = cb

    def set_observation(self, cb: Callable[[Observation], Any]) -> None:
        self._observation_cb = cb

    def set_drop(self, cb: Callable[[List[Dict[str, float]]], Any]) -> None:
        self._drop_cb = cb

    def set_action_chunk(self, cb: Callable[[ActionChunk], Any]) -> None:
        self._action_chunk_cb = cb

    def set_video(self, track_name: str, cb: Callable[[str, VideoFrameData], Any]) -> None:
        self._video_cbs[track_name] = cb


_uniffi_bound_loop: Optional[asyncio.AbstractEventLoop] = None
_uniffi_bound_loop_lock = threading.Lock()


def _set_uniffi_event_loop(loop: asyncio.AbstractEventLoop) -> None:
    """Point UniFFI's global async-trait dispatch at `loop`.

    The underlying `uniffi_set_event_loop` is a process-global — multiple
    Portals on different asyncio loops in the same process will collide.
    Warn on mismatch so the misuse is at least visible rather than a silent
    cross-loop dispatch. The normal single-loop case is a no-op on the
    second call.
    """
    global _uniffi_bound_loop
    with _uniffi_bound_loop_lock:
        if _uniffi_bound_loop is loop:
            return
        if _uniffi_bound_loop is not None and _uniffi_bound_loop.is_running():
            _log.warning(
                "livekit-portal: multiple Portals bound to different asyncio "
                "loops in the same process; RPC handler dispatch will run on "
                "the most-recently-connected loop. This is a UniFFI "
                "limitation (uniffi_set_event_loop is process-global)."
            )
        _uniffi_bound_loop = loop
        _ffi.uniffi_set_event_loop(loop)


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


# --- RPC handler adapter ----------------------------------------------------


class _RpcHandlerAdapter(_ffi.RpcHandler):
    """Wrap a user callable so it satisfies the UniFFI async trait.

    Accepts either `async def handle(data) -> str` or sync `def handle(data) -> str`.
    Sync callables run inline on the asyncio loop, so handlers doing blocking
    work should use `async def` and `await asyncio.to_thread(...)` themselves.
    Raising `RpcError.Error` propagates to the caller; any other exception
    becomes a generic application error (code 1500).
    """

    def __init__(self, callback: Callable[[RpcInvocationData], Any]) -> None:
        super().__init__()
        self._callback = callback

    async def handle(self, data: RpcInvocationData) -> str:
        try:
            result = self._callback(data)
            if asyncio.iscoroutine(result):
                return await result
            return result  # type: ignore[return-value]
        except (_ffi.RpcError, asyncio.CancelledError):
            # RpcError: user-signalled application error, propagate verbatim.
            # CancelledError: the Rust side dropped the future (timeout or
            # caller cancellation); let asyncio unwind the task cleanly
            # instead of writing a bogus result on a torn-down handle.
            raise
        except Exception as e:  # noqa: BLE001
            traceback.print_exc()
            raise _ffi.RpcError.Error(
                code=1500,
                message=f"handler raised {type(e).__name__}: {e}",
                data=None,
            ) from e


# --- Byte stream handler adapter -------------------------------------------


class _ByteStreamHandlerAdapter(_ffi.ByteStreamHandler):
    """Wrap a user callable so it satisfies the UniFFI byte-stream trait.

    The handler is invoked on a tokio worker when a matching byte stream
    finishes reading; we hop onto the registered asyncio loop for user code.
    Accepts `def handle(sender, data)` or `async def`.
    """

    def __init__(
        self,
        callback: Callable[[str, bytes], Any],
        dispatcher: "_Dispatcher",
    ) -> None:
        super().__init__()
        self._callback = callback
        self._dispatcher = dispatcher

    def handle(self, sender: str, data: bytes) -> None:
        self._dispatcher._schedule(self._callback, sender, data)


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
        # Callbacks fire on tokio workers; hop them onto this loop. The
        # UniFFI-generated RPC handler dispatch also needs to know which
        # loop to run async foreign-trait methods on, since it's invoked
        # from a tokio worker with no asyncio loop of its own.
        loop = asyncio.get_running_loop()
        self._dispatcher.bind_loop(loop)
        _set_uniffi_event_loop(loop)
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

    # -- byte streams (generic reliable binary) ------------------------------

    async def send_bytes(
        self,
        topic: str,
        data: bytes,
        destination: Optional[str] = None,
    ) -> None:
        """Send a one-shot binary payload on `topic`. Reliable and ordered,
        no 15 KB cap. `destination` is a participant identity; `None`
        broadcasts to the room.
        """
        await self._inner.send_bytes(topic, data, destination)

    def register_byte_stream_handler(
        self,
        topic: str,
        handler: Callable[[str, bytes], Any],
    ) -> None:
        """Register a handler for byte streams on `topic`. `handler(sender,
        data)` fires once per incoming stream, with the sender's participant
        identity and the assembled payload. May be `def` or `async def`.

        The topic `portal_action_chunk` is reserved for `send_action_chunk`;
        register a handler there at your own risk (it replaces Portal's
        built-in chunk dispatcher).
        """
        wrapper = _ByteStreamHandlerAdapter(handler, self._dispatcher)
        self._inner.register_byte_stream_handler(topic, wrapper)

    def unregister_byte_stream_handler(self, topic: str) -> None:
        self._inner.unregister_byte_stream_handler(topic)

    # -- action chunks -------------------------------------------------------

    async def send_action_chunk(
        self,
        chunk: ActionChunk,
        destination: Optional[str] = None,
    ) -> None:
        """Send an action chunk. `chunk.payload` must be `horizon * action_dim
        * sizeof(dtype)` little-endian bytes.
        """
        await self._inner.send_action_chunk(chunk, destination)

    def get_action_chunk(self) -> Optional[ActionChunk]:
        return self._inner.get_action_chunk()

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

    def on_action_chunk(self, callback: Callable[[ActionChunk], Any]) -> None:
        """Register a callback fired once per received `ActionChunk`. The
        callback can be `def` or `async def`; it runs on this Portal's
        asyncio loop.
        """
        self._dispatcher.set_action_chunk(callback)

    # -- rpc -----------------------------------------------------------------

    def peer_identity(self) -> Optional[str]:
        """Identity of the peer once Portal has seen any traffic from them.

        `None` before the peer has published any Portal-topic data packet
        or a subscribed video track (whichever happens first).
        """
        return self._inner.peer_identity()

    def register_rpc_method(
        self,
        method: str,
        handler: Callable[[RpcInvocationData], Any],
    ) -> None:
        """Register a handler for `method`. The handler is invoked on this
        Portal's asyncio loop whenever a peer calls `perform_rpc(method, ...)`.

        `handler` may be a regular `def` returning `str`, or an `async def`
        returning `str`. To signal an application error, `raise
        RpcError.Error(code=..., message=..., data=...)` — that will be
        serialized back to the caller.
        """
        wrapper = _RpcHandlerAdapter(handler)
        self._inner.register_rpc_method(method, wrapper)

    def unregister_rpc_method(self, method: str) -> None:
        self._inner.unregister_rpc_method(method)

    async def perform_rpc(
        self,
        method: str,
        payload: str = "",
        destination: Optional[str] = None,
        response_timeout_ms: Optional[int] = None,
    ) -> str:
        """Invoke `method` on the peer. When `destination` is omitted,
        Portal routes to the identified peer (see `peer_identity`),
        falling back to the single remote participant if none is
        identified yet. Returns the handler's string payload.
        """
        return await self._inner.perform_rpc(
            destination,
            method,
            payload,
            response_timeout_ms,
        )

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
    "ActionChunk",
    "ChunkDtype",
    "PortalMetrics",
    "SyncMetrics",
    "TransportMetrics",
    "BufferMetrics",
    "RttMetrics",
    "PortalError",
    "RpcInvocationData",
    "RpcError",
    "i420_bytes_to_numpy_rgb",
]
