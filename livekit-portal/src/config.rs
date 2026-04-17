use crate::types::{Role, SyncConfig};

/// Configuration for a Portal session. Built incrementally before connecting.
#[derive(Debug, Clone)]
pub struct PortalConfig {
    pub(crate) session: String,
    pub(crate) role: Role,
    pub(crate) video_tracks: Vec<String>,
    pub(crate) state_fields: Vec<String>,
    pub(crate) action_fields: Vec<String>,
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
            state_fields: Vec::new(),
            action_fields: Vec::new(),
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

    pub fn add_state(&mut self, fields: &[&str]) {
        self.state_fields.extend(fields.iter().map(|s| s.to_string()));
    }

    pub fn add_action(&mut self, fields: &[&str]) {
        self.action_fields.extend(fields.iter().map(|s| s.to_string()));
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
