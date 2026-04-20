"""livekit-portal. Python bindings.

Public surface:
  - `Role`. enum (ROBOT, OPERATOR)
  - `PortalConfig`. builder; construct, configure, then hand to Portal()
  - `Portal`. main object; `await connect/disconnect`, send/get, on_* callbacks
  - `Observation`, `Action`, `State`, `VideoFrameData`. dataclasses returned by
    callbacks and get_*
  - `PortalError`. exception type raised for Rust-side errors

Frame formats: sends take RGB24 (bytes or `np.ndarray(H, W, 3)` uint8);
receives deliver I420 (planar Y+U+V concatenated). Use
`livekit.portal.i420_bytes_to_numpy_rgb` to convert received frames for
display.
"""
from __future__ import annotations

import asyncio
import enum
import weakref
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional

from . import _events, _ffi, _frame
from ._events import PortalFfiError as PortalError
from ._frame import i420_bytes_to_numpy_rgb
from ._proto import ffi_pb2, portal_pb2, types_pb2


class Role(enum.IntEnum):
    ROBOT = types_pb2.ROBOT
    OPERATOR = types_pb2.OPERATOR


@dataclass(frozen=True)
class VideoFrameData:
    """A decoded video frame.

    `data` is I420 planar bytes on the receive side. Width/height are in
    pixels. `timestamp_us` is the sender's system time in microseconds.
    """

    width: int
    height: int
    data: bytes
    timestamp_us: int


@dataclass(frozen=True)
class Observation:
    """A synchronized observation: one state matched with one frame from
    every registered video track, all aligned to `timestamp_us`."""

    timestamp_us: int
    state: Dict[str, float]
    frames: Dict[str, VideoFrameData] = field(default_factory=dict)


@dataclass(frozen=True)
class Action:
    """An action received from the operator (Robot side).

    `timestamp_us` is the sender's wall-clock time in microseconds.
    """

    values: Dict[str, float]
    timestamp_us: int


@dataclass(frozen=True)
class State:
    """A state received from the robot (Operator side, un-synced path).

    For synchronized state matched with frames, use `Observation` instead.
    `timestamp_us` is the sender's wall-clock time in microseconds.
    """

    values: Dict[str, float]
    timestamp_us: int


# --- internal request builders ----------------------------------------------

def _call(resp_field: str, message: Any) -> Any:
    """Send an FfiRequest whose oneof variant is `resp_field`, return the
    matching FfiResponse variant. Raises if variant mismatch."""
    req = ffi_pb2.FfiRequest()
    getattr(req, resp_field).CopyFrom(message)
    resp = _ffi.request(req)
    if resp.WhichOneof("message") != resp_field:
        raise RuntimeError(
            f"ffi response variant mismatch: expected {resp_field}, got {resp.WhichOneof('message')}"
        )
    return getattr(resp, resp_field)


def _raise_if_error(inner: Any) -> None:
    if inner.HasField("error"):
        e = inner.error
        raise PortalError(e.variant, e.message)


# --- PortalConfig ------------------------------------------------------------

