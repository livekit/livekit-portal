use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use livekit::data_track::{DataTrack, DataTrackFrame, DataTrackStream, Local, PushFrameError};
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::error::{PortalError, PortalResult};
use crate::serialization::{deserialize_values, serialize_values};
use crate::sync_buffer::SyncBuffer;
use crate::types::to_field_map;
use crate::video::now_us;

// --- Publisher ---

pub(crate) struct DataPublisher {
    fields: Vec<String>,
    track: DataTrack<Local>,
}

impl DataPublisher {
    pub fn new(fields: Vec<String>, track: DataTrack<Local>) -> Self {
        Self { fields, track }
    }

    pub fn send(&self, values: &[f64], timestamp_us: Option<u64>) -> PortalResult<()> {
        if values.len() != self.fields.len() {
            return Err(PortalError::WrongValueCount {
                expected: self.fields.len(),
                got: values.len(),
            });
        }
        let ts = timestamp_us.unwrap_or_else(now_us);
        let payload = serialize_values(ts, values);
        let frame = DataTrackFrame::new(Bytes::from(payload));
        self.track
            .try_push(frame)
            .map_err(|e: PushFrameError| PortalError::DataTrack(e.to_string()))
    }

    /// Send from a HashMap, reordering to declared field order. Missing fields default to 0.0.
    pub fn send_map(
        &self,
        map: &HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let values: Vec<f64> =
            self.fields.iter().map(|name| *map.get(name).unwrap_or(&0.0)).collect();
        self.send(&values, timestamp_us)
    }
}

// --- Receiver ---

type DataCb = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;

pub(crate) struct DataReceiver {
    task_handle: JoinHandle<()>,
}

impl DataReceiver {
    /// Spawn a receiver for action (robot side) — fires callback directly, no sync.
    pub fn spawn_action(
        fields: Vec<String>,
        stream: DataTrackStream,
        callback: Arc<Mutex<Option<DataCb>>>,
    ) -> Self {
        let handle = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(frame) = stream.next().await {
                if let Ok((_, values)) = deserialize_values(&frame.payload(), fields.len()) {
                    let map = to_field_map(&fields, values);
                    if let Some(cb) = callback.lock().as_ref() {
                        cb(map);
                    }
                }
            }
        });
        Self { task_handle: handle }
    }

    /// Spawn a receiver for state (operator side) — feeds SyncBuffer.
    pub fn spawn_state(
        fields: Vec<String>,
        stream: DataTrackStream,
        sync_buffer: Arc<Mutex<SyncBuffer>>,
        raw_callback: Arc<Mutex<Option<DataCb>>>,
    ) -> Self {
        let handle = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(frame) = stream.next().await {
                if let Ok((timestamp_us, values)) =
                    deserialize_values(&frame.payload(), fields.len())
                {
                    if let Some(cb) = raw_callback.lock().as_ref() {
                        cb(to_field_map(&fields, values.clone()));
                    }
                    sync_buffer.lock().push_state(timestamp_us, values);
                }
            }
        });
        Self { task_handle: handle }
    }

    pub fn abort(&self) {
        self.task_handle.abort();
    }
}
