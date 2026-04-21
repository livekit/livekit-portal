# Quickstart

Get a robot and an operator talking over LiveKit in ~5 minutes.

This page uses the **lerobot plugin path** — the shortest route. If your stack
isn't lerobot-based, jump to the raw [Portal API reference](portal-api.md) once
you're through this page.

## What you need

- Python 3.12+ and [`uv`](https://docs.astral.sh/uv/)
- A LiveKit server: [LiveKit Cloud](https://cloud.livekit.io) (free tier works) or
  a local `livekit-server --dev`
- Your `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`

You do **not** need a physical robot to try this — the first example below
publishes a synthetic test pattern.

## 1. Install

```bash
uv pip install livekit-portal
```

For local development from this repo:

```bash
cd python/packages/livekit-portal
uv sync
bash scripts/build_native.sh release
```

## 2. Mint tokens

Both sides need a JWT for the same LiveKit room. Minimal helper:

```python
import datetime
from livekit import api
from livekit.protocol.room import RoomConfiguration

def mint(identity: str, room: str, key: str, secret: str) -> str:
    grants = api.VideoGrants(
        room_join=True, room=room, can_publish=True, can_subscribe=True,
    )
    return (
        api.AccessToken(key, secret)
        .with_identity(identity)
        .with_grants(grants)
        # tight playout delay bounds minimize teleop latency
        .with_room_config(
            RoomConfiguration(name=room, min_playout_delay=0, max_playout_delay=1)
        )
        .with_ttl(datetime.timedelta(hours=6))
        .to_jwt()
    )
```

Identities must be unique within the room (e.g. `"robot"`, `"operator"`).

## 3. Run it — lerobot plugin path

Two scripts, one per machine. Both connect to the same room name.

### Robot side (next to the hardware)

Wrap your existing lerobot `Robot`:

```python
from lerobot.robots.so100 import SO100Robot, SO100RobotConfig   # or your class
from lerobot_teleoperator_livekit import (
    LiveKitTeleoperator, LiveKitTeleoperatorConfig,
)

robot = SO100Robot(SO100RobotConfig(...))
robot.connect()

teleop = LiveKitTeleoperator(
    LiveKitTeleoperatorConfig(
        url="wss://your-project.livekit.cloud",
        token=mint("robot", "session-1", API_KEY, API_SECRET),
        session="session-1",
        fps=30,
    ),
    robot=robot,                     # schema inferred from the robot
)
teleop.connect()

try:
    while running:
        obs = robot.get_observation()
        teleop.send_feedback(obs)    # goes over the wire
        action = teleop.get_action() # latest action from operator (may be {})
        if action:
            robot.send_action(action)
        sleep(1 / 30)
finally:
    teleop.disconnect()
    robot.disconnect()
```

### Operator side (workstation, trainer, policy host)

Wrap your local `Teleoperator` (leader arm, gamepad, or policy output):

```python
from lerobot.teleoperators.leader import LeaderArmTeleop, LeaderArmTeleopConfig
from lerobot_robot_livekit import LiveKitRobot, LiveKitRobotConfig

leader = LeaderArmTeleop(LeaderArmTeleopConfig(...))
leader.connect()

robot = LiveKitRobot(
    LiveKitRobotConfig(
        url="wss://your-project.livekit.cloud",
        token=mint("operator", "session-1", API_KEY, API_SECRET),
        session="session-1",
        fps=30,
        camera_names=("cam1",),
        camera_height=480, camera_width=640,
    ),
    teleop=leader,                   # schema inferred from leader
)
robot.connect()

try:
    while running:
        obs = robot.get_observation()   # synced {cameras, state, timestamp}
        action = leader.get_action()    # or: policy(obs)
        robot.send_action(action)
        sleep(1 / 30)
finally:
    robot.disconnect()
    leader.disconnect()
```

The remote physical robot is now a local lerobot `Robot` to any downstream
workflow — teleoperation, dataset recording, policy eval — none of it needs to
know it's remote.

## 4. Try the runnable examples first

Before wiring Portal into your real stack, run the shipped examples:

- [`examples/python/basic/`](../examples/python/basic) — no hardware needed.
  Synthetic video + state on one terminal, subscriber on another. Ten-minute
  sanity check that your LiveKit credentials and native build are good.
- [`examples/python/so101/`](../examples/python/so101) — real hardware. Drive a
  physical SO-101 follower from a remote SO-101 leader, with the camera and
  joint state rendered in [rerun](https://rerun.io). Full calibration + wiring
  walkthrough in its [README](../examples/python/so101/README.md).

The examples are the fastest path to a known-good setup you can adapt.

## Running a VLA policy instead of teleop

Same setup — the operator side doesn't care *what* produces the action:

```python
while running:
    obs = robot.get_observation()
    action = policy(obs.frames, obs.state)   # or policy.select_action(obs)
    robot.send_action(action)
```

## Next steps

- [Concepts](concepts.md) — roles, the observation model, frame format.
- [Tuning](tuning.md) — `fps`, `slack`, `tolerance`, asymmetric rates, reliability.
- [lerobot integration](lerobot.md) — full plugin config reference, CLI mode,
  schema inference rules, troubleshooting.
- [Portal API](portal-api.md) — the raw API, for stacks that aren't lerobot.