class PortalConfig:
    """Builder for a Portal session.

    Matches `livekit_portal::PortalConfig`. every setter translates to an FFI
    request so the Rust-side config reflects the full declared state by the
    time `Portal(config)` is constructed.
    """

    __slots__ = (
        "_handle",
        "_session",
        "_role",
        "_video_tracks",
        "_state_fields",
        "_action_fields",
        "_finalizer",
        "__weakref__",
    )

    def __init__(self, session: str, role: Role) -> None:
        resp = _call(
            "new_config",
            portal_pb2.NewPortalConfigRequest(session=session, role=int(role)),
        )
        self._handle: int = resp.handle.id
        self._session = session
        self._role = role
        self._video_tracks: List[str] = []
        self._state_fields: List[str] = []
        self._action_fields: List[str] = []
        self._finalizer = weakref.finalize(self, _dispose_handle, self._handle)

    @property
    def handle(self) -> int:
        return self._handle

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
        _call(
            "config_add_video",
            portal_pb2.ConfigAddVideoRequest(config_handle=self._handle, name=name),
        )
        self._video_tracks.append(name)

    def add_state(self, fields: List[str]) -> None:
        _call(
            "config_add_state",
            portal_pb2.ConfigAddStateRequest(config_handle=self._handle, fields=fields),
        )
        self._state_fields.extend(fields)

    def add_action(self, fields: List[str]) -> None:
        _call(
            "config_add_action",
            portal_pb2.ConfigAddActionRequest(config_handle=self._handle, fields=fields),
        )
        self._action_fields.extend(fields)

    def set_fps(self, fps: int) -> None:
        _call(
            "config_set_fps",
            portal_pb2.ConfigSetFpsRequest(config_handle=self._handle, fps=fps),
        )

    def set_slack(self, ticks: int) -> None:
        _call(
            "config_set_slack",
            portal_pb2.ConfigSetSlackRequest(config_handle=self._handle, ticks=ticks),
        )

    def set_tolerance(self, ticks: float) -> None:
        _call(
            "config_set_tolerance",
            portal_pb2.ConfigSetToleranceRequest(config_handle=self._handle, ticks=ticks),
        )

    def set_state_reliable(self, reliable: bool) -> None:
        _call(
            "config_set_state_reliable",
            portal_pb2.ConfigSetStateReliableRequest(
                config_handle=self._handle, reliable=reliable
            ),
        )

    def set_action_reliable(self, reliable: bool) -> None:
        _call(
            "config_set_action_reliable",
            portal_pb2.ConfigSetActionReliableRequest(
                config_handle=self._handle, reliable=reliable
            ),
        )

    def set_ping_ms(self, ms: int) -> None:
        _call(
            "config_set_ping_ms",
            portal_pb2.ConfigSetPingMsRequest(config_handle=self._handle, ms=ms),
        )

    def close(self) -> None:
        """Release the Rust-side config handle. Normally called by GC; call
        explicitly if you need deterministic release."""
        if self._finalizer.alive:
            self._finalizer()


# --- Portal ------------------------------------------------------------------

