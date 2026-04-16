use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use livekit::data_track::{DataTrack, DataTrackStream, Remote};
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::serialization::deserialize_values;
use crate::sync_buffer::SyncBuffer;

type ActionCallback = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;
type StateCallback = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;

pub(crate) struct DataReceiver {
    _fields: Vec<String>,
    task_handle: JoinHandle<()>,
}

impl DataReceiver {
    /// Spawn a receiver for action (robot side) — fires callback directly, no sync.
    pub fn spawn_action(
        fields: Vec<String>,
        stream: DataTrackStream,
        callback: Arc<Mutex<Option<ActionCallback>>>,
    ) -> Self {
        let fields_clone = fields.clone();
        let handle = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(frame) = stream.next().await {
                if let Ok((_, values)) = deserialize_values(&frame.payload(), fields_clone.len()) {
                    let map: HashMap<String, f64> = fields_clone
                        .iter()
                        .zip(values.into_iter())
                        .map(|(k, v)| (k.clone(), v))
                        .collect();
                    if let Some(cb) = callback.lock().as_ref() {
                        cb(map);
                    }
                }
            }
        });
        Self {
            _fields: fields,
            task_handle: handle,
        }
    }

    /// Spawn a receiver for state (operator side) — feeds SyncBuffer.
    pub fn spawn_state(
        fields: Vec<String>,
        stream: DataTrackStream,
        sync_buffer: Arc<Mutex<SyncBuffer>>,
        raw_callback: Arc<Mutex<Option<StateCallback>>>,
    ) -> Self {
        let fields_clone = fields.clone();
        let handle = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(frame) = stream.next().await {
                if let Ok((timestamp_us, values)) =
                    deserialize_values(&frame.payload(), fields_clone.len())
                {
                    // Fire raw (unsynced) state callback
                    if let Some(cb) = raw_callback.lock().as_ref() {
                        let map: HashMap<String, f64> = fields_clone
                            .iter()
                            .zip(values.iter().copied())
                            .map(|(k, v)| (k.clone(), v))
                            .collect();
                        cb(map);
                    }

                    sync_buffer.lock().push_state(timestamp_us, values);
                }
            }
        });
        Self {
            _fields: fields,
            task_handle: handle,
        }
    }

    pub fn abort(&self) {
        self.task_handle.abort();
    }
}
