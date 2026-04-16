use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Robot,
    Operator,
}

/// A synchronized observation: one state matched with one frame from every registered video track.
#[derive(Debug, Clone)]
pub struct Observation {
    pub state: HashMap<String, f64>,
    pub frames: HashMap<String, VideoFrameData>,
    pub timestamp_us: u64,
}

/// Decoded video frame data, owned.
#[derive(Debug, Clone)]
pub struct VideoFrameData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub timestamp_us: u64,
}

/// Sync configuration with sensible defaults for robotics.
#[derive(Debug, Clone, Copy)]
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

/// Build a field-name → value HashMap from ordered fields and values.
pub(crate) fn to_field_map(fields: &[String], values: Vec<f64>) -> HashMap<String, f64> {
    fields.iter().zip(values).map(|(k, v)| (k.clone(), v)).collect()
}
