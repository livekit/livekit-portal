<a href="https://livekit.io/">
  <img src=".github/assets/livekit-mark.png" alt="LiveKit logo" width="100" height="100">
</a>

# livekit-portal

<!--BEGIN_DESCRIPTION-->
A simple link for robots to their teleoperator or agents. Handles sending video streams, data streams, and observation syncing over LiveKit. Built to work well with LeRobot or any modern robot model stack.
<!--END_DESCRIPTION-->

## The problem

Modern robotics stacks expect synchronized observations bundled together. A VLA model needs video frames and joint states matched by timestamp, delivered as one unit. LiveKit tracks are decoupled by default. video, audio, and data all stream independently.

Portal bridges this gap. It tags video frames and state data with timestamps on the sender side, then matches them on the receiver side into synchronized observations.

## How it works

Portal has two roles: **Robot** and **Operator**.

- **Robot** publishes video frames and joint states, subscribes to actions
- **Operator** subscribes to video and states (synced into observations), publishes actions

```python
# On robot side
from livekit.portal import Portal, PortalConfig, Role

config = PortalConfig("session", Role.ROBOT)
config.add_video("camera1")
config.add_video("camera2")
config.add_state(["joint1", "joint2", "joint3"])
config.add_action(["joint1", "joint2", "joint3"])

portal = Portal(config)
await portal.connect(url, token)

def on_action(action):
    # action.values is the dict; action.timestamp_us is the sender's clock (µs).
    robot.send_action(action.values)

portal.on_action(on_action)

while True:
    obs = robot.get_observation()
    portal.send_video_frame("camera1", obs.image.camera1, width, height)
    portal.send_video_frame("camera2", obs.image.camera2, width, height)
    portal.send_state(obs.state)
    sleep(1 / fps)
```

```python
# On operator side
from livekit.portal import Portal, PortalConfig, Role

config = PortalConfig("session", Role.OPERATOR)
config.add_video("camera1")
config.add_video("camera2")
config.add_state(["joint1", "joint2", "joint3"])
config.add_action(["joint1", "joint2", "joint3"])

portal = Portal(config)
await portal.connect(url, token)

def on_observation(obs):
    action = model.select_action(obs)
    portal.send_action(action)

portal.on_observation(on_observation)
```

## Video frame format

`send_video_frame` expects packed **RGB24**. byte order `R, G, B`, one byte per channel, no alpha. Layout is row-major and tightly packed (stride = `width * 3`), so an exact buffer is `width * height * 3` bytes. `width` and `height` must both be even (I420 chroma subsampling).

This matches NumPy `uint8` arrays of shape `(H, W, 3)` in RGB order. the output of `PIL.Image.convert("RGB")`, or OpenCV's `cvtColor(frame, COLOR_BGR2RGB)`.

Portal converts to I420 internally via libyuv's SIMD-optimized `RAWToI420` before handing the frame to WebRTC. Approximate cost on modern ARM64 (NEON) or x86 (AVX2):

| Resolution | Per-frame | At 30 fps |
|---|---|---|
| 640×480 | ~0.3–0.9 ms | ~1–3% of a core |
| 1280×720 | ~1–3 ms | ~3–10% |
| 1920×1080 | ~2–6 ms | ~6–20% |

If your camera already produces I420/NV12, you're paying for a round-trip. For RGB/BGR sources (most cameras + most Python pipelines), this is as fast as doing the conversion yourself and saves a call.

## Synchronization

State and video frames are tagged with system time on the sender side. The receiver matches them locally within a configurable search range. An observation is only formed when all registered video tracks have a matching frame for a given state. Unmatched states are dropped and reported via the drop callback.

Video frame timestamps are embedded using LiveKit's packet trailer feature, which survives the full WebRTC encode/decode pipeline.

> **Sender requirement:** every received video frame must carry a `user_timestamp` in its packet-trailer metadata. Portal enables this automatically on tracks it publishes (`PacketTrailerFeatures.user_timestamp = true`). A subscribed track produced by anything that does *not* set this field is unsupported. Portal cannot synchronize it and the receive task will panic on the first such frame. Either republish the source through Portal or enable user-timestamp trailers on the upstream publisher.

## Tuning

Portal assumes unified sampling. the robot captures state + frames at the same tick. All sync parameters derive from a single `fps`, and all internal buffers share a single `slack` size.

```python
config.set_fps(30)            # unified capture rate (default: 30)
config.set_slack(5)           # ticks of pipeline headroom (default: 5)
config.set_tolerance(1.5)     # match window in tick units (default: 1.5)

config.set_state_reliable(True)   # default: True
config.set_action_reliable(True)  # default: True

config.set_ping_ms(1000)      # RTT ping cadence; 0 disables (default: 1000)
```

**`fps`**. unified sampling rate (use the video rate if video and state differ). Drives the match window with `tolerance`. Raise to 60 for high-rate robots.

**`slack`**. ticks of pipeline headroom: the per-track video sync buffer and the state sync buffer use this. Larger values tolerate more jitter and loss-detection latency at the cost of staleness. Minimum useful value is 2; default 5 ≈ 167ms at 30fps.

