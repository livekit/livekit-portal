//! UniFFI wrapper around `livekit-portal`.
//!
//! The core `livekit_portal::Portal` stays free of binding concerns; this
//! crate re-exposes it as a proc-macro-annotated UniFFI surface that
//! generates Python (and, later, Swift/Kotlin) bindings directly from Rust.
//!
//! Shape:
//!   * `PortalConfig` and `Portal` are `#[uniffi::Object]`s. Constructors and
//!     methods run through UniFFI's Arc-based lifecycle.
//!   * Records (`VideoFrame`, `Observation`, `Action`, `State`, metrics)
//!     cross the boundary by value. Callbacks always own their payload.
//!   * `PortalCallbacks` is a foreign trait (`with_foreign`). The foreign
//!     side implements it once; the five closures registered into
//!     `core::Portal` fan out into its methods.
//!   * `connect`/`disconnect` are native `async` — no more request/async_id
//!     correlation.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use livekit_portal as core;

uniffi::setup_scaffolding!();

// ---------------------------------------------------------------------------
// Enums & records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Role {
    Robot,
    Operator,
}

impl From<Role> for core::Role {
    fn from(r: Role) -> Self {
        match r {
            Role::Robot => core::Role::Robot,
            Role::Operator => core::Role::Operator,
        }
    }
}

impl From<core::Role> for Role {
    fn from(r: core::Role) -> Self {
        match r {
            core::Role::Robot => Role::Robot,
            core::Role::Operator => Role::Operator,
        }
    }
}

