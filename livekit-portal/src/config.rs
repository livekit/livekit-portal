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

    /// Unified observation rate. All sync parameters derive from this:
    /// sender captures state + frames at this rate, and `search_range = 1/(2·fps)`.
    pub fn set_fps(&mut self, fps: u32) {
        assert!(fps > 0, "fps must be > 0");
        self.fps = fps;
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

    /// Derived sync config used internally by the sync buffer and observation
    /// sink. Not part of the public API.
    pub(crate) fn sync_config(&self) -> SyncConfig {
        SyncConfig {
            video_buffer_size: self.slack,
            state_buffer_size: self.slack,
            observation_buffer_size: self.slack,
            search_range_us: 1_000_000 / (2 * self.fps as u64),
        }
    }
}
