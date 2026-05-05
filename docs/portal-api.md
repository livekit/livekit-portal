# Portal API

The primary surface for using livekit-portal from any robotics stack.

You construct a `PortalConfig`, hand it to a `Portal`, register callbacks,
and push frames and state or actions. Everything else in this repository,
including the optional lerobot plugins, is built on top of this API.

## Installation

Portal is not on PyPI yet. You build from source. See the
[Quickstart](quickstart.md#1-install) for the full flow. Summary:

```bash
git clone https://github.com/livekit/livekit-portal.git
cd livekit-portal

bash scripts/build_ffi_python.sh release   # or `debug` for faster iteration
cd python/packages/livekit-portal && uv sync
```

If the cdylib lives elsewhere (e.g. during Rust-side dev), point
`LIVEKIT_PORTAL_FFI_LIB` at it and skip the copy step.

### Rust

The core crate is usable directly without going through Python. From
another Cargo workspace, depend on the path:

```toml
[dependencies]
livekit-portal = { path = "path/to/livekit-portal/livekit-portal" }
```

Python bindings ship via the `livekit-portal-ffi` crate (UniFFI + C ABI)
and a pure-Python package in `python/packages/livekit-portal/`.

## Role semantics

Portal has two roles: `Role.ROBOT` and `Role.OPERATOR`. The role is fixed at
`PortalConfig` construction. Calling a send method the role does not own
returns `WrongRole`.

| Role | Publishes | Subscribes |
|------|-----------|------------|
| `Role.ROBOT` | video frames, state | actions |
| `Role.OPERATOR` | actions | video frames + state, merged into synced observations |

Both sides must register the same schema via `add_video` / `add_state_typed` /
`add_action_typed`. Camera names, field names, and per-field dtypes must
match across sides.

State and action schemas are typed. Each field declares a `DType` that drives
its on-wire width. `DType.F64` is the lossless default. `F32` halves the
bytes per field for joint angles. `I8`, `I16`, `U8`, `U16`, `U32` suit
discrete indices or counters. `Bool` is one byte for binary signals like
gripper open or estop. Values you send through `send_state` /
`send_action` stay as Python floats. Saturation applies at the wire boundary
for out-of-range integer values.

## Robot side

```python
import asyncio
from livekit.portal import DType, Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session", Role.ROBOT)
    cfg.add_video("camera1")
    cfg.add_video("camera2")
    cfg.add_state_typed([
        ("joint1", DType.F32),
        ("joint2", DType.F32),
        ("joint3", DType.F32),
    ])
    cfg.add_action_typed([
        ("joint1", DType.F32),
        ("joint2", DType.F32),
        ("joint3", DType.F32),
    ])

    portal = Portal(cfg)

    def on_action(action):
        # action.values is the dict.
        # action.timestamp_us is the sender's clock.
        robot.send_action(action.values)

    portal.on_action(on_action)
    await portal.connect(url, token)

    while running:
        obs = robot.get_observation()
        portal.send_video_frame("camera1", obs.image.cam1, width, height)
        portal.send_video_frame("camera2", obs.image.cam2, width, height)
        portal.send_state(obs.state)
        await asyncio.sleep(1 / fps)

asyncio.run(main())
```

## Operator side

```python
import asyncio
from livekit.portal import DType, Portal, PortalConfig, Role

async def main():
    cfg = PortalConfig("session", Role.OPERATOR)
    cfg.add_video("camera1")
    cfg.add_video("camera2")
    cfg.add_state_typed([
        ("joint1", DType.F32),
        ("joint2", DType.F32),
        ("joint3", DType.F32),
    ])
    cfg.add_action_typed([
        ("joint1", DType.F32),
        ("joint2", DType.F32),
        ("joint3", DType.F32),
    ])

    portal = Portal(cfg)

    def on_observation(obs):
        # obs.frames: dict[str, np.ndarray]   # one per registered video track
        # obs.state:  dict[str, float]
        # obs.timestamp_us: int               # sender clock
        action = model.select_action(obs)
        portal.send_action(action)

    portal.on_observation(on_observation)
    await portal.connect(url, token)

asyncio.run(main())
```

Callbacks fire on the asyncio loop that was running when you registered
them. User code never runs on the tokio worker thread.

## Typed values on receive

`Action`, `State`, and `Observation` are typed by default. `.values`
(and `observation.state`) hold Python-native types per the declared
schema: `DType.BOOL` fields are `bool`, integer dtypes are `int`, float
dtypes are `float`. `.raw_values` (and `observation.raw_state`) keep
the lossless `f64` view if you want to write into a numpy buffer
without a per-field cast.

```python
def on_action(action):
    # action.values["gripper"] is True (bool)
    # action.values["mode"] is 3 (int)
    # action.values["shoulder"] is 0.5 (float)
    # action.raw_values is the underlying Dict[str, float]
    ...
```

The Rust SDK mirrors this: `Action` / `State` / `Observation` carry
`values: HashMap<String, TypedValue>` alongside `raw_values:
HashMap<String, f64>`. The mental model is identical across languages:
declare a dtype, send whatever you want, receive back as the declared
type.

## Gotchas

- **Send-time dtype mismatch raises immediately.** If you send a
  `float` into a `BOOL` field, a `bool` into a `F32` field, or any
  other type that doesn't match the declared dtype, `send_state` /
  `send_action` raises `PortalError::DtypeMismatch` before the packet
  is constructed. No silent cast. Python follows the same rule via
  `isinstance` checks on each value. `int` is accepted for float
  dtypes (standard numeric promotion); `bool` is rejected everywhere
  except `BOOL` fields.
- **Saturation is silent except for a one-time log.** Saturation
  happens after the dtype check passes — e.g., sending `9999` as an
  `i8` in Rust (or `9999` as an int for an `I8` field in Python)
  clips to `127`. The publisher emits a single `WARN` per (topic,
  field) on first saturation, then stays quiet. The peer receives
  the clipped value and never sees the original.
- **Schema mismatch is detected but not raised.** Every packet carries a
  `u32` fingerprint derived from the ordered field names and dtypes. A
  peer whose schema disagrees (any rename, dtype flip, or reorder) sees
  its packets dropped with a `WARN` per unique offending fingerprint. The
  healthy side keeps running. No exception is raised.
- **Unknown field names on send are dropped.** Keys in the dict you pass
  to `send_action` / `send_state` that are not in the declared schema get
  a one-time `WARN` and are then silently ignored. Check `portal.metrics()`
  and your logs if a field appears to not arrive — the typo is the usual
  cause.
- **NaN into `Bool` becomes `false`.** NaN into integer dtypes becomes
  `0`. Both count as saturation and log once per field.
- **Boundary values do not saturate.** `127.0` into `I8`, `-128.0` into
  `I8`, `65535.0` into `U16`, and `0.0` into any unsigned type are
  representable and silent.

## Frame format

`send_video_frame` expects packed RGB24 NumPy arrays of shape `(H, W, 3)`
uint8. Width and height must both be even. Full details in
[concepts.md](concepts.md#video-frame-format).

## Frame video (lossless or codec-of-your-choice)

`add_video(name)` defaults to `VideoCodec.H264`, the WebRTC media path
(lossy). For policies that read the pixels — VLA inference, behavior
cloning, any case where colorspace shift breaks the policy distribution
— pass a non-H264 codec on the same call:

```python
from livekit.portal import VideoCodec

cfg.add_video("front", codec=VideoCodec.MJPEG, quality=90)
cfg.add_video("wrist", codec=VideoCodec.PNG)
cfg.add_video("debug", codec=VideoCodec.RAW)
```

The user-facing API is identical — `send_video_frame`, `on_video_frame`,
`get_video_frame`, observations all work the same way. The frames travel
over a reliable byte stream (not WebRTC media), encoded with the chosen
codec, and arrive as RGB on the other end.

Latency scales with encoded payload size: the byte-stream path costs
roughly `1 ms + 2 ms × ⌈encoded_size / 15 KB⌉` per frame on localhost.
Pick a codec whose output fits in one chunk for low-latency inference.
At typical inference resolutions (224×224 to 480p) MJPEG q=80–95 fits.

See [frame-video.md](frame-video.md) for the codec/fps tables, wire
format, and metrics surface.

## What else is on `Portal`

- `portal.on_observation(cb)`: synced observations (operator only).
- `portal.on_drop(cb)`: states that could not be matched (operator only).
- `portal.on_action(cb)`: incoming actions (robot only).
- `portal.on_state(cb)`: raw state firehose (operator only). Every packet
  fires. Use `on_observation` if you want paced data.
- `portal.send_action(values, timestamp_us=...)`: operator only.
- `portal.send_video_frame(name, frame, timestamp_us=...)`: robot only.
- `portal.send_state(values, timestamp_us=...)`: robot only.
- `portal.metrics()`: `PortalMetrics` snapshot (sync, transport, buffers,
  rtt).
- `portal.register_rpc_method(name, handler)` /
  `portal.perform_rpc(name, ...)`: see [rpc.md](rpc.md).
- `await portal.connect(url, token)` / `await portal.disconnect()`.
- `portal.close()` / `cfg.close()`: release native handles.

## End-to-end encryption

Call `cfg.set_e2ee_key(key: bytes)` before `connect`. Both peers must use the
same key. All media tracks and data channels (state, actions, RPC) are
encrypted with AES-GCM.

```python
import os

cfg.set_e2ee_key(os.environ["PORTAL_E2EE_KEY"].encode())
```

See [e2ee.md](e2ee.md) for key generation, distribution patterns, and coverage
details.

## Reference

- [Concepts](concepts.md). Roles, observation model, frame format.
- [Frame video](frame-video.md). Codec choice, latency math, wire format
  for byte-stream-based per-frame video.
- [Tuning](tuning.md). `fps`, `slack`, `tolerance`, asymmetric rates.
- [Synchronization deep dive](synchronization.md). The match algorithm.
- [RPC](rpc.md). Imperative commands on top of LiveKit RPC.
- [E2EE](e2ee.md). Shared-key end-to-end encryption.
- [`examples/python/basic/`](../examples/python/basic). The smallest
  end-to-end script using this API, with synthetic video.
- [lerobot integration](lerobot.md). The optional convenience plugins that
  wrap this API for lerobot users.
