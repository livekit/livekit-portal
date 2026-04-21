<p align="center">
  <a href="https://livekit.io/">
    <img src=".github/assets/livekit-mark.png" alt="LiveKit logo" width="100" height="100">
  </a>
</p>

<h1 align="center">livekit-portal</h1>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License"></a>
  <a href="https://www.python.org/downloads/"><img src="https://img.shields.io/badge/python-3.10%2B-blue" alt="Python 3.10+"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-stable-orange" alt="Rust"></a>
</p>

<!--BEGIN_DESCRIPTION-->
<p align="center"><b>Teleoperate a robot, or run a policy against it, from anywhere on the internet.</b> Portal carries cameras, joint state, and actions between a robot host and a control host over LiveKit. On the control side, everything arrives as synchronized <code>(frames, state, timestamp)</code> observations. Works with any robotics stack. An optional <a href="https://github.com/huggingface/lerobot">LeRobot</a> plugin adds a one-line drop-in for lerobot users.</p>
<!--END_DESCRIPTION-->

<p align="center">
  <a href="#your-code-does-not-change">Show me</a> ·
  <a href="#install">Install</a> ·
  <a href="#examples">Examples</a> ·
  <a href="docs/quickstart.md">Quickstart</a> ·
  <a href="docs/portal-api.md">Portal API</a> ·
  <a href="docs/concepts.md">Concepts</a> ·
  <a href="docs/synchronization.md">Deep dive</a>
</p>

---

## Features

**Remote robot, same code.** Your robot loop keeps its shape. Portal moves the hardware to another machine. Your policy or teleop code still sees a local-looking `Robot` object.

**Synced observations out of the box.** Cameras and joint state arrive fused into `Observation(frames, state, timestamp_us)`. That is the shape robotics policies already consume. No matching logic on your side.

**Works with any stack.** A direct `Portal` API in Python and Rust. An optional [lerobot](https://github.com/huggingface/lerobot) plugin for a one-line wrap around your existing `Robot` or `Teleoperator`.

**Low-latency transport.** WebRTC video (SIMD RGB→I420). SCTP data channels with reliable or unreliable delivery per stream. RPC for one-shots like `home` or `calibrate`. Rust core, Python bindings via UniFFI.

---

## Your code does not change

A classical lerobot loop runs on the same machine as the robot:

```python
# all on one machine, robot plugged in
from lerobot.robots.myrobot import MyRobot

robot = MyRobot(config=...)
robot.connect()

obs = robot.get_observation()
action = model.select_action(obs)
robot.send_action(action)
```

With Portal, the same loop splits into two small files. One lives next to the
robot. The other lives wherever your policy or teleop runs.

**`robot_host.py`** runs on the machine the robot is plugged into.

```python
from lerobot.robots.myrobot import MyRobot
from lerobot_teleoperator_livekit import (
    LiveKitTeleoperator, LiveKitTeleoperatorConfig,
)

robot = MyRobot(config=...)
robot.connect()

teleop = LiveKitTeleoperator(
    LiveKitTeleoperatorConfig(url=..., token=..., session="session-1", fps=30),
    robot=robot,
)
teleop.connect()

while running:
    teleop.send_feedback(robot.get_observation())     # upstream to operator
    if action := teleop.get_action():                 # action from operator
        robot.send_action(action)
```

**`control_host.py`** runs wherever you drive the robot from.

```python
from lerobot_robot_livekit import LiveKitRobot, LiveKitRobotConfig

robot = LiveKitRobot(LiveKitRobotConfig(
    url=..., token=..., session="session-1", fps=30,
    camera_names=("cam1",),
))
robot.connect()

obs = robot.get_observation()
action = model.select_action(obs)
robot.send_action(action)
```

The last three lines of `control_host.py` are the same three lines as the
classical loop. The robot just lives on another machine now.

The example above uses the lerobot plugin because it makes the diff a single
import. Portal itself is a standalone library. The plugin is a thin wrap on
top. See the [30-second sketch](#30-second-sketch) for the raw Portal API.

## The idea

Think of your robot as a device that normally plugs into one computer.
Portal lets it plug into a different one over the network. Your teleop
interface, your training loop, or your policy server can run anywhere and
still see the robot as if it were local.

You use Portal by wrapping whatever code already drives your robot. On the
robot host, you publish frames and state through a `Portal` object. On the
control host, you receive them as a bundled `Observation` and publish actions
back. No framework is assumed.

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

## 30-second sketch

The raw Portal API, no plugin. This is what the lerobot wrappers use
internally.

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
    # obs.frames:       dict[str, np.ndarray]
    # obs.state:        dict[str, float]
    # obs.timestamp_us: int
    portal.send_action(policy(obs))     # or teleop.get_action()

portal.on_observation(on_observation)
await portal.connect(url, token)
```

Full runnable version, including token minting, in the
[Quickstart](docs/quickstart.md).

## Documentation

| Page | What's in it |
|---|---|
| [Quickstart](docs/quickstart.md) | Install, tokens, first run with the Portal API |
| [Portal API](docs/portal-api.md) | The primary surface. `PortalConfig`, callbacks, send methods, role semantics |
| [Concepts](docs/concepts.md) | Roles, the observation model, frame format |
| [Tuning](docs/tuning.md) | `fps`, `slack`, `tolerance`, asymmetric rates, reliability |
| [RPC](docs/rpc.md) | Imperative commands (`home`, `calibrate`, ...) on top of LiveKit RPC |
| [Synchronization deep dive](docs/synchronization.md) | The full match algorithm, cursor bookkeeping, complexity |
| [lerobot integration](docs/lerobot.md) | The optional convenience plugins |

## License

Apache-2.0. See [LICENSE](LICENSE) for details.