/// Decoded video frame. Receive-side `data` is I420 planar bytes; send-side
/// callers pass packed RGB24 directly to `send_video_frame`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Observation {
    pub timestamp_us: u64,
    pub state: HashMap<String, f64>,
    pub frames: HashMap<String, VideoFrame>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Action {
    pub values: HashMap<String, f64>,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct State {
    pub values: HashMap<String, f64>,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct SyncMetrics {
    pub observations_emitted: u64,
    pub states_dropped: u64,
    pub match_delta_us_p50: Option<u64>,
    pub match_delta_us_p95: Option<u64>,
    pub last_blocker_track: Option<String>,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct TransportMetrics {
    pub frames_sent: HashMap<String, u64>,
    pub frames_received: HashMap<String, u64>,
    pub states_sent: u64,
    pub states_received: u64,
    pub actions_sent: u64,
    pub actions_received: u64,
    pub frame_jitter_us: HashMap<String, u64>,
    pub state_jitter_us: u64,
    pub action_jitter_us: u64,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct BufferMetrics {
    pub video_fill: HashMap<String, u64>,
    pub state_fill: u64,
    pub evictions: HashMap<String, u64>,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct RttMetrics {
    pub rtt_us_last: Option<u64>,
    pub rtt_us_mean: Option<u64>,
    pub rtt_us_p95: Option<u64>,
    pub pings_sent: u64,
    pub pongs_received: u64,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct PortalMetrics {
    pub sync: SyncMetrics,
    pub transport: TransportMetrics,
    pub buffers: BufferMetrics,
    pub rtt: RttMetrics,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum PortalError {
    #[error("room error: {0}")]
    Room(String),

    #[error("portal is already connected")]
    AlreadyConnected,

    #[error("portal is not connected")]
    NotConnected,

    #[error("no peer in the room")]
    NoPeer,

    #[error("room has multiple remote participants; pass destination explicitly")]
    AmbiguousPeer,

    #[error("unknown video track: {0}")]
    UnknownVideoTrack(String),

    #[error("wrong frame size: expected {expected} bytes, got {got}")]
    WrongFrameSize { expected: u64, got: u64 },

    #[error("invalid frame dimensions: {width}x{height} (must both be even)")]
    InvalidFrameDimensions { width: u32, height: u32 },

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("operation not available for role {0:?}")]
    WrongRole(Role),

    #[error("rpc error {code}: {message}")]
    Rpc { code: u32, message: String, data: Option<String> },
}

impl From<core::PortalError> for PortalError {
    fn from(e: core::PortalError) -> Self {
        match e {
            core::PortalError::Room(s) => PortalError::Room(s),
            core::PortalError::AlreadyConnected => PortalError::AlreadyConnected,
            core::PortalError::NotConnected => PortalError::NotConnected,
            core::PortalError::NoPeer => PortalError::NoPeer,
            core::PortalError::AmbiguousPeer => PortalError::AmbiguousPeer,
            core::PortalError::UnknownVideoTrack { name } => PortalError::UnknownVideoTrack(name),
            core::PortalError::WrongFrameSize { expected, got } => {
                PortalError::WrongFrameSize { expected: expected as u64, got: got as u64 }
            }
            core::PortalError::InvalidFrameDimensions { width, height } => {
                PortalError::InvalidFrameDimensions { width, height }
            }
            core::PortalError::Deserialization(s) => PortalError::Deserialization(s),
            core::PortalError::WrongRole(r) => PortalError::WrongRole(r.into()),
            core::PortalError::Rpc(e) => {
                PortalError::Rpc { code: e.code, message: e.message, data: e.data }
            }
        }
    }
}

pub type PortalResult<T> = Result<T, PortalError>;

// ---------------------------------------------------------------------------
// RPC types
// ---------------------------------------------------------------------------

/// Handler-side view of an incoming RPC invocation.
#[derive(Debug, Clone, uniffi::Record)]
pub struct RpcInvocationData {
    pub request_id: String,
    pub caller_identity: String,
    pub payload: String,
    pub response_timeout_ms: u64,
}

impl From<core::RpcInvocationData> for RpcInvocationData {
    fn from(d: core::RpcInvocationData) -> Self {
        Self {
            request_id: d.request_id,
            caller_identity: d.caller_identity,
            payload: d.payload,
            response_timeout_ms: d.response_timeout.as_millis() as u64,
        }
    }
}

/// Error raised by an RPC handler or returned from `perform_rpc`. A
/// single-variant enum to satisfy UniFFI (which requires errors to be
/// enums); foreign handlers raise `RpcError.Error(code=..., message=...,
/// data=...)` to signal failure.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RpcError {
    #[error("rpc error {code}: {message}")]
    Error { code: u32, message: String, data: Option<String> },
}

impl From<core::RpcError> for RpcError {
    fn from(e: core::RpcError) -> Self {
        RpcError::Error { code: e.code, message: e.message, data: e.data }
    }
}

impl From<RpcError> for core::RpcError {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::Error { code, message, data } => core::RpcError::new(code, message, data),
        }
    }
}

/// Foreign-implemented handler for a single RPC method.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait RpcHandler: Send + Sync {
    async fn handle(&self, data: RpcInvocationData) -> Result<String, RpcError>;
}

// ---------------------------------------------------------------------------
// Foreign callback trait — the five push events plus the drop notification.
// The foreign side implements this once per `Portal`.
// ---------------------------------------------------------------------------

#[uniffi::export(with_foreign)]
pub trait PortalCallbacks: Send + Sync {
    fn on_action(&self, action: Action);
    fn on_state(&self, state: State);
    fn on_observation(&self, observation: Observation);
    fn on_video_frame(&self, track_name: String, frame: VideoFrame);
    fn on_drop(&self, dropped: Vec<HashMap<String, f64>>);
}

// ---------------------------------------------------------------------------
// PortalConfig
// ---------------------------------------------------------------------------

#[derive(uniffi::Object)]
pub struct PortalConfig {
    inner: Mutex<core::PortalConfig>,
}

#[uniffi::export]
impl PortalConfig {
    #[uniffi::constructor]
    pub fn new(session: String, role: Role) -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(core::PortalConfig::new(session, role.into())) })
    }

    pub fn add_video(&self, name: String) {
        self.inner.lock().add_video(name);
    }

    pub fn add_state(&self, fields: Vec<String>) {
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.inner.lock().add_state(&refs);
    }

    pub fn add_action(&self, fields: Vec<String>) {
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.inner.lock().add_action(&refs);
    }

    pub fn set_fps(&self, fps: u32) {
        self.inner.lock().set_fps(fps);
    }

    pub fn set_slack(&self, ticks: u32) {
        self.inner.lock().set_slack(ticks);
    }

    pub fn set_tolerance(&self, ticks: f32) {
        self.inner.lock().set_tolerance(ticks);
    }

    pub fn set_state_reliable(&self, reliable: bool) {
        self.inner.lock().set_state_reliable(reliable);
    }

    pub fn set_action_reliable(&self, reliable: bool) {
        self.inner.lock().set_action_reliable(reliable);
    }

    pub fn set_ping_ms(&self, ms: u64) {
        self.inner.lock().set_ping_ms(ms);
    }
}

