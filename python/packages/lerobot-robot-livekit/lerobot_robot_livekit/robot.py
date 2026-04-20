"""LiveKit Portal robot implementation.

Runs on the operator side. Opens a Portal as ``Role.OPERATOR`` and presents
the remote physical robot as a local lerobot ``Robot``. When constructed
with a local lerobot ``Teleoperator`` (e.g. a leader arm, a gamepad), it
introspects ``action_features`` to derive motor keys automatically. the
local teleop stays in the user's loop and generates the actions that
LiveKitRobot forwards over the wire.
"""
from __future__ import annotations

import asyncio
import threading
from dataclasses import dataclass
from typing import Any

from lerobot.robots.config import RobotConfig
from lerobot.robots.robot import Robot

from livekit.portal import Portal, PortalConfig, Role, i420_bytes_to_numpy_rgb


@RobotConfig.register_subclass("livekit")
@dataclass
class LiveKitRobotConfig(RobotConfig):
    url: str = ""
    token: str = ""
    session: str = ""
    fps: int = 30

    # Explicit-mode fallbacks used only when no local Teleoperator is passed.
    motors: tuple[str, ...] = ()
    camera_names: tuple[str, ...] = ()
    camera_height: int = 480
    camera_width: int = 640

    # Portal tuning.
    slack: int | None = None
    tolerance: float | None = None
    state_reliable: bool = True
    action_reliable: bool = True


