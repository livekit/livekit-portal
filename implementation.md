# Implementation Plan

## LiveKit SDK Primitives We Use

| Portal concept | LiveKit primitive | Notes |
|---|---|---|
| Video publish | `NativeVideoSource` + `LocalVideoTrack` + `capture_frame()` | One per camera. User timestamp set via `FrameMetadata` packet trailer. |
| Video timestamp | `PacketTrailerFeatures` + `FrameMetadata` ([PR #890](https://github.com/livekit/rust-sdks/pull/890)) | Embeds `user_timestamp` (u64 µs) as a binary trailer on encoded frames. Survives full WebRTC pipeline including E2EE. |
| Video subscribe | `NativeVideoStream` (async `Stream` trait) | One per subscribed track. Yields `BoxVideoFrame` with `frame_metadata.user_timestamp`. |
| State/action publish | `LocalParticipant::publish_data(DataPacket)` | Configurable reliability per topic (`state_reliable`, `action_reliable`, both default `true`). Topic-based routing (`portal_state`, `portal_action`). |
| State/action receive | `RoomEvent::DataReceived` | Dispatched synchronously by topic in the room event handler. No async task needed. |
| Action chunk publish | `LocalParticipant::send_bytes` (byte stream) | One stream per send. Topic `portal_action_chunk`. Reliable by design. Bypasses the 15 KB data-packet cap that small data packets impose. |
| Action chunk receive | `RoomEvent::ByteStreamOpened` | Dispatcher takes the reader on a matching topic, spawns a task that calls `read_all().await` and routes the payload through the chunk-fingerprint table. |
| Frame video publish | `LocalParticipant::send_bytes` (byte stream) | One stream per frame. Topic `portal_frame_video`. Per-track `FrameVideoPublisher` owns an mpsc-fed drainer task; encode + framing fold into one `Vec<u8>` allocation per frame. |
| Frame video receive | `RoomEvent::ByteStreamOpened` | Same shape as chunks. Dispatcher routes the payload through `FrameVideoTrackEntry` (spec + slots + metrics fused, one HashMap lookup per frame). For RAW the codec payload is a `Bytes::slice` of the byte-stream Vec — zero-copy through to `VideoFrameData.data`. |
| Session | LiveKit Room | `session` param maps to room name. |
| Role | Participant identity | `role` sets identity. Unique per room — prevents duplicate robots. |

## Serialization

Both sides declare the same schema upfront (`add_state("joint1", ...)`).
Field names are agreed at config time, so we don't send them on every
packet. Per-field dtype is declared too; the wire width is the dtype's
byte size, not always 8.

### State wire format (data packet, topic `portal_state`)

```
[u32 schema_fingerprint][u64 timestamp_us][field0 bytes][field1 bytes]...
```

Header is 12 bytes. `schema_fingerprint` is FNV-1a over ordered
`(name, dtype_tag)` pairs — both peers compute the same number; mismatched
fingerprints drop the packet with a one-shot warn.

### Action wire format (data packet, topic `portal_action`)

```
[u32 action_fingerprint][u64 timestamp_us][u64 in_reply_to_ts_us][field0 bytes]...
```

Header is 20 bytes. `action_fingerprint = schema_fingerprint XOR ACTION_STREAM_TAG`
so a v2 peer never silently mis-parses a v1 peer's 12-byte-header packet —
the xor'd tag baked into the fingerprint forces SchemaMismatch on a version
skew. `in_reply_to_ts_us = 0` is the wire sentinel for "no correlation"
(epoch-µs timestamps are never literally zero in practice).

### Chunk wire format (byte stream, topic `portal_action_chunk`)

Chunks ship as LiveKit **byte streams** rather than data packets, because
chunk payloads can exceed the 15 KB data-packet limit easily. Byte streams
are reliable and ordered; we serialize sends through one in-flight stream
per chunk publisher to keep receive order.

```
[u32 chunk_fingerprint][u64 timestamp_us][u64 in_reply_to_ts_us][row 0][row 1]...[row H-1]
```

Where each row is the per-field values in declared field order, packed at
each field's dtype width. Total payload is `20 + horizon * sum(field.size_bytes)`.

`chunk_fingerprint` hashes name + horizon + ordered fields, then xors a chunk
stream tag. Multiple chunks on one Portal are dispatched by fingerprint —
the receiver looks up the matching `ChunkSlot` and decodes against its
schema.

### Frame video wire format (byte stream, topic `portal_frame_video`)

One byte stream per frame. The header carries the codec id, dimensions,
timestamp, and track name; the rest of the payload is whatever the codec
emitted.

```
[u8 version = 1][u8 codec_id][u16 width][u16 height][u64 timestamp_us]
[u16 track_name_len][u8 × track_name_len name][u8 × N codec payload]
```

Header is 16 bytes plus the name. `codec_id` is `0=RAW`, `1=PNG`, `2=MJPEG`.
A single topic multiplexes every declared frame-video track on a Portal —
dispatch is by track-name lookup into `frame_video_entries`, not by
fingerprint, because the receiver can fail-fast on undeclared names rather
than tolerate schema drift the way action chunks do.

Track-name length is capped at 256 bytes on send and receive, so a
malformed/forged header cannot make the receiver burn CPU on a 64 KB-name
parse before bailing.

For RAW, the codec payload is byte-for-byte the input RGB. The receiver
slices the byte-stream `Bytes` at the post-header offset to yield
`VideoFrameData.data` with zero extra allocation. PNG and MJPEG go
through `image` crate decoders.

The byte-stream transport has a latency floor of `~1ms + 2ms × ⌈size /
15KB⌉` set by the SCTP data channel drain rate inside libwebrtc. Each
chunk is one `tx.send_receive(packet).await` round-trip into the
engine. See [docs/frame-video.md](docs/frame-video.md) for the
codec/fps trade-off table.

## Components

### 1. `PortalConfig`

Holds the declared schema before connection. Built up by `add_video`, `add_state`, `add_action` calls.

```rust
struct PortalConfigData {
    session: String,
    role: Role,                    // Robot or Operator
    video_tracks: Vec<String>,     // ordered camera names
    state_fields: Vec<String>,     // ordered field names
    action_fields: Vec<String>,    // ordered field names
    state_reliable: bool,          // default true
    action_reliable: bool,         // default true
    sync_config: SyncConfig,       // video_buffer, state_buffer, search_range, observation_buffer
}
```

### 2. `Portal`

Main struct. Owns the LiveKit room connection and all sub-components. Created from `PortalConfig`.

```rust
struct Portal {
    config: PortalConfigData,
    room: Room,
    // Role determines which of these are active:
    video_publishers: HashMap<String, VideoPublisher>,   // Robot only
    state_publisher: Option<DataPublisher>,               // Robot only
    action_publisher: Option<DataPublisher>,              // Operator only
    video_receivers: HashMap<String, VideoReceiver>,      // Operator only
    sync_buffer: Option<SyncBuffer>,                      // Operator only
}
```

On `connect()`:
- Robot: creates `VideoPublisher` per camera, `DataPublisher` for state (reliable `publish_data` with topic `portal_state`)
- Operator: creates `SyncBuffer`, `DataPublisher` for action (reliable `publish_data` with topic `portal_action`), waits for `RoomEvent::TrackSubscribed` to create `VideoReceiver` per matched camera name
- Both: room event handler dispatches `RoomEvent::DataReceived` by topic for state/action receive

### 3. `VideoPublisher`

Wraps one `NativeVideoSource` + `LocalVideoTrack`.

```rust
struct VideoPublisher {
    name: String,
    source: NativeVideoSource,
    track: LocalVideoTrack,
}
```

- `send_frame(buffer, timestamp_us)`: wraps buffer in `VideoFrame`, sets `frame_metadata = Some(FrameMetadata { user_timestamp: Some(ts), frame_id: None })`, calls `source.capture_frame()`. The `user_timestamp` is embedded as a packet trailer (PR #890) that survives encoding/decoding.
Track is published with `packet_trailer_features: PacketTrailerFeatures { user_timestamp: true, frame_id: false }` to enable the trailer.

Frame input: user provides raw pixel data. Portal wraps it in an `I420Buffer` (or accepts a pre-built `VideoFrame`). The Python layer would accept numpy arrays and convert.

### 4. `DataPublisher`

Wraps `LocalParticipant::publish_data` for reliable state/action delivery.

```rust
struct DataPublisher {
    fields: Vec<String>,           // schema
    topic: String,                 // "portal_state" or "portal_action"
    reliable: bool,                // from config.state_reliable / config.action_reliable
    local_participant: LocalParticipant,
}
```

- `send(values: &[f64], timestamp_us: Option<u64>)`: serializes to `[timestamp][f64...]`, sends via `publish_data(DataPacket { payload, topic, reliable: self.reliable })`
- If no custom timestamp, uses `SystemTime::now()` converted to µs
- Fire-and-forget: `publish_data` is async but spawned as a task to keep `send` synchronous

### 5. `VideoReceiver`

Wraps one `NativeVideoStream`. Runs an async task that pulls frames and pushes them into the `SyncBuffer`.

```rust
struct VideoReceiver {
    name: String,
    stream: NativeVideoStream,
    // Feeds into SyncBuffer
}
```

Each received `BoxVideoFrame` carries `frame_metadata.user_timestamp` — the sender's system time embedded via the packet trailer. This is the key used for sync matching (not `timestamp_us`, which is the RTP capture timestamp and not user-controllable end-to-end).

### 6. Data Receive (no dedicated struct)

Handled synchronously in the room event handler via `RoomEvent::DataReceived`. The `handle_data_received` function dispatches by topic:

- `portal_action` (robot receives): deserializes and fires `on_action` callback directly
- `portal_state` (operator receives): deserializes, fires raw `on_state` callback, and pushes into `SyncBuffer`

No async task or dedicated receiver struct needed — data arrives as room events.

### 7. `SyncBuffer`

The core synchronization engine. Lives on the operator side only.

```rust
struct SyncBuffer {
    video_buffers: HashMap<String, VecDeque<Arc<VideoFrameData>>>,  // per track
    state_buffer: VecDeque<(u64, Vec<f64>)>,  // (timestamp_us, values)
    state_fields: Vec<String>,
    config: SyncConfig,
    observation_cb: Option<Box<dyn Fn(Observation)>>,
    drop_cb: Option<Box<dyn Fn(Vec<HashMap<String, f64>>)>>,
}

struct Observation {
    state: HashMap<String, f64>,
    frames: HashMap<String, VideoFrameData>,  // camera_name -> owned frame data
    timestamp_us: u64,
}
```

No wrapper types — `Arc<VideoFrameData>` stores frames directly (VideoFrameData already has `timestamp_us`), and state is a `(u64, Vec<f64>)` tuple.

**Sync algorithm** (runs whenever a new state or frame arrives):

```
for each state in state_buffer (oldest first):
    for each video track:
        find frame in that track's buffer where |frame.timestamp_us - state.timestamp_us| < search_range
        pick the frame with minimum delta
        if no frame found for this track:
            this state cannot be synced with a fresh frame
            if config.reuse_stale_frames AND last_emitted_frames[track] is Some:
                use last emitted frame as a stale fallback (do NOT drain buffer)
            else if all frames in this track's buffer are NEWER than state + search_range:
                state is unsyncable — drop it and all older states
                fire drop callback
            else:
                wait for more frames (break, try again later)
            break
    if all tracks matched (fresh or stale):
        form Observation
        for tracks with a fresh match:
            remove matched frames and all older frames from that buffer
            update last_emitted_frames[track] = that frame
        for tracks using stale reuse:
            leave buffer and last_emitted_frames untouched
        remove this state from state buffer
        push Observation to observation buffer
        fire observation callback
```

Stale reuse is opt-in (`config.reuse_stale_frames`, default off). When on, video "freezes" on the last good frame during loss while every state still turns into an observation. See `docs/synchronization.md` for the full state table.

### 8. Language Bindings

Two layers: UniFFI for simple types/methods, C ABI event callback for async events (matching livekit-ffi's pattern).

#### UniFFI (types + sync methods)

Interface defined via UniFFI proc macros (`#[uniffi::export]`, `#[derive(uniffi::Record)]`, etc.) — no UDL file. Used for:

- Types: `Role` (Enum), `SyncConfig`/`VideoFrameData`/`Observation` (Record), `PortalError` (flat Error)
- Objects: `PortalConfig`, `Portal` with exported methods
- Sync methods: `send_video_frame`, `send_state`, `send_action`
- Async methods: `connect`, `disconnect` (UniFFI async support, backed by tokio)
- `Dict[str, float]` maps to UniFFI's `HashMap<String, f64>`
- Frame data (numpy arrays ↔ raw bytes) needs a thin conversion layer on the Python side

UniFFI does **not** handle callbacks. UniFFI callback interfaces have cross-module `FfiConverterArc` limitations that make them unreliable for this use case.

#### C ABI Event Callback (async events → Python)

This follows the same pattern as `livekit-ffi`. The Rust library is **not** aware of this layer — it uses its normal closure-based callback API internally. The C ABI layer is a thin bridge between Rust closures and the foreign language.

**Pattern (same as livekit-ffi):**

1. Python registers a single C function pointer during init: `type FfiCallbackFn = unsafe extern "C" fn(*const u8, usize)`
2. Rust serializes events as bytes and calls this one function
3. Python deserializes and dispatches to user-registered Python callbacks

```
┌─────────────┐      closures       ┌──────────────┐    C callback    ┌────────┐
│ Portal core │  ──────────────────► │ FFI bridge   │ ────────────────►│ Python │
│ (Rust)      │  on_action, etc.    │ (cabi.rs)    │  serialize +     │        │
│             │                     │              │  fn(*u8, usize)  │        │
└─────────────┘                     └──────────────┘                  └────────┘
```

**Event wire format**: a tag byte followed by payload.

| Tag | Event | Payload |
|-----|-------|---------|
| `0x01` | action | `[u8 tag][f64 val1][f64 val2]...[u64 in_reply_to_ts_us]` ordered by declared action fields, with `0` sentinel for "no correlation" |
| `0x02` | observation | `[u8 tag][u64 timestamp_us][u32 n_fields][f64 val1]...[u32 n_tracks][per track: u32 name_len][name bytes][u32 width][u32 height][u32 data_len][I420 bytes]...` |
| `0x03` | state | `[u8 tag][f64 val1][f64 val2]...` ordered by declared state fields |
| `0x04` | video | `[u8 tag][u32 name_len][name bytes][u32 width][u32 height][u32 data_len][I420 bytes][u64 timestamp_us]` |
| `0x05` | drop | `[u8 tag][u32 n_dropped][per dropped: [f64 val1][f64 val2]...]` |
| `0x06` | action_chunk | `[u8 tag][u32 name_len][name bytes][u32 horizon][per row of len `horizon`: [f64 val1]...[f64 valN]][u64 timestamp_us][u64 in_reply_to_ts_us]` |

All values little-endian.

**Rust FFI bridge** (`src/cabi.rs`):

```rust
/// Registered once by Python during init. Must be threadsafe and must not block.
pub type PortalEventCallbackFn = unsafe extern "C" fn(*const u8, usize);

static EVENT_CB: OnceLock<PortalEventCallbackFn> = OnceLock::new();

/// Called by Python once before creating any Portal.
#[no_mangle]
pub unsafe extern "C" fn portal_set_event_callback(cb: PortalEventCallbackFn) {
    EVENT_CB.set(cb).ok();
}

fn send_event(data: &[u8]) {
    if let Some(cb) = EVENT_CB.get() {
        unsafe { cb(data.as_ptr(), data.len()) };
    }
}
```

When the Portal's Rust closure callbacks fire (e.g. `on_action`), the FFI bridge serializes the event and calls `send_event`. The Portal core never knows about the C ABI — it just sees normal Rust closures.

**Python side** (`python/livekit_portal/__init__.py`):

```python
import ctypes
from livekit_portal._bindings import Portal as _Portal, PortalConfig, Role, ...

# Load the cdylib
_lib = ctypes.CDLL("liblivekit_portal.dylib")

# Register the C callback
CALLBACK_TYPE = ctypes.CFUNCTYPE(None, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t)

_user_callbacks = {}  # tag -> python callable

def _on_event(ptr, length):
    data = bytes(ctypes.cast(ptr, ctypes.POINTER(ctypes.c_uint8 * length)).contents)
    tag = data[0]
    payload = data[1:]
    if tag in _user_callbacks:
        _user_callbacks[tag](payload)  # deserialize + dispatch

_cb_ref = CALLBACK_TYPE(_on_event)  # prevent GC
_lib.portal_set_event_callback(_cb_ref)

class Portal:
    def __init__(self, config):
        self._inner = _Portal(config)

    def on_action(self, callback):
        _user_callbacks[0x01] = lambda payload: callback(_deserialize_action(payload))
        self._inner._register_action_bridge()  # wires the Rust closure → C ABI

    # ... etc
```

**Key principle**: the Rust Portal core is callback-agnostic. It exposes `on_action(impl Fn)` etc. The FFI bridge registers closures that serialize + call the C function pointer. Python deserializes and dispatches. This keeps the Rust library clean and testable without any FFI concerns.

## Parameter Flow

```
User code (Python)
    │
    ▼
portal(session, role)           → PortalConfig { session, role }
    │
add_video("camera1")            → config.video_tracks.push("camera1")
add_state("j1", "j2", "j3")    → config.state_fields = ["j1", "j2", "j3"]
add_action("j1", "j2", "j3")   → config.action_fields = ["j1", "j2", "j3"]
set_video_buffer(30)            → config.video_buffer_size = 30
set_search_range(30)            → config.search_range_us = 30_000
    │
    ▼
connect()                       → Room::connect(url, token)
                                → based on role, create publishers
                                → Robot: publish video tracks, create DataPublisher for state
                                → Operator: create SyncBuffer, create DataPublisher for action
                                → Both: room event handler dispatches DataReceived + TrackSubscribed
    │
    ▼
send_video_frame("cam1", frame) → VideoPublisher["cam1"].send_frame(frame, now_us())
send_state({...})               → DataPublisher.send(values, now_us()) via publish_data(reliable)
send_action({...})              → DataPublisher.send(values, now_us()) via publish_data(reliable)
    │
    ▼
on_action(cb)                   → callback stored, fired on DataReceived(topic="portal_action")
on_observation(cb)              → SyncBuffer observation callback
on_drop(cb)                     → SyncBuffer drop callback
```

## Crate Structure

```
livekit-portal/
├── Cargo.toml
├── src/
│   ├── lib.rs              // pub mod everything, UniFFI scaffolding
│   ├── portal.rs           // Portal struct, connect(), role-based setup, room event handler
│   ├── config.rs           // PortalConfig (UniFFI Object), PortalConfigData
│   ├── video.rs            // VideoPublisher + VideoReceiver + frame conversion helpers
│   ├── data.rs             // DataPublisher (reliable publish_data) + handle_data_received
│   ├── sync_buffer.rs      // SyncBuffer, sync algorithm, Observation
│   ├── serialization.rs    // compact binary wire format for state/action
│   ├── types.rs            // Role, Observation, VideoFrameData, SyncConfig
│   └── error.rs            // PortalError
├── python/
│   └── livekit_portal/     // generated by UniFFI + thin helpers for numpy conversion
```

## Open Questions

1. **Frame input format on Python side**: accept numpy RGBA/RGB arrays and convert to I420 internally? Or require I420?

## Resolved

- **Room token generation**: user provides the token. Portal calls `Room::connect(url, token)` directly.
- **Video encoding options**: sensible defaults for robotics (H.264, no simulcast). Not configurable for now.
- **Data transport**: uses reliable `publish_data` (SCTP) for lossless, ordered state/action delivery. Not lossy data tracks.
- **Reconnection**: `RoomEvent::Reconnected` flushes sync buffers. LiveKit SDK handles reconnection internally.
