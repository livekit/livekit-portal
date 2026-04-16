use std::sync::Arc;

use parking_lot::Mutex;

use crate::types::{Role, SyncConfig};

/// Internal config data, cloneable for passing to Portal.
#[derive(Debug, Clone)]
pub(crate) struct PortalConfigData {
    pub role: Role,
    pub video_tracks: Vec<String>,
    pub state_fields: Vec<String>,
    pub action_fields: Vec<String>,
    pub sync_config: SyncConfig,
}

/// Configuration for a Portal session. Built incrementally before connecting.
#[derive(uniffi::Object)]
pub struct PortalConfig {
    data: Mutex<PortalConfigData>,
}

#[uniffi::export]
impl PortalConfig {
    #[uniffi::constructor]
    pub fn new(role: Role) -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(PortalConfigData {
                role,
                video_tracks: Vec::new(),
                state_fields: Vec::new(),
                action_fields: Vec::new(),
                sync_config: SyncConfig::default(),
            }),
        })
    }

    pub fn add_video(&self, name: String) {
        self.data.lock().video_tracks.push(name);
    }

    pub fn add_state(&self, fields: Vec<String>) {
        self.data.lock().state_fields = fields;
    }

    pub fn add_action(&self, fields: Vec<String>) {
        self.data.lock().action_fields = fields;
    }

    pub fn set_video_buffer(&self, size: u32) {
        self.data.lock().sync_config.video_buffer_size = size;
    }

    pub fn set_state_buffer(&self, size: u32) {
        self.data.lock().sync_config.state_buffer_size = size;
    }

    pub fn set_search_range_ms(&self, ms: u64) {
        self.data.lock().sync_config.search_range_us = ms * 1000;
    }

    pub fn set_observation_buffer(&self, size: u32) {
        self.data.lock().sync_config.observation_buffer_size = size;
    }
}

impl PortalConfig {
    pub(crate) fn snapshot(&self) -> PortalConfigData {
        self.data.lock().clone()
    }
}
