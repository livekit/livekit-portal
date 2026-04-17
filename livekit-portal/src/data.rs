use std::collections::HashMap;
use std::sync::Arc;

use livekit::prelude::*;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::PortalResult;
use crate::metrics::{DataStream, MetricsRegistry};
use crate::rtt::{RttService, RTT_TOPIC};
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
    metrics: Arc<MetricsRegistry>,
    stream: DataStream,
    // Per-field snapshot of the last sent value. `send_map` carries these
    // forward when a caller supplies only a subset of the declared fields,
    // so partial updates stay consistent with the robot's actual state.
    // Seeded with 0.0, so fields never sent resolve to 0.
    last_values: Mutex<Vec<f64>>,
}

impl DataPublisher {
    pub fn new(
        fields: Vec<String>,
        topic: &str,
        reliable: bool,
        local_participant: LocalParticipant,
        metrics: Arc<MetricsRegistry>,
        stream: DataStream,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<DataPacket>();
        let task = tokio::spawn(async move {
            while let Some(packet) = rx.recv().await {
                if let Err(e) = local_participant.publish_data(packet).await {
                    log::warn!("failed to publish data: {e}");
                }
            }
        });
        let last_values = Mutex::new(vec![0.0; fields.len()]);
        Self {
            fields,
            topic: topic.to_string(),
            reliable,
            tx,
            task: Some(task),
            metrics,
            stream,
            last_values,
        }
    }

    /// Send from a HashMap, reordering to declared field order. Missing fields
    /// inherit their last sent value (0.0 if never sent) — partial updates
    /// carry forward prior state instead of silently zeroing it.
    pub fn send_map(
        &self,
        map: &HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let values = resolve_with_carry_forward(&self.fields, &mut self.last_values.lock(), map);
        self.dispatch(&values, timestamp_us)
    }

    fn dispatch(&self, values: &[f64], timestamp_us: Option<u64>) -> PortalResult<()> {
        let ts = timestamp_us.unwrap_or_else(now_us);
        let payload = serialize_values(ts, values);
        let packet = DataPacket {
            payload,
            topic: Some(self.topic.clone()),
            reliable: self.reliable,
            destination_identities: Vec::new(),
        };
        if self.tx.send(packet).is_ok() {
            self.metrics.bump_sent(self.stream);
        }
        Ok(())
    }
}

/// Update `last` in place with values from `map` for each declared field,
/// leaving other slots untouched (carry-forward). Returns a copy for sending.
fn resolve_with_carry_forward(
    fields: &[String],
    last: &mut [f64],
    map: &HashMap<String, f64>,
) -> Vec<f64> {
    for (i, name) in fields.iter().enumerate() {
        if let Some(&v) = map.get(name) {
            last[i] = v;
        }
    }
    last.to_vec()
}

impl Drop for DataPublisher {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

// --- Receiver (dispatches DataReceived events) ---

pub(crate) type DataCb = Box<dyn Fn(&HashMap<String, f64>) + Send + Sync>;

/// Push callback + latest-wins slot for a single data stream (state or action),
/// paired so receivers and getters share one allocation.
pub(crate) struct DataSlots {
    pub cb: Mutex<Option<DataCb>>,
    pub latest: Mutex<Option<HashMap<String, f64>>>,
}

impl DataSlots {
    pub fn new() -> Self {
        Self { cb: Mutex::new(None), latest: Mutex::new(None) }
    }

    /// Build the field map once, fire the callback by reference (no clone),
    /// then hand ownership to the latest slot.
    fn deliver(&self, fields: &[String], values: &[f64]) {
        let map = to_field_map(fields, values);
        if let Some(cb) = self.cb.lock().as_ref() {
            cb(&map);
        }
        *self.latest.lock() = Some(map);
    }

    pub fn clear(&self) {
        *self.latest.lock() = None;
    }
}

/// Handle a `DataReceived` event. Pushes into the sync buffer if applicable and
/// returns any observations/drops that resulted, for the caller to dispatch
/// outside any locks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_data_received(
    payload: &[u8],
    topic: &str,
    config_role: Role,
    action_fields: &[String],
    state_fields: &[String],
    action: &DataSlots,
    state: &DataSlots,
    sync_buffer: Option<&Arc<Mutex<SyncBuffer>>>,
    metrics: &MetricsRegistry,
    rtt: &RttService,
) -> SyncOutput {
    if topic == RTT_TOPIC {
        rtt.handle_packet(payload);
        return SyncOutput::empty();
    }
    match (config_role, topic) {
        (Role::Robot, "portal_action") => match deserialize_values(payload, action_fields.len()) {
            Ok((send_ts, values)) => {
                metrics.record_action_received(send_ts, now_us());
                action.deliver(action_fields, &values);
            }
            Err(e) => log::warn!("failed to deserialize action payload: {e}"),
        },
        (Role::Operator, "portal_state") => match deserialize_values(payload, state_fields.len()) {
            Ok((timestamp_us, values)) => {
                metrics.record_state_received(timestamp_us, now_us());
                state.deliver(state_fields, &values);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carry_forward_fills_missing_fields() {
        let fields =
            vec!["j1".to_string(), "j2".to_string(), "j3".to_string()];
        let mut last = vec![0.0; 3];

        let m: HashMap<_, _> = [("j1".to_string(), 1.0)].into_iter().collect();
        assert_eq!(
            resolve_with_carry_forward(&fields, &mut last, &m),
            vec![1.0, 0.0, 0.0],
            "unsent fields start at seed (0.0)"
        );

        let m: HashMap<_, _> = [("j2".to_string(), 2.5)].into_iter().collect();
        assert_eq!(
            resolve_with_carry_forward(&fields, &mut last, &m),
            vec![1.0, 2.5, 0.0],
            "j1 carries forward; j2 updates; j3 still at seed"
        );

        let m: HashMap<_, _> =
            [("j1".to_string(), -1.0), ("j3".to_string(), 7.0)].into_iter().collect();
        assert_eq!(
            resolve_with_carry_forward(&fields, &mut last, &m),
            vec![-1.0, 2.5, 7.0],
            "j2 carries forward when omitted; others update"
        );
    }

    #[test]
    fn unknown_fields_in_map_are_ignored() {
        let fields = vec!["j1".to_string()];
        let mut last = vec![0.0];
        let m: HashMap<_, _> =
            [("j1".to_string(), 3.0), ("unknown".to_string(), 99.0)].into_iter().collect();
        assert_eq!(resolve_with_carry_forward(&fields, &mut last, &m), vec![3.0]);
    }
}
