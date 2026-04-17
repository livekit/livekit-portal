"""Event router for FfiEvent callbacks.

Two kinds of events arrive from Rust:
  1. Async-op completions (`ConnectCallback`, `DisconnectCallback`). match by
     `async_id` against a pending future registered by the caller.
  2. Push events (`ActionEvent`, `ObservationEvent`, `StateEvent`,
     `VideoFrameEvent`, `DropEvent`). scoped by `portal_handle` and an
     event kind string; routed to the most recently registered user callback.

User callbacks may be plain sync functions. To avoid running them on the
tokio worker thread (where reentering the FFI would deadlock and where long
work blocks the audio/video receive path), they're scheduled onto the asyncio
loop of the thread that registered them via `loop.call_soon_threadsafe`.
"""
from __future__ import annotations

import asyncio
import threading
from typing import Any, Callable, Dict, Optional, Tuple

from ._proto import ffi_pb2

# --- Async future registry ---------------------------------------------------

_async_lock = threading.Lock()
_async_futures: Dict[int, Tuple[asyncio.Future, asyncio.AbstractEventLoop]] = {}


def register_async(async_id: int, loop: asyncio.AbstractEventLoop) -> asyncio.Future:
    """Create a future bound to `loop`, keyed by `async_id`. Resolved when
    a matching callback event arrives."""
    fut: asyncio.Future = loop.create_future()
    with _async_lock:
        _async_futures[async_id] = (fut, loop)
    return fut


def _resolve_async(async_id: int, error: Optional[ffi_pb2.FfiError]) -> None:
    with _async_lock:
        entry = _async_futures.pop(async_id, None)
    if entry is None:
        return
    fut, loop = entry

    def _set() -> None:
        if fut.done():
            return
        if error is not None:
            fut.set_exception(PortalFfiError(error.variant, error.message))
        else:
            fut.set_result(None)

    loop.call_soon_threadsafe(_set)


class PortalFfiError(RuntimeError):
    """Exception raised when a Rust-side operation returns an error.

    `.variant` is the Rust `PortalError` variant name (e.g. `"WrongRole"`,
    `"AlreadyConnected"`). `.message` is the human-readable form.
    """

    def __init__(self, variant: str, message: str) -> None:
        super().__init__(f"{variant}: {message}")
        self.variant = variant
        self.message = message


# --- Push callback registry --------------------------------------------------

_push_lock = threading.Lock()
# key: (portal_handle, kind) where kind is "action"|"observation"|"state"|"drop"
#      or ("video", track_name).
# value: (callback, loop).
_push_callbacks: Dict[Tuple[int, Any], Tuple[Callable[..., None], asyncio.AbstractEventLoop]] = {}


def register_push(
    portal_handle: int,
    kind: Any,
    callback: Callable[..., None],
    loop: asyncio.AbstractEventLoop,
) -> None:
    with _push_lock:
        _push_callbacks[(portal_handle, kind)] = (callback, loop)


def unregister_push(portal_handle: int, kind: Any) -> None:
    with _push_lock:
        _push_callbacks.pop((portal_handle, kind), None)


def unregister_all(portal_handle: int) -> None:
    with _push_lock:
        stale = [k for k in _push_callbacks if k[0] == portal_handle]
        for k in stale:
            _push_callbacks.pop(k, None)


def _dispatch_push(portal_handle: int, kind: Any, *args: Any) -> None:
    with _push_lock:
        entry = _push_callbacks.get((portal_handle, kind))
    if entry is None:
        return
    cb, loop = entry
    loop.call_soon_threadsafe(lambda: _safely_call(cb, *args))


def _safely_call(cb: Callable[..., None], *args: Any) -> None:
    try:
        cb(*args)
    except BaseException:  # noqa: BLE001
        import traceback
        traceback.print_exc()


# --- Main dispatch -----------------------------------------------------------

def dispatch(event: ffi_pb2.FfiEvent) -> None:
    """Called from the ctypes trampoline on a tokio worker thread."""
    which = event.WhichOneof("message")
    if which == "connect":
        cb = event.connect
        _resolve_async(cb.async_id, cb.error if cb.HasField("error") else None)
    elif which == "disconnect":
        cb = event.disconnect
        _resolve_async(cb.async_id, cb.error if cb.HasField("error") else None)
    elif which == "action":
        ev = event.action
        _dispatch_push(ev.portal_handle, "action", dict(ev.values))
    elif which == "state":
        ev = event.state
        _dispatch_push(ev.portal_handle, "state", dict(ev.values))
    elif which == "observation":
        ev = event.observation
        _dispatch_push(ev.portal_handle, "observation", _build_observation(ev.observation))
    elif which == "video_frame":
        ev = event.video_frame
        _dispatch_push(
            ev.portal_handle,
            ("video", ev.track_name),
            ev.track_name,
            _build_video_frame(ev.frame),
        )
    elif which == "drop":
        ev = event.drop
        dropped = [dict(d.values) for d in ev.dropped]
        _dispatch_push(ev.portal_handle, "drop", dropped)
    # Unknown oneofs are silently ignored. forwards compatible.


def _build_observation(proto_obs: Any) -> Any:
    # Lazy import to avoid cycles with the wrapper classes.
    from . import Observation, VideoFrameData

    frames = {
        name: VideoFrameData(
            width=f.width, height=f.height, data=f.data, timestamp_us=f.timestamp_us
        )
        for name, f in proto_obs.frames.items()
    }
    return Observation(
        timestamp_us=proto_obs.timestamp_us,
        state=dict(proto_obs.state),
        frames=frames,
    )


def _build_video_frame(proto_frame: Any) -> Any:
    from . import VideoFrameData

    return VideoFrameData(
        width=proto_frame.width,
        height=proto_frame.height,
        data=proto_frame.data,
        timestamp_us=proto_frame.timestamp_us,
    )
