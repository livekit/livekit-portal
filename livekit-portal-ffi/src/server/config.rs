use std::sync::Arc;

use livekit_portal::PortalConfig;
use parking_lot::Mutex;

use super::FfiHandle;

/// Handle-registry wrapper around `livekit_portal::PortalConfig`. The core
/// config's field lists are `pub(crate)`. not readable from outside its
/// crate. so we keep a parallel copy of the declared video/state/action
/// names here, updated by each `ConfigAdd*Request` handler. The copy is used
/// later by `FfiPortal` to decode received-event ordering.
#[derive(Clone)]
pub struct FfiPortalConfig {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    config: PortalConfig,
    video_tracks: Vec<String>,
    state_fields: Vec<String>,
    action_fields: Vec<String>,
}

pub struct DeclaredFields {
    pub video_tracks: Vec<String>,
    pub state_fields: Vec<String>,
    pub action_fields: Vec<String>,
}

impl FfiPortalConfig {
    pub fn new(config: PortalConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                video_tracks: Vec::new(),
                state_fields: Vec::new(),
                action_fields: Vec::new(),
            })),
        }
    }

    pub fn add_video(&self, name: String) {
        let mut g = self.inner.lock();
        g.config.add_video(name.clone());
        g.video_tracks.push(name);
    }

    pub fn add_state(&self, fields: Vec<String>) {
        let mut g = self.inner.lock();
        let refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
        g.config.add_state(&refs);
        g.state_fields.extend(fields);
    }

    pub fn add_action(&self, fields: Vec<String>) {
        let mut g = self.inner.lock();
        let refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
        g.config.add_action(&refs);
        g.action_fields.extend(fields);
    }

    pub fn with_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut PortalConfig) -> R,
    {
        f(&mut self.inner.lock().config)
    }

    pub fn snapshot(&self) -> PortalConfig {
        self.inner.lock().config.clone()
    }

    pub fn declared_fields(&self) -> DeclaredFields {
        let g = self.inner.lock();
        DeclaredFields {
            video_tracks: g.video_tracks.clone(),
            state_fields: g.state_fields.clone(),
            action_fields: g.action_fields.clone(),
        }
    }
}

impl FfiHandle for FfiPortalConfig {}
