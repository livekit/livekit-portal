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

## Tuning the sync buffer

The defaults are tuned for 60fps video and state. All settings are on the config object before connecting.

```python
config.set_search_range_ms(10)    # max timestamp delta for a state-frame match (default: 10ms)
config.set_video_buffer(5)        # frames buffered per video track (default: 5)
config.set_state_buffer(5)        # states buffered for sync (default: 5)
config.set_observation_buffer(3)  # synced observations buffered (default: 3)
```

**`search_range_ms`** — how close (in ms) a state timestamp and a video frame timestamp must be to pair. At 60fps, one frame is ~16.7ms. The default of 10ms is generous for same-loop-iteration sends but tight enough to never accidentally match a neighboring frame. Increase if your state and video are sent from different threads with more jitter.

**`video_buffer`** / **`state_buffer`** — how many samples to hold while waiting for a match. At 60fps, 5 frames = ~83ms of headroom. If a match can't happen within that window, the state is dropped (and the drop callback fires). Increase if you have high network jitter or variable frame rates.

**`observation_buffer`** — how many synced observations to hold before the oldest is evicted. If your model inference takes 50ms at 60fps, ~3 observations queue up. Increase if your consumer is bursty.

**Data reliability** — state and action use reliable (lossless, ordered) delivery by default. For high-frequency inference where only the latest value matters, switch to unreliable to avoid head-of-line blocking under packet loss:

```python
config.set_state_reliable(False)   # default: True
config.set_action_reliable(False)  # default: True
```

## Language support

Portal is written in Rust. Python bindings will be available via a separate FFI crate.

## License

Apache-2.0