// ---------------------------------------------------------------------------
// Portal
// ---------------------------------------------------------------------------

#[derive(uniffi::Object)]
pub struct Portal {
    inner: core::Portal,
    // Held only to keep the foreign trait object alive for the lifetime of
    // the Portal — core::Portal's closures already own their own `Arc` clones.
    _callbacks: Arc<dyn PortalCallbacks>,
    state_fields: Vec<String>,
    action_fields: Vec<String>,
    video_tracks: Vec<String>,
}

#[uniffi::export(async_runtime = "tokio")]
impl Portal {
    /// Construct a Portal from a built config. Callbacks must be passed at
    /// construction — `livekit_portal::Portal` registers them internally and
    /// there's no re-register-later escape hatch on the core side.
    #[uniffi::constructor]
    pub fn new(config: Arc<PortalConfig>, callbacks: Arc<dyn PortalCallbacks>) -> Arc<Self> {
        let cfg = config.inner.lock().clone();
        let state_fields = cfg.state_fields().to_vec();
        let action_fields = cfg.action_fields().to_vec();
        let video_tracks = cfg.video_tracks().to_vec();

        let inner = core::Portal::new(cfg);

        let cb = callbacks.clone();
        inner.on_action(move |ts, values| {
            cb.on_action(Action { values: values.clone(), timestamp_us: ts });
        });
        let cb = callbacks.clone();
        inner.on_state(move |ts, values| {
            cb.on_state(State { values: values.clone(), timestamp_us: ts });
        });
        let cb = callbacks.clone();
        inner.on_observation(move |obs| {
            cb.on_observation(observation_from_core(obs));
        });
        let cb = callbacks.clone();
        inner.on_drop(move |dropped| {
            cb.on_drop(dropped);
        });
        for track in &video_tracks {
            let cb = callbacks.clone();
            let track_name = track.clone();
            inner.on_video_frame(track, move |_name, frame| {
                cb.on_video_frame(track_name.clone(), frame_from_core(frame));
            });
        }

        Arc::new(Self {
            inner,
            _callbacks: callbacks,
            state_fields,
            action_fields,
            video_tracks,
        })
    }

    pub async fn connect(&self, url: String, token: String) -> PortalResult<()> {
        self.inner.connect(&url, &token).await.map_err(Into::into)
    }

    pub async fn disconnect(&self) -> PortalResult<()> {
        self.inner.disconnect().await.map_err(Into::into)
    }

    pub fn send_video_frame(
        &self,
        track_name: String,
        rgb_data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        self.inner
            .send_video_frame(&track_name, &rgb_data, width, height, timestamp_us)
            .map_err(Into::into)
    }

    pub fn send_state(
        &self,
        values: HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        self.inner.send_state(&values, timestamp_us).map_err(Into::into)
    }

    pub fn send_action(
        &self,
        values: HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        self.inner.send_action(&values, timestamp_us).map_err(Into::into)
    }

    pub fn get_observation(&self) -> Option<Observation> {
        self.inner.get_observation().as_ref().map(observation_from_core)
    }

    pub fn get_action(&self) -> Option<Action> {
        self.inner.get_action().map(|(ts, values)| Action { values, timestamp_us: ts })
    }

    pub fn get_state(&self) -> Option<State> {
        self.inner.get_state().map(|(ts, values)| State { values, timestamp_us: ts })
    }

    pub fn get_video_frame(&self, track_name: String) -> Option<VideoFrame> {
        self.inner.get_video_frame(&track_name).as_ref().map(frame_from_core)
    }

    pub fn metrics(&self) -> PortalMetrics {
        metrics_from_core(self.inner.metrics())
    }

    pub fn reset_metrics(&self) {
        self.inner.reset_metrics();
    }

    pub fn state_fields(&self) -> Vec<String> {
        self.state_fields.clone()
    }

    pub fn action_fields(&self) -> Vec<String> {
        self.action_fields.clone()
    }

