<a href="https://livekit.io/">
  <img src=".github/assets/livekit-mark.png" alt="LiveKit logo" width="100" height="100">
</a>

# livekit-portal

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)](https://www.python.org/downloads/)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org/)

<!--BEGIN_DESCRIPTION-->
Teleoperate a robot, or run a policy against it, from anywhere on the internet. Portal carries cameras, joint state, and actions between a robot host and a control host over LiveKit. On the control side, everything arrives as synchronized `(frames, state, timestamp)` observations. Works with any robotics stack. An optional [LeRobot](https://github.com/huggingface/lerobot) plugin adds a one-line wrap for lerobot users.
<!--END_DESCRIPTION-->

## Your code does not change

A local lerobot loop looks like this:

```python
from lerobot.robots.myrobot import MyRobot

robot = MyRobot(config=...)
robot.connect()

obs = robot.get_observation()
action = model.select_action(obs)
robot.send_action(action)
```

Swap one import and you get the same loop driving a robot on another
machine, over the internet:

```python
from lerobot_robot_livekit import LiveKitRobot, LiveKitRobotConfig

robot = LiveKitRobot(LiveKitRobotConfig(url=..., token=..., fps=30,
                                        camera_names=("cam1",)))
robot.connect()

obs = robot.get_observation()
action = model.select_action(obs)
robot.send_action(action)
```

Same three lines at the bottom. The robot can be in another room or
another country. `obs` still arrives as a bundled, timestamp-synced
observation.

The example above uses the lerobot plugin, because it gives the cleanest
apples-to-apples comparison. Portal itself is a standalone library. The
plugin is a thin, optional wrap on top of it. See the
[30-second sketch](#30-second-sketch) for the same idea in the raw Portal
API.

## The idea

Think of your robot as a device that normally plugs into one computer.
Portal lets it plug into a different one over the network. Your teleop
interface, your training loop, or your policy server can run anywhere and
still see the robot as if it were local. The physical robot stays in the
loop. Portal only adds the network tier.

You use Portal by wrapping whatever code already drives your robot. On the
robot host, you publish frames and state through a `Portal` object. On the
control host, you receive them as a bundled `Observation(frames, state,
timestamp_us)` and publish actions back. No framework is assumed.

If you happen to be on [lerobot](https://github.com/huggingface/lerobot),
two optional plugin packages collapse that to a one-line wrap around your
existing `Robot` or `Teleoperator`. See
[lerobot integration](docs/lerobot.md).

## I want to…

| Goal | Start here |
|---|---|
| **Wire Portal into my own robotics stack** | [Quickstart](docs/quickstart.md) and [Portal API](docs/portal-api.md) |
| **Run a policy against a remote robot** | [Quickstart](docs/quickstart.md). The policy sits on the control side and consumes `Observation`s. |
| **See a working end-to-end example with no hardware** | [`examples/python/basic/`](examples/python/basic) |
| **Shortcut for lerobot users** | [lerobot integration](docs/lerobot.md) |

## Examples

Running examples is the fastest way to a known-good setup. Both live under
[`examples/python/`](examples/python).

**[`examples/python/basic/`](examples/python/basic)**

No hardware required. Uses the Portal API directly. Synthetic video and
state on one terminal, subscriber on another. Proves your LiveKit
credentials and native build are healthy.

```bash
cd examples/python/basic
cp .env.example .env            # fill in LIVEKIT_URL / API_KEY / API_SECRET
uv sync
uv run robot.py                 # terminal 1
uv run teleoperator.py          # terminal 2
```

**[`examples/python/so101/`](examples/python/so101)**

Real hardware. Uses the lerobot plugin. A physical **SO-101 follower** is
driven by a remote **SO-101 leader**. Camera and joint state render in
[rerun](https://rerun.io). Full calibration and wiring walkthrough in its
[README](examples/python/so101/README.md).

Adapt either to bring up your own setup.

## 30-second sketch

Robot side. Publishes frames and state. Receives actions.

```python
from livekit.portal import Portal, PortalConfig, Role

cfg = PortalConfig("session-1", Role.ROBOT)
cfg.add_video("cam1")
cfg.add_state(["j1", "j2", "j3"])
cfg.add_action(["j1", "j2", "j3"])

portal = Portal(cfg)
portal.on_action(lambda a: robot.send_action(a.values))
await portal.connect(url, token)

while running:
    obs = robot.get_observation()
    portal.send_video_frame("cam1", obs.image, width, height)
    portal.send_state(obs.state)
    await asyncio.sleep(1 / 30)
```

Control side. Receives synced observations. Publishes actions.

```python
from livekit.portal import Portal, PortalConfig, Role

cfg = PortalConfig("session-1", Role.OPERATOR)
cfg.add_video("cam1")
cfg.add_state(["j1", "j2", "j3"])
cfg.add_action(["j1", "j2", "j3"])

portal = Portal(cfg)

def on_observation(obs):
    # obs.frames: dict[str, np.ndarray]
    # obs.state:  dict[str, float]
    # obs.timestamp_us: int
    portal.send_action(policy(obs))          # or teleop.get_action()

portal.on_observation(on_observation)
await portal.connect(url, token)
```

Full runnable version, including token minting, in the
[Quickstart](docs/quickstart.md).

## Install

```bash
uv pip install livekit-portal
# or
pip install livekit-portal
```

For local development, build the native library once:

```bash
cd python/packages/livekit-portal
uv sync
bash scripts/build_native.sh release
```

## Documentation

- [Quickstart](docs/quickstart.md). Install, tokens, first run with the
  Portal API.
- [Portal API](docs/portal-api.md). The primary surface. `PortalConfig`,
  callbacks, send methods, role semantics.
- [Concepts](docs/concepts.md). Roles, observation model, frame format.
- [Tuning](docs/tuning.md). `fps`, `slack`, `tolerance`, asymmetric rates,
  reliability.
- [RPC](docs/rpc.md). Imperative commands (`home`, `calibrate`, etc.) on
  top of LiveKit RPC.
- [Synchronization deep dive](docs/synchronization.md). The full match
  algorithm, cursor bookkeeping, complexity analysis.
- [lerobot integration](docs/lerobot.md). The optional convenience plugins.

## License

Apache-2.0. See [LICENSE](LICENSE) for details.
