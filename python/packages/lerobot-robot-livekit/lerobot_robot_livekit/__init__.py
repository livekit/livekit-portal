"""LiveKit Portal robot plugin for lerobot.

Deployed on the **operator side**. Makes a remote physical robot appear as a
local ``Robot`` to any lerobot workflow (teleoperation, data recording,
policy evaluation). Importing this module registers ``LiveKitRobot`` as
``--robot.type=livekit``.
"""
from __future__ import annotations

from .robot import LiveKitRobot, LiveKitRobotConfig

__all__ = ["LiveKitRobot", "LiveKitRobotConfig"]