    pub fn video_tracks(&self) -> Vec<String> {
        self.video_tracks.clone()
    }

    // --- RPC ---

    /// Identity of the identified peer, or `None` if Portal has not yet
    /// seen any Portal-topic traffic from a remote participant.
    pub fn peer_identity(&self) -> Option<String> {
        self.inner.peer_identity()
    }

    /// Register a method handler. Handlers may be registered before or
    /// after `connect()`; reconnects reapply the stored set.
    pub fn register_rpc_method(&self, method: String, handler: Arc<dyn RpcHandler>) {
        self.inner.register_rpc_method(&method, wrap_foreign_handler(handler));
    }

    pub fn unregister_rpc_method(&self, method: String) {
        self.inner.unregister_rpc_method(&method);
    }

    /// Invoke a method on the peer. When `destination` is `None`, the call
    /// is routed to the identified peer, falling back to the single remote
    /// participant in the room. Timeout defaults to the SDK's 15s if
    /// `response_timeout_ms` is `None`.
    pub async fn perform_rpc(
        &self,
        destination: Option<String>,
        method: String,
        payload: String,
        response_timeout_ms: Option<u64>,
    ) -> PortalResult<String> {
        let timeout = response_timeout_ms.map(std::time::Duration::from_millis);
        self.inner
            .perform_rpc(destination.as_deref(), &method, payload, timeout)
            .await
            .map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Conversions from core types. Records own their data, so we copy frame
// bytes out of the core's `Arc<[u8]>` into `Vec<u8>` at the boundary.
// ---------------------------------------------------------------------------

fn frame_from_core(f: &core::VideoFrameData) -> VideoFrame {
    VideoFrame {
        width: f.width,
        height: f.height,
        data: f.data.to_vec(),
        timestamp_us: f.timestamp_us,
    }
}

fn observation_from_core(o: &core::Observation) -> Observation {
    Observation {
        timestamp_us: o.timestamp_us,
        state: o.state.clone(),
        frames: o.frames.iter().map(|(k, v)| (k.clone(), frame_from_core(v))).collect(),
    }
}

/// Adapt a foreign `RpcHandler` trait object to the core handler type.
/// The outer `Fn` closure is invoked once per incoming RPC; the Arc clone
/// moves an owned handle into the returned future so the closure can be
/// called again without consuming its capture.
fn wrap_foreign_handler(handler: Arc<dyn RpcHandler>) -> core::RpcHandler {
    Arc::new(move |data: core::RpcInvocationData| {
        let handler = handler.clone();
        Box::pin(async move {
            let ffi_data = RpcInvocationData::from(data);
            handler.handle(ffi_data).await.map_err(Into::into)
        })
    })
}

fn metrics_from_core(m: core::PortalMetrics) -> PortalMetrics {
    PortalMetrics {
        sync: SyncMetrics {
            observations_emitted: m.sync.observations_emitted,
            states_dropped: m.sync.states_dropped,
            match_delta_us_p50: m.sync.match_delta_us_p50,
            match_delta_us_p95: m.sync.match_delta_us_p95,
            last_blocker_track: m.sync.last_blocker_track,
        },
        transport: TransportMetrics {
            frames_sent: m.transport.frames_sent,
            frames_received: m.transport.frames_received,
            states_sent: m.transport.states_sent,
            states_received: m.transport.states_received,
            actions_sent: m.transport.actions_sent,
            actions_received: m.transport.actions_received,
            frame_jitter_us: m.transport.frame_jitter_us,
            state_jitter_us: m.transport.state_jitter_us,
            action_jitter_us: m.transport.action_jitter_us,
        },
        buffers: BufferMetrics {
            video_fill: m.buffers.video_fill.into_iter().map(|(k, v)| (k, v as u64)).collect(),
            state_fill: m.buffers.state_fill as u64,
            evictions: m.buffers.evictions,
        },
        rtt: RttMetrics {
            rtt_us_last: m.rtt.rtt_us_last,
            rtt_us_mean: m.rtt.rtt_us_mean,
            rtt_us_p95: m.rtt.rtt_us_p95,
            pings_sent: m.rtt.pings_sent,
            pongs_received: m.rtt.pongs_received,
        },
    }
}
