"""Run on the **operator** side.

Drives a local SO-101 leader arm and presents the remote SO-101 follower
as a local lerobot ``Robot`` over a LiveKit Portal session as
``Role.OPERATOR``. Each tick: read leader pose, push as action; pull
synced observation back (joint positions + camera frame) and stream it
to a rerun viewer.

Usage:
    cp .env.example .env  # fill in API_KEY / API_SECRET / serial port
    uv run teleoperator.py
"""
from __future__ import annotations

import numpy as np
import rerun as rr
from lerobot.teleoperators.so_leader import SO101Leader, SO101LeaderConfig
from lerobot.utils.visualization_utils import init_rerun
from lerobot_robot_livekit import LiveKitRobot, LiveKitRobotConfig

from _common import env_int, env_str, load_env, mint_token, pace, required_env

IDENTITY = "so101-operator"


def log_rerun(namespace: str, data: dict) -> None:
    """Log a lerobot-shaped dict (motor floats + camera ndarrays) under `namespace`.

    Images are JPEG-compressed so memory stays bounded even with scrubbable
    history retained; scalars are logged as rerun scalar series.
    """
    for k, v in data.items():
        entity = f"{namespace}.{k}"
        if isinstance(v, np.ndarray):
            rr.log(entity, rr.Image(v).compress())
        else:
            rr.log(entity, rr.Scalars(float(v)))


def main() -> None:
    load_env()
    url = required_env("LIVEKIT_URL")
    room = required_env("LIVEKIT_ROOM")
    fps = env_int("PORTAL_FPS", 30)
    camera_name = required_env("SO101_CAMERA_NAME")

    # Leader = local physical arm producing actions.
    # LiveKitRobot = remote follower dressed up as a local lerobot Robot, so
    # send_action() goes over the wire and get_observation() returns the
    # remote's synced joint state + camera frames.
    leader = SO101Leader(SO101LeaderConfig(
        id=env_str("SO101_LEADER_ID", "so101_leader"),
        port=required_env("SO101_LEADER_PORT"),
    ))
    robot = LiveKitRobot(LiveKitRobotConfig(
        url=url,
        token=mint_token(IDENTITY, room),
        session=room,
        fps=fps,
        camera_names=(camera_name,),
        camera_width=env_int("SO101_CAMERA_WIDTH", 640),
        camera_height=env_int("SO101_CAMERA_HEIGHT", 480),
    ), teleop=leader)

    leader.connect()
    robot.connect()
    init_rerun(session_name=f"so101-{room}")  # spawns the rerun viewer
    print(f"[operator] '{IDENTITY}' in '{room}' @ {fps} fps; ctrl-c to stop")

    try:
        for _ in pace(fps):
            # Send action first so control latency never waits on rerun logging.
            if action := leader.get_action():
                robot.send_action(action)

            obs = robot.get_observation()

            # Anchor rerun's timeline to the sender's wall clock so scrubbing
            # reflects what happened on the physical robot, not receive time.
            if ts_us := robot.last_observation_timestamp_us:
                rr.set_time("robot_time", timestamp=ts_us / 1e6)

            log_rerun("observation", obs or {})
            log_rerun("action", action or {})
    except KeyboardInterrupt:
        print("\n[operator] stopping ...")
    finally:
        robot.disconnect()
        leader.disconnect()


if __name__ == "__main__":
    main()