**`tolerance`**. how far a state reaches when matching a frame, in tick units. `search_range = tolerance / fps`.
- `0.5` (tight). match only within half a tick. A single lost frame drops the observation. Best for real-time control.
- `1.5` (default, widened). state falls back to the ±1 neighbor frame if its native frame was lost. Preserves observations at the cost of ±1-tick misalignment. Best for data collection and lossy links. A fair-share check prevents earlier states from stealing neighbor frames.
- `> 2.0`. allows T±2 matches. Higher recovery but the misalignment risk outweighs the benefit for most setups.

### Choosing `tolerance`

| Use case | Pick | Why |
|---|---|---|
| Real-time inference / control | `0.5` | Misalignment (acting on a visibly different frame) is worse than dropping. A drop is an explicit signal; a misaligned observation is silently wrong. |
| Data collection for VLA training | `1.5` | Every observation is a training sample. A ±1-tick misalignment (~16ms at 60fps) is usually invisible to a trained model; a dropped observation is lost data. |
| Teleop viewer | `1.5` | Visual continuity matters more than frame-perfect state alignment. |
| Clean local network (<1% loss) | either | Drops are already rare. Default is fine. |
| Lossy / cellular / wireless | `1.5` | Widening materially reduces drop rate under real loss conditions. |
| Strict-alignment datasets | `0.5` | If downstream tooling relies on exact state/frame pairing, drops are cheaper than mislabeled pairs. |

### Asymmetric rates (video faster than state)

The library handles video > state rates transparently. intervening frames between state ticks get drained at match time and don't pile up, **provided the buffer is large enough**. Two rules:

1. **Set `fps` to the video rate**, not the state rate. The match window is measured in frame intervals, so it has to know the video cadence. Example: video 60fps, state 30Hz → `set_fps(60)`.
2. **Set `slack ≥ ceil(video_rate / state_rate) + 1`**. Between consecutive state matches, roughly `video_rate / state_rate` frames accumulate per track; slack must cover that plus jitter headroom. At default slack=5 the library cleanly handles up to ~4× asymmetry. For 10× asymmetry (video 60fps, state 6Hz), bump to `slack=12` or so.

Example: video 60fps, state 10Hz (asymmetric teleop with slow sensor):

```python
config.set_fps(60)
config.set_slack(8)          # ceil(60/10) + 2 = 8
config.set_tolerance(1.5)    # still measured in video-tick intervals (~16.6ms each)
```

One thing to note: under asymmetric rates, the overall drop rate is proportional to `state_rate × video_loss_rate`, not the video rate. Losing a video frame that happened to fall on a state tick costs one observation; losing a video frame between state ticks costs nothing (it would have been drained anyway).

**Reliability**. state and action use reliable (lossless, ordered) SCTP delivery by default. For high-frequency control where only the latest value matters, switch to unreliable to avoid head-of-line blocking under packet loss. Video is always unreliable (RTP).

## Language support

Portal is written in Rust. Python bindings ship via the `livekit-portal-ffi` crate (protobuf + C ABI, matching livekit-ffi's pattern) and a pure-Python package in `python/`.

### Python. build and install

The cdylib is built with `cargo` and copied into the package; the Python package itself is a hatchling project managed with `uv`.

```bash
# one-time setup
cd python
uv venv
uv pip install -e '.[examples]'      # livekit-api + python-dotenv for examples
./scripts/build_native.sh release    # builds livekit-portal-ffi and copies the cdylib in
```

`scripts/build_native.sh debug` is faster to iterate on. If the cdylib lives somewhere else (e.g. during Rust-side dev), point `LIVEKIT_PORTAL_FFI_LIB` at it and skip the copy step.

When the `.proto` schema changes, regenerate the Python bindings:

```bash
./scripts/generate_protos.sh         # rewrites python/livekit/portal/_proto/*_pb2.py
```

### Python. quickstart

```python
import asyncio
from livekit.portal import Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session", Role.ROBOT)
    cfg.add_video("cam1")
    cfg.add_state(["j1", "j2", "j3"])
    cfg.add_action(["j1", "j2", "j3"])

    portal = Portal(cfg)
    portal.on_action(lambda a: print("got action", a.values, "ts", a.timestamp_us))

    await portal.connect(url, token)
    # send RGB bytes or a numpy (H, W, 3) uint8 array
    portal.send_video_frame("cam1", frame, width=W, height=H)
    portal.send_state({"j1": 0.1, "j2": 0.2, "j3": 0.3})
    await portal.disconnect()

asyncio.run(main())
```

Callbacks fire on the asyncio loop that was running when you called `on_action` / `on_observation` / etc., so user code never runs on the tokio worker thread directly.

### Python. end-to-end test

`examples/robot.py` and `examples/teleoperator.py` exercise both roles against a LiveKit server. They mint their own JWTs from `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET`, so you just need the API credentials and a running server.

```bash
cp python/examples/.env.example python/examples/.env
# fill in LIVEKIT_URL, LIVEKIT_API_KEY, LIVEKIT_API_SECRET
```

Then in two terminals:

```bash
cd python
uv run examples/robot.py             # terminal 1
uv run examples/teleoperator.py      # terminal 2
```

`uv run` handles the venv automatically. no `source .venv/bin/activate` needed. The first run resolves dependencies and installs the package; later runs reuse the cached env.

For each directory in `<script_dir>`, `<script_dir>/..`, then `<cwd>`, the loader reads `.env` first and then `.env.local` (local overrides .env). The first directory that contains either file wins, so put credentials in `examples/.env` (committed-template-adjacent) or `examples/.env.local` (gitignored, your real values).

## License

Apache-2.0
