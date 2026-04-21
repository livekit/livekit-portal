<a href="https://livekit.io/">
  <img src=".github/assets/livekit-mark.png" alt="LiveKit logo" width="100" height="100">
</a>

# livekit-portal

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)](https://www.python.org/downloads/)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org/)

<!--BEGIN_DESCRIPTION-->
Teleoperate a robot, or run a policy against it, from anywhere on the internet. Portal carries cameras, joint state, and actions over LiveKit and delivers synchronized `(frames, state, timestamp)` observations on the control side — the shape robotics policies already expect. Drop-in for [LeRobot](https://github.com/huggingface/lerobot); a direct API covers other stacks.
<!--END_DESCRIPTION-->

## The idea

Think of your robot as a device that normally plugs into one computer. Portal
lets it "plug into" a different one over the network — so your teleop
interface, your training loop, or your policy server can run anywhere and
still see the robot as if it were local. The physical robot stays in the
loop; Portal only adds the network tier.

Two plugins make this drop-in for
[lerobot](https://github.com/huggingface/lerobot): wrap your existing `Robot`
on the robot-side machine and your `Teleoperator` (or policy output) on the
control-side machine, and the remote arm shows up as a local lerobot device
to any workflow — teleoperation, dataset recording, policy eval. If you're
not on lerobot, a direct `Portal` API covers the same ground.

## I want to…

| Goal | Start here |
|---|---|
| **Teleoperate a lerobot-compatible robot over the network** | [Quickstart](docs/quickstart.md) |
| **Run a policy against a remote robot** | [Quickstart](docs/quickstart.md) (same setup — the policy replaces the leader on the control side) |
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
