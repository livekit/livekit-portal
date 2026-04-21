# Quickstart

Get a robot host and a control host talking over LiveKit in about 5 minutes
using the Portal API directly.

If you are already on lerobot, there is a one-line shortcut at the bottom of
this page that wraps the same code.

## What you need

- Python 3.10+ and [`uv`](https://docs.astral.sh/uv/)
- A LiveKit server: [LiveKit Cloud](https://cloud.livekit.io) (free tier
  works) or a local `livekit-server --dev`
- Your `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`

You do **not** need a physical robot to try this. The first example publishes
a synthetic test pattern.

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

## 3. Robot host

Runs next to the hardware. Declares what it will publish (video tracks,
state fields) and what it will receive (action fields), then pumps frames
and state at your capture rate.

```python
import asyncio, time
from livekit.portal import Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session-1", Role.ROBOT)
    cfg.add_video("cam1")
    cfg.add_state(["j1", "j2", "j3"])
    cfg.add_action(["j1", "j2", "j3"])
    cfg.set_fps(30)

    portal = Portal(cfg)

    def on_action(a):
        # a.values is the action dict; a.timestamp_us is the sender's clock
        robot.send_action(a.values)

    portal.on_action(on_action)
    await portal.connect(URL, mint("robot", "session-1", API_KEY, API_SECRET))

    while running:
        obs = robot.get_observation()
        ts = int(time.time() * 1_000_000)
        portal.send_video_frame("cam1", obs.image, width, height, timestamp_us=ts)
        portal.send_state(obs.state, timestamp_us=ts)
        await asyncio.sleep(1 / 30)

asyncio.run(main())
```

`obs.image` must be a NumPy `uint8` array of shape `(H, W, 3)` in RGB.

## 4. Control host

Runs wherever your operator, trainer, or policy lives. Declares the same
schema, then consumes synchronized observations and publishes actions.

```python
import asyncio
from livekit.portal import Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session-1", Role.OPERATOR)
    cfg.add_video("cam1")
    cfg.add_state(["j1", "j2", "j3"])
    cfg.add_action(["j1", "j2", "j3"])
    cfg.set_fps(30)

    portal = Portal(cfg)

    def on_observation(obs):
        # obs.frames: dict[str, np.ndarray]      # one per registered video track
        # obs.state:  dict[str, float]
        # obs.timestamp_us: int                  # sender clock
        action = policy(obs)                     # or teleop.get_action(), etc.
        portal.send_action(action)

    portal.on_observation(on_observation)
    await portal.connect(URL, mint("operator", "session-1", API_KEY, API_SECRET))

    while running:
        await asyncio.sleep(1)

asyncio.run(main())
```

`policy(obs)` here is any function that turns an observation into an
action dict. Teleoperation, imitation learning, VLA inference, a hand-written
P controller: Portal does not care.

## 5. Try the shipped examples

Before wiring Portal into your real stack, run the basic example. It uses
the exact API above, with synthetic video and a token minter already wired
up.

- [`examples/python/basic/`](../examples/python/basic): no hardware needed.
  Ten-minute sanity check that your LiveKit credentials and native build
  work.
- [`examples/python/so101/`](../examples/python/so101): real hardware. Drive
  a physical SO-101 follower from a remote SO-101 leader, with the camera
  and joint state rendered in [rerun](https://rerun.io). Uses the lerobot
  plugin shortcut (see below). Full calibration + wiring walkthrough in its
  [README](../examples/python/so101/README.md).

## Shortcut: lerobot users

If your robot and control code already use the
[lerobot](https://github.com/huggingface/lerobot) `Robot` / `Teleoperator`
interfaces, two optional plugin packages wrap the Portal code above so you
don't have to write it yourself.

```python
# robot host: wraps a local lerobot Robot
from lerobot_teleoperator_livekit import LiveKitTeleoperator, LiveKitTeleoperatorConfig

teleop = LiveKitTeleoperator(
    LiveKitTeleoperatorConfig(url=URL, token=token, session="session-1", fps=30),
    robot=my_robot,
)
teleop.connect()
```

```python
# control host: wraps a local lerobot Teleoperator (or a policy)
from lerobot_robot_livekit import LiveKitRobot, LiveKitRobotConfig

robot = LiveKitRobot(
    LiveKitRobotConfig(
        url=URL, token=token, session="session-1", fps=30,
        camera_names=("cam1",), camera_height=480, camera_width=640,
    ),
    teleop=my_leader,
)
robot.connect()
```

The plugins are syntactic sugar over the Portal API above. Full reference
and CLI mode: [lerobot integration](lerobot.md).

## Next steps

- [Portal API](portal-api.md): the full surface. All callbacks, send
  methods, role semantics.
- [Concepts](concepts.md): roles, the observation model, frame format.
- [Tuning](tuning.md): `fps`, `slack`, `tolerance`, asymmetric rates,
  reliability.