class LiveKitRobot(Robot):
    """lerobot Robot that receives synced observations from a remote physical
    robot over a Portal session and publishes actions back to it.

    Construct with an optional local ``Teleoperator`` instance; its
    ``action_features`` determine the motor keys used over the wire. State
    is assumed to mirror the action schema (standard lerobot convention),
    and camera names must be provided separately — the local teleop doesn't
    know what cameras the remote robot has.

    Typical operator-side use::

        leader = MyLeaderArmTeleop(...)
        robot = LiveKitRobot(cfg, teleop=leader)
        robot.connect()
        while running:
            obs = robot.get_observation()
            action = leader.get_action()
            robot.send_action(action)
    """

    config_class = LiveKitRobotConfig
    name = "livekit"

    def __init__(
        self,
        config: LiveKitRobotConfig,
        teleop: Any | None = None,
    ) -> None:
        super().__init__(config)
        self.config = config

        self._state_keys, self._action_keys, self._cameras = self._resolve_schema(
            config, teleop
        )
        self._state_motors = [_strip_pos(k) for k in self._state_keys]
        self._action_motors = [_strip_pos(k) for k in self._action_keys]
        self._camera_names = list(self._cameras.keys())

        self._obs_features: dict = {k: float for k in self._state_keys}
        for name, shape in self._cameras.items():
            self._obs_features[name] = shape
        self._act_features: dict = {k: float for k in self._action_keys}

        self._portal: Portal | None = None
        self._portal_cfg: PortalConfig | None = None
        self._loop: asyncio.AbstractEventLoop | None = None
        self._loop_thread: threading.Thread | None = None
        self._connected = False
        self._last_observation_timestamp_us: int | None = None

    # -- lerobot interface ----------------------------------------------------

    @property
    def observation_features(self) -> dict:
        return self._obs_features

    @property
    def action_features(self) -> dict:
        return self._act_features

    @property
    def is_connected(self) -> bool:
        return self._connected

    @property
    def is_calibrated(self) -> bool:
        return True

    def calibrate(self) -> None:
        pass

    def configure(self) -> None:
        pass

    def connect(self, calibrate: bool = True) -> None:
        if self._connected:
            return
        if not self.config.url or not self.config.token:
            raise RuntimeError(
                "LiveKitRobotConfig.url and .token are required; mint a token"
                " with Role.OPERATOR grants before calling connect()."
            )

        self._start_loop()

        self._portal_cfg = PortalConfig(self.config.session or "lerobot", Role.OPERATOR)
        for cam in self._camera_names:
            self._portal_cfg.add_video(cam)
        if self._state_motors:
            self._portal_cfg.add_state(self._state_motors)
        if self._action_motors:
            self._portal_cfg.add_action(self._action_motors)
        self._portal_cfg.set_fps(self.config.fps)
        if self.config.slack is not None:
            self._portal_cfg.set_slack(self.config.slack)
        if self.config.tolerance is not None:
            self._portal_cfg.set_tolerance(self.config.tolerance)
        self._portal_cfg.set_state_reliable(self.config.state_reliable)
        self._portal_cfg.set_action_reliable(self.config.action_reliable)

        self._portal = Portal(self._portal_cfg)
        self._run(self._portal.connect(self.config.url, self.config.token))
        self._connected = True

    def disconnect(self) -> None:
        if not self._connected:
            return
        try:
            if self._portal is not None:
                self._run(self._portal.disconnect())
        finally:
            if self._portal is not None:
                self._portal.close()
                self._portal = None
            if self._portal_cfg is not None:
                self._portal_cfg.close()
                self._portal_cfg = None
            self._stop_loop()
            self._connected = False

    def get_observation(self) -> dict[str, Any]:
        """Latest synced observation from the remote robot, shaped for
        lerobot (``{motor}.pos -> float``, ``{camera} -> np.ndarray(H,W,3)``
        uint8 RGB). Empty dict until the first observation syncs.

        The sender-side timestamp of this observation is available via
        :attr:`last_observation_timestamp_us` after the call returns.
        """
        if self._portal is None:
            return {}
        obs = self._portal.get_observation()
        if obs is None:
            return {}
        self._last_observation_timestamp_us = obs.timestamp_us
        out: dict[str, Any] = {}
        for key, motor in zip(self._state_keys, self._state_motors):
            if motor in obs.state:
                out[key] = float(obs.state[motor])
        for cam in self._camera_names:
            frame = obs.frames.get(cam)
            if frame is not None:
                out[cam] = i420_bytes_to_numpy_rgb(
                    frame.data, frame.width, frame.height
                )
        return out

    @property
    def last_observation_timestamp_us(self) -> int | None:
        """Sender's system time in µs (epoch) for the most recent observation
        returned by :meth:`get_observation`, or ``None`` if none yet."""
        return self._last_observation_timestamp_us

    def send_action(self, action: dict[str, Any]) -> dict[str, Any]:
        """Publish an action to the remote robot. Returns ``action`` unchanged
        so callers can record it."""
        if self._portal is None or not self._connected:
            return action
        values: dict[str, float] = {}
        for key, motor in zip(self._action_keys, self._action_motors):
            if key in action:
                values[motor] = float(action[key])
        if values:
            self._portal.send_action(values)
        return action

    # -- schema resolution ---------------------------------------------------

    @staticmethod
    def _resolve_schema(
        config: LiveKitRobotConfig,
        teleop: Any | None,
    ) -> tuple[list[str], list[str], dict[str, tuple[int, ...]]]:
        camera_shape = (config.camera_height, config.camera_width, 3)
        cameras = {name: camera_shape for name in config.camera_names}

        if teleop is not None:
            act_features = dict(getattr(teleop, "action_features", {}))
            if not act_features:
                raise ValueError(
                    "local teleop has empty action_features; cannot infer"
                    " schema"
                )
            action_keys = sorted(act_features.keys())
            # lerobot convention: observation mirrors action for telemetry
            # (each commanded motor reports its actual position back).
            state_keys = list(action_keys)
            return state_keys, action_keys, cameras

        if config.motors or config.camera_names:
            state_keys = sorted(f"{m}.pos" for m in config.motors)
            action_keys = list(state_keys)
            return state_keys, action_keys, cameras

        raise ValueError(
            "LiveKitRobot needs either a local Teleoperator instance or"
            " config.motors / config.camera_names to derive its schema"
        )

    # -- background loop plumbing --------------------------------------------

    def _start_loop(self) -> None:
        self._loop = asyncio.new_event_loop()
        started = threading.Event()

        def _runner() -> None:
            asyncio.set_event_loop(self._loop)
            started.set()
            self._loop.run_forever()

        self._loop_thread = threading.Thread(
            target=_runner, name="livekit-portal-loop", daemon=True
        )
        self._loop_thread.start()
        started.wait()

    def _stop_loop(self) -> None:
        if self._loop is None:
            return
        self._loop.call_soon_threadsafe(self._loop.stop)
        if self._loop_thread is not None:
            self._loop_thread.join(timeout=5.0)
        self._loop.close()
        self._loop = None
        self._loop_thread = None

    def _run(self, coro):
        assert self._loop is not None, "background loop not started"
        return asyncio.run_coroutine_threadsafe(coro, self._loop).result()


def _strip_pos(key: str) -> str:
    return key[: -len(".pos")] if key.endswith(".pos") else key
