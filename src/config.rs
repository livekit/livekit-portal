use crate::types::{Role, SyncConfig};

/// Configuration for a Portal session. Built incrementally before connecting.
#[derive(Debug, Clone)]
pub struct PortalConfig {
    pub(crate) session: String,
    pub(crate) role: Role,
    pub(crate) video_tracks: Vec<String>,
    pub(crate) state_fields: Vec<String>,
    pub(crate) action_fields: Vec<String>,
    pub(crate) sync_config: SyncConfig,
}

impl PortalConfig {
    pub fn new(session: impl Into<String>, role: Role) -> Self {
        Self {
            session: session.into(),
            role,
            video_tracks: Vec::new(),
            state_fields: Vec::new(),
            action_fields: Vec::new(),
            sync_config: SyncConfig::default(),
        }
    }

    pub fn add_video(&mut self, name: impl Into<String>) {
        self.video_tracks.push(name.into());
    }

    pub fn add_state(&mut self, fields: &[&str]) {
        self.state_fields = fields.iter().map(|s| s.to_string()).collect();
    }

    pub fn add_action(&mut self, fields: &[&str]) {
        self.action_fields = fields.iter().map(|s| s.to_string()).collect();
    }

    pub fn set_video_buffer(&mut self, size: usize) {
        self.sync_config.video_buffer_size = size;
    }

    pub fn set_state_buffer(&mut self, size: usize) {
        self.sync_config.state_buffer_size = size;
    }

    pub fn set_search_range_ms(&mut self, ms: u64) {
        self.sync_config.search_range_us = ms * 1000;
    }

    pub fn set_observation_buffer(&mut self, size: usize) {
        self.sync_config.observation_buffer_size = size;
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn role(&self) -> Role {
        self.role
    }
}
