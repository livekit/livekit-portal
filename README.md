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

portal.set_action_callback(on_action)

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

portal.set_observation_callback(on_observation)
```

## Synchronization

State and video frames are tagged with system time on the sender side. The receiver matches them locally within a configurable search range (default 30ms). An observation is only formed when all registered video tracks have a matching frame for a given state. Unmatched states are dropped and reported via the drop callback.

Video frame timestamps are embedded using LiveKit's packet trailer feature, which survives the full WebRTC encode/decode pipeline.

## Configuration

```python
config.set_video_buffer(30)       # frames buffered per video track
config.set_state_buffer(30)       # states buffered for sync
config.set_search_range_ms(30)    # max timestamp delta for a match
config.set_observation_buffer(10) # synced observations buffered
```

## Language support

Portal is written in Rust and uses UniFFI to generate Python bindings. The Rust API is available directly for Rust users.

## License

Apache-2.0
