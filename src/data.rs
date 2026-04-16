use std::collections::HashMap;
use std::sync::Arc;

use livekit::prelude::*;
use parking_lot::Mutex;

use crate::error::{PortalError, PortalResult};
use crate::serialization::{deserialize_values, serialize_values};
use crate::sync_buffer::SyncBuffer;
use crate::types::{to_field_map, Role};
use crate::video::now_us;

// --- Publisher ---

pub(crate) struct DataPublisher {
    fields: Vec<String>,
    topic: String,
    local_participant: LocalParticipant,
}

impl DataPublisher {
    pub fn new(fields: Vec<String>, topic: &str, local_participant: LocalParticipant) -> Self {
        Self { fields, topic: topic.to_string(), local_participant }
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
            reliable: true,
            destination_identities: Vec::new(),
        };
        // publish_data is async but we fire-and-forget via spawn
        let lp = self.local_participant.clone();
        tokio::spawn(async move {
            if let Err(e) = lp.publish_data(packet).await {
                log::warn!("failed to publish data: {e}");
            }
        });
        Ok(())
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

// --- Receiver (dispatches DataReceived events) ---

pub(crate) type DataCb = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;

pub(crate) fn handle_data_received(
    payload: &[u8],
    topic: &str,
    config_role: Role,
    action_fields: &[String],
    state_fields: &[String],
    action_cb: &Arc<Mutex<Option<DataCb>>>,
    state_cb: &Arc<Mutex<Option<DataCb>>>,
    sync_buffer: &Option<Arc<Mutex<SyncBuffer>>>,
) {
    match (config_role, topic) {
        (Role::Robot, "portal_action") => {
            if let Ok((_, values)) = deserialize_values(payload, action_fields.len()) {
                let map = to_field_map(action_fields, values);
                if let Some(cb) = action_cb.lock().as_ref() {
                    cb(map);
                }
            }
        }
        (Role::Operator, "portal_state") => {
            if let Ok((timestamp_us, values)) = deserialize_values(payload, state_fields.len()) {
                if let Some(cb) = state_cb.lock().as_ref() {
                    cb(to_field_map(state_fields, values.clone()));
                }
                if let Some(sb) = sync_buffer {
                    sb.lock().push_state(timestamp_us, values);
                }
            }
        }
        _ => {}
    }
}
