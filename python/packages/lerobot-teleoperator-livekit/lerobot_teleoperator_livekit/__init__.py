"""LiveKit Portal teleoperator plugin for lerobot.

Deployed on the **robot side**. Wraps `livekit.portal.Portal` in `Role.ROBOT`
so lerobot can drive a remote physical robot by running a teleop loop that
pushes actions over LiveKit. Importing this module registers
``LiveKitTeleoperator`` as ``--teleop.type=livekit``.
"""
from __future__ import annotations

from .teleoperator import LiveKitTeleoperator, LiveKitTeleoperatorConfig

__all__ = ["LiveKitTeleoperator", "LiveKitTeleoperatorConfig"]
