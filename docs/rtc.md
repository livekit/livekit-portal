# Real-Time Chunking (RTC)

Portal's scalar action path (`send_action` / `get_action`) fits continuous
teleoperation, where the operator decides one action per tick. Chunked
policies (ACT, pi0, and the real-time chunking family from Physical
Intelligence) do not fit that model. They emit `H` future actions per
inference and overlap inference with execution.

Portal supports this by carrying two primitives:

- A reserved typed wire type, `ActionChunk`, for policy-emitted chunks.
- A generic `send_bytes` / `register_byte_stream_handler` pair for anything
  else that wants reliable topic-keyed binary delivery.

Portal is the transport. The RTC state machine (when to request a new
chunk, how to splice, how to track current step, how to cache
`prev_chunk_left_over` for soft guidance) is user code.

## The shape on the wire

```python
from livekit.portal import ActionChunk, ChunkDtype

chunk = ActionChunk(
    horizon=32,
    action_dim=7,
    dtype=ChunkDtype.F32,
    captured_at_us=int(time.time() * 1_000_000),
    payload=tensor.tobytes(),      # H * K * 4 bytes, little-endian f32
)
```

`payload` length must equal `horizon * action_dim * sizeof(dtype)`. Portal
validates this on serialize and on receive.

Portal does not negotiate a chunk schema at session start. Joint layout,
dtype choice, and horizon bounds are the caller's agreement.

## Sending and receiving

Policy side:

```python
await portal.send_action_chunk(chunk, destination="robot")
```

Robot side:

```python
def on_chunk(chunk: ActionChunk) -> None:
    data = np.frombuffer(chunk.payload, dtype=np.float32)
    data = data.reshape(chunk.horizon, chunk.action_dim)
    # splice into your local queue

portal.on_action_chunk(on_chunk)
```

Or pull the latest:

```python
chunk = portal.get_action_chunk()
```

Chunks travel over a reserved byte-stream topic. There is no 15 KB ceiling
(RPC's limit does not apply) and no base64 overhead.

## The request pattern

Portal does not prescribe one. The example uses the simplest approach:

1. Robot runs its control loop at `control_hz`, pulling from a local queue.
2. When the queue's remaining actions fall below a threshold, the robot
   fires `portal.perform_rpc("request_chunk", payload=json({current_step,
   d_hint, ...}))`.
3. The policy's RPC handler synthesizes a chunk and calls
   `portal.send_action_chunk(chunk, destination=caller_identity)`, then
   returns `"ok"` from the RPC.
4. The robot's `on_action_chunk` fires when the chunk arrives and splices
   at `real_delay = current_step - chunk_start_step`.

The cursor stream I considered in the design is not needed: the policy
owns its own emitted chunks, the robot tells the policy where it is in
the RPC payload, and both sides measure latency locally.

See [examples/python/rtc](../examples/python/rtc) for a complete working
demo including a Modal deployment for the policy side.

## Generic byte streams

If you need reliable topic-keyed binary delivery for something other than
chunks (calibration blobs, waypoint graphs, serialized state snapshots),
use the same primitive without the typed wrapper:

```python
await portal.send_bytes("my-topic", data, destination=None)

def handle(sender: str, data: bytes) -> None:
    ...
portal.register_byte_stream_handler("my-topic", handle)
```

`register_byte_stream_handler` replaces any prior handler on the same
topic. The reserved topic `portal_action_chunk` belongs to
`send_action_chunk` and `on_action_chunk`; do not register a user handler
there.

## What Portal does not do

- No chunk buffering or queue management.
- No splice or eviction policy.
- No interpolation from policy rate to control rate.
- No `prev_chunk_left_over` slicing or soft prefix guidance.
- No latency tracking beyond what RPC round-trips already expose.

All of the above are one-or-two-screen problems in user code, and every
project solves them differently. Portal stays out of the way.
