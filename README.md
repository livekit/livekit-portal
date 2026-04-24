<p align="center">
  <a href="https://livekit.io/">
    <img src=".github/assets/livekit-mark.png" alt="LiveKit logo" width="100" height="100">
  </a>
</p>

<h1 align="center">livekit-portal</h1>

<p align="center">
  <a href="https://github.com/livekit/livekit-portal/actions/workflows/tests.yml"><img src="https://github.com/livekit/livekit-portal/actions/workflows/tests.yml/badge.svg?branch=main" alt="tests"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License"></a>
  <a href="https://www.python.org/downloads/"><img src="https://img.shields.io/badge/python-3.10%2B-blue" alt="Python 3.10+"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-stable-orange" alt="Rust"></a>
</p>

<p align="center">
  <img src=".github/assets/portal-demo.gif" alt="Portal demo: synced camera and joint state between a remote robot and a local operator" width="720">
</p>

<!--BEGIN_DESCRIPTION-->
<p align="center"><b>Teleoperate a robot, or run a policy against it, from anywhere on the internet.</b> Portal carries cameras, joint state, and actions between a robot host and a control host over LiveKit. On the control side, everything arrives as synchronized <code>(frames, state, timestamp)</code> observations. Works with any robotics stack. An optional <a href="https://github.com/huggingface/lerobot">LeRobot</a> plugin adds a one-line drop-in for lerobot users.</p>
<!--END_DESCRIPTION-->

<p align="center">
  <a href="#quickstart">Quickstart</a> ·
  <a href="#examples">Examples</a> ·
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

## Quickstart

### Install

Portal is not on PyPI yet, and there are no prebuilt native binaries.
Today you build from source. The flow is one clone, one build, one sync.

**Prerequisites:**

- A [Rust toolchain](https://rustup.rs/) (stable `cargo`)
- Python 3.10+
- [`uv`](https://docs.astral.sh/uv/)

```bash
git clone https://github.com/livekit/livekit-portal.git
cd livekit-portal

bash scripts/build_ffi_python.sh release   # compile cdylib + generate UniFFI bindings
cd python/packages/livekit-portal && uv sync   # install Python deps into .venv
```

`build_ffi_python.sh` calls `cargo build -p livekit-portal-ffi`, drops the
resulting `liblivekit_portal_ffi.{dylib,so,dll}` next to the Python
sources, and runs `uniffi-bindgen` to emit the matching Python module. On
a cold machine this takes a couple of minutes.

`from livekit.portal import Portal` now works inside that `.venv`.

**Use from another project.** After the native build, depend on the
package by path. The [shipped examples](examples/python/basic/pyproject.toml)
do this with relative paths because they sit inside the repo. From any
other project, use an absolute path:

```bash
# uv
uv add --editable /absolute/path/to/livekit-portal/python/packages/livekit-portal

# pip
pip install -e /absolute/path/to/livekit-portal/python/packages/livekit-portal
```

Or wire it directly into your `pyproject.toml`:

```toml
[project]
dependencies = ["livekit-portal"]

[tool.uv.sources]
livekit-portal = { path = "/absolute/path/to/livekit-portal/python/packages/livekit-portal", editable = true }
```

Rerun `build_ffi_python.sh` whenever the Rust code changes. The editable
install picks up the refreshed cdylib on the next import. Prebuilt
wheels are on the roadmap.

### Code

A complete remote-robot session in two files. The robot host publishes
frames and state, executes actions, and exposes a `home` RPC. The control
host receives synced observations, runs a policy, and calls `home` before
the control loop starts.

**`robot.py`** runs on the machine the robot is plugged into.

```python
import asyncio, time
from livekit.portal import DType, Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session-1", Role.ROBOT)
    cfg.add_video("front")                       # add more tracks for multi-camera
    cfg.add_state_typed([("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)])
    cfg.add_action_typed([("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)])
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
from livekit.portal import DType, Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session-1", Role.OPERATOR)
    cfg.add_video("front")
    cfg.add_state_typed([("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)])
    cfg.add_action_typed([("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)])
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
code above is a sketch. For a runnable version with token minting already
wired up, see [`examples/python/basic/`](examples/python/basic) or the
step-by-step [Quickstart doc](docs/quickstart.md).

## Behind the project

Teleoperation over WAN is a networking problem before it is a robotics
problem. Low-latency video and control data have to traverse NAT,
asymmetric bandwidth, jitter, and packet loss. WebRTC was built for
exactly this, and [LiveKit](https://livekit.io/) wraps it in a
production-grade SFU with a clean SDK. Portal builds the robotics layer
on top.

That layer exists because robotics policies want one bundled
`Observation` per tick: cameras, joint state, and a timestamp arriving
together. LiveKit's transport primitives do not deliver data that way.
Video tracks and data streams each have their own pacing, codec path,
and retransmission. On the receiver they surface as independent event
streams arriving out of phase.

Portal closes that gap. Every outgoing frame and state packet carries the
sender's monotonic clock (packet-trailer metadata for video, a `u64`
prefix for data). On the control side, a per-session `SyncBuffer` matches
them by sender timestamp:

```text
for each head state S:
    for each registered video track k:
        F = nearest pending frame in track k to S
        if |S - F| < search_range:                   track k matches
        elif track k's newest frame is past S + R:   drop the state
        else:                                        wait for a newer frame

if every track matched:
    emit Observation { frames, state, timestamp_us: S }
```

The real implementation is amortized `O(N + M)` through two-pointer
cursors and blocker-gated short-circuiting, with `O(1)` unmatchability
detection. Full walkthrough in
[docs/synchronization.md](docs/synchronization.md). The
[Concepts](docs/concepts.md) page covers roles and the observation model.
[Tuning](docs/tuning.md) covers `fps`, `slack`, and `tolerance`.

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

## Why LiveKit

Portal sits on LiveKit rather than raw WebRTC or a custom transport. The
choice keeps the codebase focused on robotics instead of plumbing.

| What LiveKit gives you | Why it matters for Portal |
|---|---|
| **Production SFU** | A robot room with an operator, a policy runner, and a passive viewer is the same session as one-to-one. No mesh, no client-side re-encoding. |
| **Rooms, tokens, auth** | JWT-based permissions per participant. No identity service or handshake protocol to design. |
| **Transport primitives** | RTP media with pacing and bandwidth adaptation. SCTP data channels, reliable or unreliable. Typed byte streams with chunking. RPC for one-shots. Portal maps observations straight onto these. |
| **Cross-language SDKs** | Rust, Python, Swift, Kotlin, JavaScript, Unity. A browser teleop UI speaks the same protocol as the robot host. |
| **Deploy anywhere** | [LiveKit Cloud](https://livekit.io/cloud) for zero ops, or self-host the open-source server. TURN relays handle NAT traversal. |
| **Recording and egress** | Session recording lines up with dataset capture. Webhooks surface participant events. |

On a raw WebRTC stack you keep the media engine and lose everything
above. On a custom transport you reimplement all of it before the
robotics work even starts.

Running on a single machine or a LAN-only robot? You do not need any of
this. A direct socket is enough.

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
