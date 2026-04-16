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
    pub(crate) sync_config: SyncConfig,
    pub(crate) ping_interval_ms: u64,
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
            sync_config: SyncConfig::default(),
            ping_interval_ms: 1000,
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

    pub fn set_video_buffer(&mut self, size: u32) {
        self.sync_config.video_buffer_size = size;
    }

    pub fn set_state_buffer(&mut self, size: u32) {
        self.sync_config.state_buffer_size = size;
    }

    pub fn set_search_range_ms(&mut self, ms: u64) {
        self.sync_config.search_range_us = ms * 1000;
    }

    pub fn set_observation_buffer(&mut self, size: u32) {
        self.sync_config.observation_buffer_size = size;
    }

    pub fn set_state_reliable(&mut self, reliable: bool) {
        self.state_reliable = reliable;
    }

    pub fn set_action_reliable(&mut self, reliable: bool) {
        self.action_reliable = reliable;
    }

    /// RTT ping cadence in milliseconds. Set to `0` to disable active pinging
    /// on this side; the pong echo path remains active regardless, so the peer
    /// can still measure its own RTT. Default: 1000ms.
    pub fn set_ping_interval_ms(&mut self, ms: u64) {
        self.ping_interval_ms = ms;
    }
}
