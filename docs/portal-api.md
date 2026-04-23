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
cd livekit-portal/python/packages/livekit-portal

uv sync
bash scripts/build_native.sh release       # or `debug` for faster iteration
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

## Frame format

`send_video_frame` expects packed RGB24 NumPy arrays of shape `(H, W, 3)`
uint8. Width and height must both be even. Full details in
[concepts.md](concepts.md#video-frame-format).

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

## Reference

- [Concepts](concepts.md). Roles, observation model, frame format.
- [Tuning](tuning.md). `fps`, `slack`, `tolerance`, asymmetric rates.
- [Synchronization deep dive](synchronization.md). The match algorithm.
- [RPC](rpc.md). Imperative commands on top of LiveKit RPC.
- [`examples/python/basic/`](../examples/python/basic). The smallest
  end-to-end script using this API, with synthetic video.
- [lerobot integration](lerobot.md). The optional convenience plugins that
  wrap this API for lerobot users.
