use std::collections::HashMap;
use std::sync::Arc;

use livekit::prelude::*;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{PortalError, PortalResult};
use crate::serialization::{deserialize_values, serialize_values};
use crate::sync_buffer::{SyncBuffer, SyncOutput};
use crate::types::{to_field_map, Role};
use crate::video::now_us;

// --- Publisher ---

/// Publishes serialized state/action packets. Spawns a single background task
/// at construction; `send` enqueues onto an mpsc channel, preserving ordering
/// for reliable publishes and avoiding a task allocation per packet.
pub(crate) struct DataPublisher {
    fields: Vec<String>,
    topic: String,
    reliable: bool,
    tx: mpsc::UnboundedSender<DataPacket>,
    task: Option<JoinHandle<()>>,
}

impl DataPublisher {
    pub fn new(
        fields: Vec<String>,
        topic: &str,
        reliable: bool,
        local_participant: LocalParticipant,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<DataPacket>();
        let task = tokio::spawn(async move {
            while let Some(packet) = rx.recv().await {
                if let Err(e) = local_participant.publish_data(packet).await {
                    log::warn!("failed to publish data: {e}");
                }
            }
        });
        Self { fields, topic: topic.to_string(), reliable, tx, task: Some(task) }
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
        let packet = DataPacket {
            payload,
            topic: Some(self.topic.clone()),
            reliable: self.reliable,
            destination_identities: Vec::new(),
        };
        let _ = self.tx.send(packet);
        Ok(())
    }

    /// Send from a HashMap, reordering to declared field order.
    /// Missing keys default to 0.0 — callers should supply every declared field.
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

impl Drop for DataPublisher {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

// --- Receiver (dispatches DataReceived events) ---

pub(crate) type DataCb = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;

/// Handle a `DataReceived` event. Pushes into the sync buffer if applicable and
/// returns any observations/drops that resulted, for the caller to dispatch
/// outside any locks.
pub(crate) fn handle_data_received(
    payload: &[u8],
    topic: &str,
    config_role: Role,
    action_fields: &[String],
    state_fields: &[String],
    action_cb: &Mutex<Option<DataCb>>,
    state_cb: &Mutex<Option<DataCb>>,
    sync_buffer: Option<&Arc<Mutex<SyncBuffer>>>,
) -> SyncOutput {
    match (config_role, topic) {
        (Role::Robot, "portal_action") => match deserialize_values(payload, action_fields.len()) {
            Ok((_, values)) => {
                if let Some(cb) = action_cb.lock().as_ref() {
                    cb(to_field_map(action_fields, &values));
                }
            }
            Err(e) => log::warn!("failed to deserialize action payload: {e}"),
        },
        (Role::Operator, "portal_state") => match deserialize_values(payload, state_fields.len()) {
            Ok((timestamp_us, values)) => {
                if let Some(cb) = state_cb.lock().as_ref() {
                    cb(to_field_map(state_fields, &values));
                }
                if let Some(sb) = sync_buffer {
                    return sb.lock().push_state(timestamp_us, values);
                }
            }
            Err(e) => log::warn!("failed to deserialize state payload: {e}"),
        },
        _ => {}
    }
    SyncOutput::empty()
}