class Portal:
    """Main session object.

    Construct with a `PortalConfig`, then `await connect(url, token)`. The
    config's state_fields / action_fields / video_tracks are captured at
    construction and used to decode events.
    """

    __slots__ = (
        "_handle",
        "_state_fields",
        "_action_fields",
        "_video_tracks",
        "_finalizer",
        "__weakref__",
    )

    def __init__(self, config: PortalConfig) -> None:
        resp = _call(
            "new_portal",
            portal_pb2.NewPortalRequest(config_handle=config.handle),
        )
        self._handle: int = resp.handle.id
        self._state_fields: List[str] = list(resp.state_fields)
        self._action_fields: List[str] = list(resp.action_fields)
        self._video_tracks: List[str] = list(resp.video_tracks)
        self._finalizer = weakref.finalize(self, _finalize_portal, self._handle)

    @property
    def handle(self) -> int:
        return self._handle

    # -- async lifecycle -----------------------------------------------------

    async def connect(self, url: str, token: str) -> None:
        loop = asyncio.get_running_loop()
        resp = _call(
            "connect",
            portal_pb2.ConnectRequest(portal_handle=self._handle, url=url, token=token),
        )
        fut = _events.register_async(resp.async_id, loop)
        await fut

    async def disconnect(self) -> None:
        loop = asyncio.get_running_loop()
        resp = _call("disconnect", portal_pb2.DisconnectRequest(portal_handle=self._handle))
        fut = _events.register_async(resp.async_id, loop)
        await fut

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
        req = portal_pb2.SendVideoFrameRequest(
            portal_handle=self._handle,
            track_name=track_name,
            rgb_data=rgb,
            width=w,
            height=h,
        )
        if timestamp_us is not None:
            req.timestamp_us = timestamp_us
        resp = _call("send_video_frame", req)
        _raise_if_error(resp)

    def send_state(
        self,
        values: Dict[str, float],
        timestamp_us: Optional[int] = None,
    ) -> None:
        req = portal_pb2.SendStateRequest(portal_handle=self._handle, values=values)
        if timestamp_us is not None:
            req.timestamp_us = timestamp_us
        resp = _call("send_state", req)
        _raise_if_error(resp)

    def send_action(
        self,
        values: Dict[str, float],
        timestamp_us: Optional[int] = None,
    ) -> None:
        req = portal_pb2.SendActionRequest(portal_handle=self._handle, values=values)
        if timestamp_us is not None:
            req.timestamp_us = timestamp_us
        resp = _call("send_action", req)
        _raise_if_error(resp)

    # -- pull (sync, latest-wins) --------------------------------------------

    def get_observation(self) -> Optional[Observation]:
        resp = _call("get_observation", portal_pb2.GetObservationRequest(portal_handle=self._handle))
        if not resp.HasField("observation"):
            return None
        return _build_observation(resp.observation)

    def get_action(self) -> Optional[Action]:
        resp = _call("get_action", portal_pb2.GetActionRequest(portal_handle=self._handle))
        if not resp.present:
            return None
        return Action(values=dict(resp.values), timestamp_us=resp.timestamp_us)

    def get_state(self) -> Optional[State]:
        resp = _call("get_state", portal_pb2.GetStateRequest(portal_handle=self._handle))
        if not resp.present:
            return None
        return State(values=dict(resp.values), timestamp_us=resp.timestamp_us)

    def get_video_frame(self, track_name: str) -> Optional[VideoFrameData]:
        resp = _call(
            "get_video_frame",
            portal_pb2.GetVideoFrameRequest(portal_handle=self._handle, track_name=track_name),
        )
        if not resp.HasField("frame"):
            return None
        return _build_video_frame(resp.frame)

    # -- push callbacks ------------------------------------------------------

    def on_action(self, callback: Callable[[Action], None]) -> None:
        _events.register_push(
            self._handle, "action", callback, asyncio.get_event_loop()
        )

    def on_state(self, callback: Callable[[State], None]) -> None:
        _events.register_push(
            self._handle, "state", callback, asyncio.get_event_loop()
        )

    def on_observation(self, callback: Callable[[Observation], None]) -> None:
        _events.register_push(
            self._handle, "observation", callback, asyncio.get_event_loop()
        )

    def on_video_frame(
        self,
        track_name: str,
        callback: Callable[[str, VideoFrameData], None],
    ) -> None:
        _events.register_push(
            self._handle, ("video", track_name), callback, asyncio.get_event_loop()
        )

    def on_drop(self, callback: Callable[[List[Dict[str, float]]], None]) -> None:
        _events.register_push(
            self._handle, "drop", callback, asyncio.get_event_loop()
        )

    # -- metrics -------------------------------------------------------------

    def metrics(self) -> types_pb2.PortalMetrics:
        resp = _call("metrics", portal_pb2.MetricsRequest(portal_handle=self._handle))
        return resp.metrics

    def reset_metrics(self) -> None:
        _call("reset_metrics", portal_pb2.ResetMetricsRequest(portal_handle=self._handle))

    # -- cleanup -------------------------------------------------------------

    def close(self) -> None:
        if self._finalizer.alive:
            self._finalizer()


# --- helpers ----------------------------------------------------------------

def _build_video_frame(proto_frame: Any) -> VideoFrameData:
    return VideoFrameData(
        width=proto_frame.width,
        height=proto_frame.height,
        data=proto_frame.data,
        timestamp_us=proto_frame.timestamp_us,
    )


def _build_observation(proto_obs: Any) -> Observation:
    frames = {
        name: _build_video_frame(f) for name, f in proto_obs.frames.items()
    }
    return Observation(
        timestamp_us=proto_obs.timestamp_us,
        state=dict(proto_obs.state),
        frames=frames,
    )


def _dispose_handle(handle: int) -> None:
    try:
        req = ffi_pb2.FfiRequest()
        req.dispose_handle.handle = handle
        _ffi.request(req)
    except Exception:
        pass


def _finalize_portal(handle: int) -> None:
    _events.unregister_all(handle)
    _dispose_handle(handle)


__all__ = [
    "Role",
    "PortalConfig",
    "Portal",
    "Observation",
    "Action",
    "State",
    "VideoFrameData",
    "PortalError",
    "i420_bytes_to_numpy_rgb",
]
