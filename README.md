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

## Tuning

Portal assumes unified sampling — the robot captures state + frames at the same tick. All sync parameters derive from a single `fps`, and all internal buffers share a single `slack` size.

```python
config.set_fps(30)            # unified capture rate (default: 30)
config.set_slack(5)           # ticks of pipeline headroom (default: 5)

config.set_state_reliable(True)   # default: True
config.set_action_reliable(True)  # default: True

config.set_ping_ms(1000)      # RTT ping cadence; 0 disables (default: 1000)
```

**`fps`** — the unified sampling rate. Derives `search_range = 1/(2·fps)`, so at 30fps a state matches a frame within ~16.6ms. Raise to 60 for high-rate robots.

**`slack`** — ticks of pipeline headroom: the per-track video sync buffer, the state sync buffer, and the pull-side observation slot all use this. Larger values tolerate more network jitter and loss-detection latency at the cost of staleness. 5 ticks (≈83ms at 60fps) is comfortable; the minimum useful value is 2.

**Reliability** — state and action use reliable (lossless, ordered) SCTP delivery by default. For high-frequency control where only the latest value matters, switch to unreliable to avoid head-of-line blocking under packet loss. Video is always unreliable (RTP).

## Language support

Portal is written in Rust. Python bindings will be available via a separate FFI crate.

## License

Apache-2.0
