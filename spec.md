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

# On operator side. Pass in_reply_to_ts_us to correlate the action back to
# the observation it was produced from — required for metrics.policy.e2e_us_*.
inference_portal.send_action(
    {"joint1": 0.0, "joint2": 0.0, "joint3": 0.0},
    in_reply_to_ts_us=obs.timestamp_us,
)
```

### Action chunks (VLA inference)

Modern VLA policies emit a horizon of future actions per inference step. Portal
handles these as a first-class wire type instead of forcing a stream of scalar
sends. Declare the chunk with its horizon and per-field dtypes; the wire format
ships the whole tensor in one packet.

```python
# Both peers declare the chunk.
cfg.add_action_chunk(
    "act",
    horizon=50,
    fields=[("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)],
)

# Operator side — accepts numpy `(horizon, n_fields)` arrays for uniform
# dtype, or `Dict[str, ndarray]` for mixed dtypes. Pass in_reply_to_ts_us
# the same way as send_action.
portal.send_action_chunk("act", policy_output, in_reply_to_ts_us=obs.timestamp_us)

# Robot side — register a per-chunk callback. chunk.data is reconstructed
# as numpy arrays of the declared dtype; chunk.raw_data keeps the f64 view.
def on_chunk(chunk: ActionChunk) -> None:
    for t in range(chunk.horizon):
        robot.send_action({k: float(v[t]) for k, v in chunk.data.items()})

portal.on_action_chunk("act", on_chunk)
```

Chunks travel as **LiveKit byte streams**, not data packets — so the 15 KB
data-packet ceiling does not apply. Delivery is reliable and ordered.

### Frame video (lossless or codec-of-your-choice)

WebRTC video is I420 plus a lossy temporal codec. For policies that read
the pixels — VLA inference, behavior cloning, training-data capture —
that introduces a silent input-distribution shift. `add_frame_video`
declares a track that ships each frame independently over a byte stream,
encoded with `RAW`, `PNG`, or `MJPEG`, and decoded back to RGB on the
receiver. The user-facing API stays the same as `add_video`.

```python
# Both peers declare the track.
cfg.add_frame_video("front", codec=VideoCodec.MJPEG, quality=90)

# Sender uses the same call as a regular video track.
portal.send_video_frame("front", rgb_array)

# Receiver gets RGB the same way too.
def on_frame(name, frame):
    arr = frame_bytes_to_numpy_rgb(bytes(frame.data), frame.width, frame.height)
portal.on_video_frame("front", on_frame)
```

Track names are unique across `add_video` and `add_frame_video` — a
declared track lives on exactly one transport. Latency floor on the
byte-stream path is set by SCTP drain rate inside libwebrtc:
`latency ≈ 1ms + 2ms × ⌈encoded_size / 15KB⌉`. Pick a codec whose
output fits in one chunk for low-latency closed-loop work — see
[docs/frame-video.md](docs/frame-video.md).

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
config.set_fps(30)               # unified capture rate (video rate if asymmetric); tolerance*fps = search window
config.set_slack(5)              # ticks of pipeline headroom (video + state sync buffers)
config.set_tolerance(1.5)        # ticks of match window. 0.5 = tight (drop on loss); 1.5 = ±1 neighbor fallback

config.set_state_reliable(True)  # default: True. reliable = lossless ordered delivery, unreliable = lowest latency
config.set_action_reliable(True) # default: True. use False for high-frequency inference where latest value matters most

config.set_ping_ms(1000)         # default: 1000. set 0 to disable RTT pinging on this side

config.set_reuse_stale_frames(False)  # default: False. True = freeze video on frame loss instead of dropping the state
```

**Tolerance tradeoff**: at `tolerance=0.5`, a state only matches a frame within half a tick — a single lost frame drops the observation (precision over recovery). At the default `tolerance=1.5`, a state can fall back to the adjacent frame (T±1) if its own was lost, preserving the observation at the cost of ±1 tick of misalignment. A fair-share check prevents an earlier state from stealing a frame that a later state in the buffer has a closer claim to. Values `>2.0` allow T±2 fallback (rarely worth it). Pick **tight (≤1.0)** for real-time control where misalignment is unsafe; pick **widened (≥1.5)** for data collection or lossy links where dropping is worse than slight misalignment.

**Reuse stale frames**: default off. When on, a state whose video match window has elapsed reuses the most recent already-emitted frame on that track instead of dropping. Video "freezes" on the last good frame during loss while state keeps flowing. Every state becomes an observation once every track has emitted at least once. Before that, the strict drop-on-horizon rule still applies so the state buffer stays bounded if video never starts. Turn on for data collection or logging where losing a state is worse than a transient video freeze. Leave off for real-time control where a stale frame would misalign the perception/action loop.

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
  stale_observations_emitted  # subset of observations_emitted where any track reused its last frame (reuse_stale_frames only)
  states_dropped              # cumulative: sync-fail drops + state-buffer overflow drops
  match_delta_us_p50/p95      # worst per-track alignment within each observation, rolling window
  last_blocker_track          # sticky: most recent track that stalled sync

metrics.transport
  frames_sent / frames_received   # per video track
  states_sent / states_received
  actions_sent / actions_received
  action_chunks_sent / action_chunks_received
  frame_jitter_us                 # per video track, RFC 3550 inter-arrival jitter (EWMA, α=1/16)
  state_jitter_us / action_jitter_us / action_chunk_jitter_us

metrics.buffers                   # fill gauges + overflow counters
  video_fill                      # gauge, per video track
  state_fill                      # gauge
  evictions                       # per video track, cumulative (overflow)

metrics.rtt
  rtt_us_last / rtt_us_mean / rtt_us_p95
  pings_sent / pongs_received

metrics.policy
  e2e_us_p50 / e2e_us_p95     # observation → action latency, derived from
                              # in_reply_to_ts_us on received actions/chunks
  correlated_received          # how many actions/chunks carried correlation
```

`metrics.policy` populates only once correlated traffic arrives. When the
operator passes `in_reply_to_ts_us=obs.timestamp_us` into `send_action` /
`send_action_chunk`, the robot derives `now_us - in_reply_to_ts_us` on receive
and feeds it into a 256-sample rolling window. Both sides observe the *same*
clock here (the original observation timestamp originated as the robot's state
send time), so this is a true single-clock measurement — no NTP required.

Use this instead of `metrics.rtt` when the question is "how long does my
policy take, end to end?" — `rtt_*` is just the network ping; the policy's
inference time, queueing, and serialization don't show up there.

RTT is measured on a reserved `portal_rtt` data topic. Each side sends an unreliable ping at `ping_ms`; the other side echoes it as a pong carrying the original timestamp. The pinging side computes RTT = `now − ping_ts` when the pong arrives. Unreliable delivery is deliberate: reliable retransmits would inflate the measurement. Echo is always active, so one side can disable pinging and still let the other measure.

Jitter is the RFC 3550 EWMA on inter-arrival deltas: `J += (|D| − J) / 16`, where `D = (recv_i − recv_{i-1}) − (send_i − send_{i-1})`. Drift-robust (only looks at deltas) and unitless of absolute clock offset.

Percentiles are computed from a bounded ring of 256 recent samples — fast, bounded memory, accurate enough for health monitoring rather than SLO reporting.

**Under `reuse_stale_frames`**: `last_blocker_track` only updates while a track is still waiting for its first frame. Once every track has emitted, reuse replaces the wait, so a later freeze leaves `last_blocker_track` pinned to its last startup value — use `stale_observations_emitted` as the freeze signal instead. `match_delta_us_p95` also becomes unbounded (stale deltas can be seconds long), so alerts keyed on that metric need reshaping.

### Drop Policy

Under the default strict policy, when the video frame buffer cannot satisfy sync for a state, all states up to and including that state are dropped. When states are dropped, the drop callback fires.

```python
inference_portal.on_drop(callback)  # called with the dropped states, fires on both sync failure and buffer overflow
```

The drop callback is informational — the application decides what to do (e-stop, log degradation, custom recovery). Portal does not impose any safety policy on the robot; the user controls the robot loop and knows best how to react.

With `set_reuse_stale_frames(True)`, sync-fail drops are replaced by stale-frame reuse once a track has emitted at least once. Only two drop sources remain: (1) state-buffer overflow during the pre-first-emission startup window, and (2) a past-horizon frame arriving before any emission has happened for the blocked track.

### Sync Internals

Once a state and video frames are synced into an observation, all video frames up to that point are removed from the buffer.

Under the default strict policy, if the video frame buffer cannot satisfy sync for a state (no frame from every track within the search range), all states up to and including that state are dropped and the drop callback fires. Under the reuse policy, the state falls back to that track's most recently emitted frame and the observation fires normally — the video buffer is left untouched so a later state can still consume the fresh frame.

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
