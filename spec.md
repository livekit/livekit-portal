# LiveKit Portal Spec

## Problem

Tracks on LiveKit are decoupled by default. Video streams at a different rate than audio. Data streams at a different rate than media.

For media streams, we can sync them based on a best approximation. The first frame of audio we received is synced with the first frame of video we received. To avoid jitter, we use a buffer for synchronization and only playback when an amount of frames are synced.

This works because audio video synchronization can operate with a certain minimal latency/drift. However, in robotics applications, data needs to be accurately coupled with video frames down to the milliseconds while latency must be reduced to the minimal.

That's why we support adding a small timestamp trailer to video frames, to help reconciling data and video on the receiver side.

Robotics companies can use this feature with their own implementation. But we can also go one layer up and provide a solution that better fits the current robotics stack and provide an opinionated optimized abstraction that uses our stack.

## The current state of robotics

Classically robotics is distributed. A robot would have multiple components that work and publish data at different rate. That gives the rise to ROS and the use of DDS for a reliable and simple mental model for data distribution and consumption. The architecture is built for multiple services to work together and consume data independently. For example, a SLAM service would work alongside with an obstacle detection service which all consume data streams at different frequencies.

With the rise of VLAs and other robotics models, this flexibility is traded for simplicity. Different data streams are now consumed at the same frequency and bundled for a model to consume. Instead of using a distributed service model, robots now operate e2e through a server client architecture.

This is an example of the modern robotics loop:

```python
from lerobot.robots.myrobot import MyRobot

robot = MyRobot(config=...)
robot.connect()

obs = robot.get_observation()
action = model.select_action(obs)
robot.send_action(action)
```

This simplicity is going to be the future. This is the bitter lesson of robotics.

So how do we support this? The biggest bottleneck for robotics companies to adopt our stack for inference or data collection is **synchronization of states**.

## Solution

**Livekit Portal** is a simple link for robots to their teleoperator or agents. It handles sending video streams, data streams as well as observation syncing. Built to work well with LeRobot or any modern robot model stack.

## Synchronization

State and video frames are tagged with system time on the sender side. The receiver matches them locally from its own receive buffers. No cross-machine clock sync is needed because reconciliation happens on one machine.

Each state and video frame is tagged with the sender's system time at time of sending (with optional custom timestamp override). On the receiver side, a search range defines how close a state timestamp and a video frame timestamp must be to form a pair. The match with the minimum delta wins.

## API

### Configuration

Configuration should be matching on all sides. All sides agree on the same setup.

The `session` maps to a LiveKit room. The `role` sets the client name, which means two robots cannot join the same session — the role is unique per session.

```python
# On robot side
# pub: video, state
# sub: action
inference_portal = portal(session="test_session", role=ROBOT)
inference_portal.add_video("camera1")
inference_portal.add_video("camera2")
inference_portal.add_video("camera3")
inference_portal.add_state("joint1", "joint2", "joint3")
inference_portal.add_action("joint1", "joint2", "joint3")

# On operator side
# sub: video, state
# pub: action
inference_portal = portal(session="test_session", role=OPERATOR)
inference_portal.add_video("camera1")
inference_portal.add_video("camera2")
inference_portal.add_video("camera3")
inference_portal.add_state("joint1", "joint2", "joint3")
inference_portal.add_action("joint1", "joint2", "joint3")
```

Edge cases:

