<a href="https://livekit.io/">
  <img src=".github/assets/livekit-mark.png" alt="LiveKit logo" width="100" height="100">
</a>

# livekit-portal

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)](https://www.python.org/downloads/)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org/)

<!--BEGIN_DESCRIPTION-->
A drop-in link between robots and their teleoperators or agents. Portal handles video streams, data streams, and **timestamp-synced observations** over LiveKit, so a VLA model on the operator side sees the same bundled `(frames, state, t)` tuple it would see from a local robot. Works with [LeRobot](https://github.com/huggingface/lerobot) out of the box.
<!--END_DESCRIPTION-->

## What it is

If you have a robot *here* and you want to **teleoperate it** or **run a
policy against it** from *somewhere else*, Portal is the network tier. The
physical robot stays in the loop; Portal bundles synchronized
`(frames, state, timestamp)` observations and routes actions back — over
LiveKit, so it works across WAN.

Three concrete things you get:

- **Synchronized observations** — video frames and state arrive bundled by
  sender timestamp, in the shape VLA policies expect.
- **Drop-in for lerobot** — two plugins wrap your existing `Robot` or
  `Teleoperator`; the remote arm appears as a local device to any lerobot
  workflow (teleop, dataset recording, policy eval).
- **Works with or without it** — Rust core, Python via UniFFI. If you're not
  on lerobot, there's a direct `Portal` API.

## I want to…

| Goal | Start here |
|---|---|
| **Teleoperate a lerobot-compatible robot over the network** | [Quickstart](docs/quickstart.md) |
| **Run a VLA policy against a remote robot** | [Quickstart](docs/quickstart.md) (same setup, policy replaces the leader on the operator side) |
| **Use Portal directly from a non-lerobot stack** | [Portal API](docs/portal-api.md) |
| **See a working end-to-end example** | [Examples](#examples) below |

## Examples

Running examples is the fastest way to a known-good setup. Both live under
[`examples/python/`](examples/python):

- **[`examples/python/basic/`](examples/python/basic)** — no hardware
  required. Synthetic video + state on one terminal, subscriber on another.
  Proves your LiveKit credentials and native build are healthy.
  ```bash
  cd examples/python/basic
  cp .env.example .env            # fill in LIVEKIT_URL / API_KEY / API_SECRET
  uv sync
  uv run robot.py                 # terminal 1
  uv run teleoperator.py          # terminal 2
  ```
- **[`examples/python/so101/`](examples/python/so101)** — real hardware.
  Physical **SO-101 follower** driven by a remote **SO-101 leader**, with the
  camera and joint state rendered in [rerun](https://rerun.io). Full
  calibration + wiring walkthrough in its
  [README](examples/python/so101/README.md).

Adapt either of them to bring up your own setup.

## 30-second sketch

Robot side (wraps your existing lerobot `Robot`):

```python
teleop = LiveKitTeleoperator(
    LiveKitTeleoperatorConfig(url=..., token=..., session="session-1", fps=30),
    robot=robot,
)
teleop.connect()
while running:
    teleop.send_feedback(robot.get_observation())
    action = teleop.get_action()
    if action:
        robot.send_action(action)
```

Operator side (wraps your local leader / gamepad / policy):

```python
robot = LiveKitRobot(
    LiveKitRobotConfig(url=..., token=..., session="session-1", fps=30,
                       camera_names=("cam1",), camera_height=480, camera_width=640),
    teleop=leader,
)
robot.connect()
while running:
    obs = robot.get_observation()          # synced frames + state
    robot.send_action(leader.get_action()) # or policy(obs)
```

Full runnable version — including token minting and the non-lerobot variant —
in the [Quickstart](docs/quickstart.md).

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

- [Quickstart](docs/quickstart.md) — install, tokens, first run.
- [Concepts](docs/concepts.md) — roles, observation model, frame format.
- [lerobot integration](docs/lerobot.md) — plugin config, CLI mode,
  troubleshooting.
- [Portal API](docs/portal-api.md) — raw API for non-lerobot stacks.
- [Tuning](docs/tuning.md) — `fps` / `slack` / `tolerance`, asymmetric rates,
  reliability.
- [RPC](docs/rpc.md) — imperative commands (`home`, `calibrate`, etc.) on top
  of LiveKit RPC.
- [Synchronization deep dive](docs/synchronization.md) — the full match
  algorithm, cursor bookkeeping, complexity analysis.

## License

Apache-2.0. See [LICENSE](LICENSE) for details.
