<a href="https://livekit.io/">
  <img src=".github/assets/livekit-mark.png" alt="LiveKit logo" width="100" height="100">
</a>

# livekit-portal

<!--BEGIN_DESCRIPTION-->
A simple link for robots to their teleoperator or agents. Handles sending video streams, data streams, and observation syncing over LiveKit. Built to work well with LeRobot or any modern robot model stack.
<!--END_DESCRIPTION-->

## The problem

Modern robotics stacks expect synchronized observations bundled together. A VLA model needs video frames and joint states matched by timestamp, delivered as one unit. LiveKit tracks are decoupled by default — video, audio, and data all stream independently.

Portal bridges this gap. It tags video frames and state data with timestamps on the sender side, then matches them on the receiver side into synchronized observations.

## How it works

Portal has two roles: **Robot** and **Operator**.

- **Robot** publishes video frames and joint states, subscribes to actions
- **Operator** subscribes to video and states (synced into observations), publishes actions

```python
# On robot side
config = PortalConfig("session", Role.ROBOT)
config.add_video("camera1")
config.add_video("camera2")
config.add_state(["joint1", "joint2", "joint3"])
config.add_action(["joint1", "joint2", "joint3"])

portal = Portal(config)
await portal.connect(url, token)

def on_action(action):
    robot.send_action(action)

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

## Synchronization

State and video frames are tagged with system time on the sender side. The receiver matches them locally within a configurable search range. An observation is only formed when all registered video tracks have a matching frame for a given state. Unmatched states are dropped and reported via the drop callback.

Video frame timestamps are embedded using LiveKit's packet trailer feature, which survives the full WebRTC encode/decode pipeline.

> **Sender requirement:** every received video frame must carry a `user_timestamp` in its packet-trailer metadata. Portal enables this automatically on tracks it publishes (`PacketTrailerFeatures.user_timestamp = true`). A subscribed track produced by anything that does *not* set this field is unsupported — Portal cannot synchronize it and the receive task will panic on the first such frame. Either republish the source through Portal or enable user-timestamp trailers on the upstream publisher.

## Tuning

Portal assumes unified sampling — the robot captures state + frames at the same tick. All sync parameters derive from a single `fps`, and all internal buffers share a single `slack` size.

```python
config.set_fps(30)            # unified capture rate (default: 30)
config.set_slack(5)           # ticks of pipeline headroom (default: 5)
config.set_tolerance(1.5)     # match window in tick units (default: 1.5)

config.set_state_reliable(True)   # default: True
config.set_action_reliable(True)  # default: True

config.set_ping_ms(1000)      # RTT ping cadence; 0 disables (default: 1000)
```

**`fps`** — unified sampling rate (use the video rate if video and state differ). Drives the match window with `tolerance`. Raise to 60 for high-rate robots.

**`slack`** — ticks of pipeline headroom: the per-track video sync buffer and the state sync buffer use this. Larger values tolerate more jitter and loss-detection latency at the cost of staleness. Minimum useful value is 2; default 5 ≈ 167ms at 30fps.

**`tolerance`** — how far a state reaches when matching a frame, in tick units. `search_range = tolerance / fps`.
- `0.5` (tight) — match only within half a tick. A single lost frame drops the observation. Best for real-time control.
- `1.5` (default, widened) — state falls back to the ±1 neighbor frame if its native frame was lost. Preserves observations at the cost of ±1-tick misalignment. Best for data collection and lossy links. A fair-share check prevents earlier states from stealing neighbor frames.
- `> 2.0` — allows T±2 matches. Higher recovery but the misalignment risk outweighs the benefit for most setups.

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

The library handles video > state rates transparently — intervening frames between state ticks get drained at match time and don't pile up, **provided the buffer is large enough**. Two rules:

1. **Set `fps` to the video rate**, not the state rate. The match window is measured in frame intervals, so it has to know the video cadence. Example: video 60fps, state 30Hz → `set_fps(60)`.
2. **Set `slack ≥ ceil(video_rate / state_rate) + 1`**. Between consecutive state matches, roughly `video_rate / state_rate` frames accumulate per track; slack must cover that plus jitter headroom. At default slack=5 the library cleanly handles up to ~4× asymmetry. For 10× asymmetry (video 60fps, state 6Hz), bump to `slack=12` or so.

Example: video 60fps, state 10Hz (asymmetric teleop with slow sensor):

```python
config.set_fps(60)
config.set_slack(8)          # ceil(60/10) + 2 = 8
config.set_tolerance(1.5)    # still measured in video-tick intervals (~16.6ms each)
```

One thing to note: under asymmetric rates, the overall drop rate is proportional to `state_rate × video_loss_rate`, not the video rate. Losing a video frame that happened to fall on a state tick costs one observation; losing a video frame between state ticks costs nothing (it would have been drained anyway).

**Reliability** — state and action use reliable (lossless, ordered) SCTP delivery by default. For high-frequency control where only the latest value matters, switch to unreliable to avoid head-of-line blocking under packet loss. Video is always unreliable (RTP).

## Language support

Portal is written in Rust. Python bindings will be available via a separate FFI crate.

## License

Apache-2.0