- If operator adds a non-existent state or video (or doesn't add an existing one), nothing happens. They'll just not receive data for that particular state or video. Internally there is a buffer for state — if a new incomplete entry is added, it will carry forward the last known value.
- If robot adds a non-existent action, they will just not receive anything. The non-existent action will be 0.
- Portal is built to be as stateless as possible, so disconnect and reconnect can be gracefully handled.

### Sending

Each frame and state is tagged with the sender's system time at time of sending.

```python
# Each frame is tagged with system time
inference_portal.send_video_frame("camera1", frame1)  # optional: custom timestamp
inference_portal.send_video_frame("camera2", frame2)
inference_portal.send_video_frame("camera3", frame3)

# State is tagged with system time
inference_portal.send_state({"joint1": 0.0, "joint2": 0.0, "joint3": 0.0})  # optional: custom timestamp

# On operator side
inference_portal.send_action({"joint1": 0.0, "joint2": 0.0, "joint3": 0.0})  # optional: custom timestamp
```

### Receiving

State and action format: `Dict[str, float]`

```python
# On robot side
inference_portal.on_action(callback)

# On operator side
inference_portal.on_observation(callback)  # synced bundle of all video frames + state, called when a complete sync is formed
inference_portal.on_state(callback)        # called when a new state is received (unsynced)
inference_portal.on_video("camera1", callback)  # called when a new frame is received (unsynced)
```

An observation is a complete synced bundle: one state matched with one frame from every registered video track. There are no partial observations. If any registered video track is missing a matching frame within the sync window, the observation is not formed and the state is dropped.

### Sync Configuration

```python
inference_portal.set_video_buffer(30)       # (unit: samples) how many frames to buffer per video track for sync
inference_portal.set_state_buffer(30)       # (unit: samples) how many states to buffer for sync
inference_portal.set_search_range(30)       # (unit: ms) match if |timestamp_state - timestamp_frame| < range, pick minimum delta
inference_portal.set_observation_buffer(10) # how many synced observations to buffer

inference_portal.set_state_reliable(True)   # default: True. reliable = lossless ordered delivery, unreliable = lowest latency
inference_portal.set_action_reliable(True)  # default: True. use False for high-frequency inference where latest value matters most
```

### Drop Policy

When the video frame buffer cannot satisfy sync for a state, all states up to and including that state are dropped. When states are dropped, the drop callback fires.

```python
inference_portal.on_drop(callback)  # called with the dropped states, fires on both sync failure and buffer overflow
```

The drop callback is informational — the application decides what to do (e-stop, log degradation, custom recovery). Portal does not impose any safety policy on the robot; the user controls the robot loop and knows best how to react.

### Sync Internals

Once a state and video frames are synced into an observation, all video frames up to that point are removed from the buffer.

If the video frame buffer cannot satisfy sync for a state (no frame from every track within the search range), all states up to and including that state are dropped and the drop callback fires.

## Examples

### Robot side — callback action

```python
from lerobot.robots.myrobot import MyRobot

robot = MyRobot(config=...)
robot.connect()

inference_portal = portal(session="test_session", role=ROBOT)
inference_portal.add_video("camera1")
inference_portal.add_video("camera2")
inference_portal.add_state("joint1", "joint2", "joint3")
inference_portal.add_action("joint1", "joint2", "joint3")

def on_action(action):
    robot.send_action(action)

def on_drop(dropped_states):
    print(f"dropped {len(dropped_states)} states")

inference_portal.on_action(on_action)
inference_portal.on_drop(on_drop)

while True:
    obs = robot.get_observation()
    inference_portal.send_video_frame("camera1", obs.image.camera1)
    inference_portal.send_video_frame("camera2", obs.image.camera2)
    inference_portal.send_state(obs.state)

    sleep(1 / fps)
```

### Robot side — smoothed action

```python
from lerobot.robots.myrobot import MyRobot

robot = MyRobot(config=...)
robot.connect()

inference_portal = portal(session="test_session", role=ROBOT)
inference_portal.add_video("camera1")
inference_portal.add_video("camera2")
inference_portal.add_state("joint1", "joint2", "joint3")
inference_portal.add_action("joint1", "joint2", "joint3")

def on_action(action):
    smoother.add(action)

inference_portal.on_action(on_action)

while True:
    obs = robot.get_observation()
    inference_portal.send_video_frame("camera1", obs.image.camera1)
    inference_portal.send_video_frame("camera2", obs.image.camera2)
    inference_portal.send_state(obs.state)

    action = smoother.get()
    robot.send_action(action)

    sleep(1 / fps)
```

### Operator side — teleop

```python
from lerobot.robots.myrobot import MyRobot

leader = Leader(config=...)
leader.connect()

inference_portal = portal(session="test_session", role=OPERATOR)
inference_portal.add_video("camera1")
inference_portal.add_video("camera2")
inference_portal.add_state("joint1", "joint2", "joint3")
inference_portal.add_action("joint1", "joint2", "joint3")

def on_observation(obsv):
    show(obsv)

inference_portal.on_observation(on_observation)

while True:
    action = leader.get_action()
    inference_portal.send_action(action)

    sleep(1 / fps)
```
