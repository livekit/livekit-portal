use crate::dtype::DType;
use crate::types::{Role, SyncConfig};

/// A single field declaration: name plus on-wire dtype.
pub type FieldSchema = (String, DType);

/// Configuration for a Portal session. Built incrementally before connecting.
#[derive(Debug, Clone)]
pub struct PortalConfig {
    pub(crate) session: String,
    pub(crate) role: Role,
    pub(crate) video_tracks: Vec<String>,
    pub(crate) state_schema: Vec<FieldSchema>,
    pub(crate) action_schema: Vec<FieldSchema>,
    pub(crate) state_reliable: bool,
    pub(crate) action_reliable: bool,
    pub(crate) fps: u32,
    pub(crate) slack: u32,
    pub(crate) tolerance: f32,
    pub(crate) ping_ms: u64,
}

impl PortalConfig {
    pub fn new(session: impl Into<String>, role: Role) -> Self {
        Self {
            session: session.into(),
            role,
            video_tracks: Vec::new(),
            state_schema: Vec::new(),
            action_schema: Vec::new(),
            state_reliable: true,
            action_reliable: true,
            fps: 30,
            slack: 5,
            tolerance: 1.5,
            ping_ms: 1000,
        }
    }

    pub fn add_video(&mut self, name: impl Into<String>) {
        self.video_tracks.push(name.into());
    }

    /// Declare state fields with per-field dtype. Order is significant and
    /// must match on both peers.
    pub fn add_state_typed(&mut self, schema: &[(&str, DType)]) {
        self.state_schema.extend(schema.iter().map(|(n, d)| (n.to_string(), *d)));
    }

    /// Declare action fields with per-field dtype. Order is significant and
    /// must match on both peers.
    pub fn add_action_typed(&mut self, schema: &[(&str, DType)]) {
        self.action_schema.extend(schema.iter().map(|(n, d)| (n.to_string(), *d)));
    }

    /// Unified observation rate (set to the video capture rate if state and
    /// video differ). Drives `search_range = tolerance/fps`.
    pub fn set_fps(&mut self, fps: u32) {
        assert!(fps > 0, "fps must be > 0");
        self.fps = fps;
    }

    /// How far (in tick intervals at `fps`) a state may reach when matching
    /// a video frame. `search_range = tolerance / fps`.
    ///
    /// - `0.5` (tight): state only matches a frame within ±half a tick.
    ///   One lost frame → one dropped observation. Lowest misalignment risk.
    /// - `1.5` (default, widened): state matches its own frame, or falls
    ///   back to T±1 if its native frame was lost. Preserves observations
    ///   at the cost of occasional ±1-tick misalignment. A fair-share check
    ///   prevents an earlier state from stealing a frame closer to a later
    ///   state already in the buffer.
    /// - `> 2.0`: state may match T±2 frames. Higher recovery, higher
    ///   misalignment risk. Rarely worth it.
    ///
    /// Values must be in `(0, ∞)`. Defaults to `1.5`.
    pub fn set_tolerance(&mut self, ticks: f32) {
        assert!(ticks > 0.0, "tolerance must be > 0");
        self.tolerance = ticks;
    }

    /// Ticks of pipeline headroom — how much jitter, loss-detection latency,
    /// and consumer lag the pipeline tolerates before dropping. Applies to
    /// the per-track video sync buffer, the state sync buffer, and the
    /// pull-side observation buffer.
    pub fn set_slack(&mut self, ticks: u32) {
        assert!(ticks > 0, "slack must be > 0");
        self.slack = ticks;
    }

    pub fn set_state_reliable(&mut self, reliable: bool) {
        self.state_reliable = reliable;
    }

    pub fn set_action_reliable(&mut self, reliable: bool) {
        self.action_reliable = reliable;
    }

    /// RTT ping cadence. Set to `0` to disable active pinging on this side;
    /// the pong echo path remains active so the peer can still measure.
    pub fn set_ping_ms(&mut self, ms: u64) {
        self.ping_ms = ms;
    }

    pub fn video_tracks(&self) -> &[String] {
        &self.video_tracks
    }

    /// Ordered state field names. Derived from `state_schema`.
    pub fn state_fields(&self) -> Vec<String> {
        self.state_schema.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Ordered action field names. Derived from `action_schema`.
    pub fn action_fields(&self) -> Vec<String> {
        self.action_schema.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Full state schema (name + dtype).
    pub fn state_schema(&self) -> &[FieldSchema] {
        &self.state_schema
    }

    /// Full action schema (name + dtype).
    pub fn action_schema(&self) -> &[FieldSchema] {
        &self.action_schema
    }

    /// Derived sync config used internally by the sync buffer. Not public.
    pub(crate) fn sync_config(&self) -> SyncConfig {
        let search_range_us = (self.tolerance * 1_000_000.0 / self.fps as f32) as u64;
        SyncConfig {
            video_buffer_size: self.slack,
            state_buffer_size: self.slack,
            search_range_us,
        }
    }
}
