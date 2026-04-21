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
  <a href="#at-a-glance">Show me</a> ·
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

## At a glance

A complete remote-robot session in two files. The robot host publishes
frames and state, executes actions, and exposes a `home` RPC. The control
host receives synced observations, runs a policy, and calls `home` before
the control loop starts.

**`robot.py`** runs on the machine the robot is plugged into.

```python
import asyncio, time
from livekit.portal import Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session-1", Role.ROBOT)
    cfg.add_video("front")                       # add more tracks for multi-camera
    cfg.add_state(["j1", "j2", "j3"])
    cfg.add_action(["j1", "j2", "j3"])
    cfg.set_fps(30)

    portal = Portal(cfg)

    # One-shot commands. Either side can register. Either side can invoke.
    def on_home(_):
        robot.home()
        return "ok"
    portal.register_rpc_method("home", on_home)

    # Actions arrive here as the operator produces them.
    portal.on_action(lambda a: robot.send_action(a.values))

    await portal.connect(url, token)

    while running:
        obs = robot.get_observation()
        ts = int(time.time() * 1_000_000)
        portal.send_video_frame("front", obs.image, 640, 480, timestamp_us=ts)
        portal.send_state(obs.state, timestamp_us=ts)
        await asyncio.sleep(1 / 30)

asyncio.run(main())
```

**`operator.py`** runs wherever your policy or teleop UI lives.

```python
import asyncio
from livekit.portal import Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session-1", Role.OPERATOR)
    cfg.add_video("front")
    cfg.add_state(["j1", "j2", "j3"])
    cfg.add_action(["j1", "j2", "j3"])
    cfg.set_fps(30)

    portal = Portal(cfg)

    # Cameras, state, and a sender timestamp arrive fused as one tuple.
    def on_observation(obs):
        # obs.frames["front"], obs.state, obs.timestamp_us
        portal.send_action(policy(obs))

    portal.on_observation(on_observation)
    await portal.connect(url, token)

    await portal.perform_rpc("home")             # imperative commands, not a loop
    print(portal.metrics())                      # RTT, sync delta, jitter, drops

    while running:
        await asyncio.sleep(1)

asyncio.run(main())
```

That is the whole surface at work in one page. Synced observations, an
action callback, an RPC for one-shots, and a live metrics snapshot. The
code above is a sketch, not a runnable file. The real one is in
[`examples/python/basic/`](examples/python/basic) with token minting
already wired up.

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

## Using with lerobot

If your stack is already on [lerobot](https://github.com/huggingface/lerobot),
two optional plugin packages wrap the Portal code above. You pass in your
existing `Robot` or `Teleoperator` and the remote arm shows up as a local
lerobot device to any workflow (teleop, dataset recording, policy eval). See
[lerobot integration](docs/lerobot.md) for the full reference.

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
