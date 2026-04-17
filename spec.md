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

- If a side declares a field or video track that the peer never publishes, the consumer simply never receives it — no error. Extra fields on the peer side are ignored.
- When a caller sends a partial state/action dict (only some of the declared fields), missing fields **carry forward** their last sent value on the publisher side. Fields never sent start at `0.0`. This keeps the observation coherent when a sensor reports only a subset per tick.
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

State and action format: `Dict[str, float]`. Both a push API (callbacks, fire on every receive) and a pull API (latest-wins peek) are provided. Use the callback if you want every sample (with your own history buffer). Use the pull API if you only care about the most recent value per inference tick.

```python
# --- Push API (callbacks) ---
# On robot side
inference_portal.on_action(callback)

# On operator side
inference_portal.on_observation(callback)      # synced bundle, fires when a complete sync is formed
inference_portal.on_state(callback)            # fires on every state received (unsynced)
inference_portal.on_video_frame("camera1", callback)  # fires on every frame received (unsynced)
inference_portal.on_drop(callback)             # fires on sync-fail and state-buffer overflow

# --- Pull API (latest-wins) ---
action = inference_portal.get_action()         # robot: Option[Dict[str, float]]
obs    = inference_portal.get_observation()    # operator: Option[Observation]
state  = inference_portal.get_state()          # operator: Option[Dict[str, float]]
frame  = inference_portal.get_video_frame("camera1")  # operator: Option[VideoFrameData]
```

An observation is a complete synced bundle: one state matched with one frame from every registered video track. There are no partial observations. If any registered video track is missing a matching frame within the sync window, the observation is not formed and the state is dropped.

The pull API is peek-style: `get_*()` always returns the most recent value (or `None` if nothing has arrived yet), and repeated calls return the same value until a new one arrives. The library does not buffer history for the pull API — if you need every sample, register the callback and buffer on your side.

### Tuning

All tuning is set on the config object before connecting. Portal is built around unified sampling: the robot captures state + frames at the same tick rate, so a single `fps` knob derives the sync search window, and a single `slack` knob sizes all internal buffers.

```python
config.set_fps(30)               # unified capture rate; derives search_range = 1/(2·fps)
config.set_slack(5)              # ticks of pipeline headroom (video + state sync buffers)

config.set_state_reliable(True)  # default: True. reliable = lossless ordered delivery, unreliable = lowest latency
config.set_action_reliable(True) # default: True. use False for high-frequency inference where latest value matters most

config.set_ping_ms(1000)         # default: 1000. set 0 to disable RTT pinging on this side
```

### Metrics

Portal collects counters and gauges on hot paths with atomic updates, so observation is effectively free. Pull the current snapshot at any cadence:

```python
m = inference_portal.metrics()
# inference_portal.reset_metrics()   # zero counters and sample windows
```

The snapshot is grouped into four sections:

```
metrics.sync
  observations_emitted        # cumulative synced observations delivered
  states_dropped              # cumulative: sync-fail drops + state-buffer overflow drops
  match_delta_us_p50/p95      # worst per-track alignment within each observation, rolling window
  last_blocker_track          # sticky: most recent track that stalled sync

metrics.transport
  frames_sent / frames_received   # per video track
  states_sent / states_received
  actions_sent / actions_received
  frame_jitter_us                 # per video track, RFC 3550 inter-arrival jitter (EWMA, α=1/16)
  state_jitter_us / action_jitter_us

metrics.buffers                   # fill gauges + overflow counters
  video_fill                      # gauge, per video track
  state_fill                      # gauge
  evictions                       # per video track, cumulative (overflow)

metrics.rtt
  rtt_us_last / rtt_us_mean / rtt_us_p95
  pings_sent / pongs_received
```

RTT is measured on a reserved `portal_rtt` data topic. Each side sends an unreliable ping at `ping_ms`; the other side echoes it as a pong carrying the original timestamp. The pinging side computes RTT = `now − ping_ts` when the pong arrives. Unreliable delivery is deliberate: reliable retransmits would inflate the measurement. Echo is always active, so one side can disable pinging and still let the other measure.

Jitter is the RFC 3550 EWMA on inter-arrival deltas: `J += (|D| − J) / 16`, where `D = (recv_i − recv_{i-1}) − (send_i − send_{i-1})`. Drift-robust (only looks at deltas) and unitless of absolute clock offset.

Percentiles are computed from a bounded ring of 256 recent samples — fast, bounded memory, accurate enough for health monitoring rather than SLO reporting.

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
