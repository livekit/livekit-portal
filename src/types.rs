use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Role {
    Robot,
    Operator,
}

impl Role {
    pub fn identity(&self) -> &'static str {
        match self {
            Role::Robot => "robot",
            Role::Operator => "operator",
        }
    }
}

/// A synchronized observation: one state matched with one frame from every registered video track.
#[derive(Debug, uniffi::Record)]
pub struct Observation {
    pub state: HashMap<String, f64>,
    pub frames: HashMap<String, VideoFrameData>,
    pub timestamp_us: u64,
}

/// Decoded video frame data, owned and FFI-safe.
#[derive(Debug, Clone, uniffi::Record)]
pub struct VideoFrameData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub timestamp_us: u64,
}

/// A video frame tagged with its sender timestamp, held in the sync buffer.
#[derive(Debug)]
pub(crate) struct TimestampedFrame {
    pub timestamp_us: u64,
    pub frame: Arc<VideoFrameData>,
}

/// A state sample tagged with its sender timestamp, held in the sync buffer.
#[derive(Debug)]
pub(crate) struct TimestampedState {
    pub timestamp_us: u64,
    pub values: Vec<f64>,
}

/// Sync configuration with sensible defaults for robotics.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncConfig {
    pub video_buffer_size: u32,
    pub state_buffer_size: u32,
    pub search_range_us: u64,
    pub observation_buffer_size: u32,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            video_buffer_size: 30,
            state_buffer_size: 30,
            search_range_us: 30_000,
            observation_buffer_size: 10,
        }
    }
}
