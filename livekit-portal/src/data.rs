use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use livekit::prelude::*;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::dtype::DType;
use crate::error::PortalResult;
use crate::metrics::{DataStream, MetricsRegistry};
use crate::rtt::{RttService, RTT_TOPIC};
use crate::serialization::{deserialize_values, schema_fingerprint, serialize_values, DecodeError};
use crate::sync_buffer::{SyncBuffer, SyncOutput};
use crate::types::Role;
use crate::video::now_us;

// --- Publisher ---

/// Bound on the in-flight publish queue. Sized for ~10s at 100Hz so normal
/// operation never hits it. The bound exists so a stalled publish loop
/// (slow SFU, lossy link) cannot grow memory without limit; on overflow we
/// drop and warn rather than block the synchronous send path.
const PUBLISH_QUEUE_CAP: usize = 1024;

/// Publishes serialized state/action packets. Spawns a single background task
/// at construction; `send` enqueues onto an mpsc channel, preserving ordering
/// for reliable publishes and avoiding a task allocation per packet.
pub(crate) struct DataPublisher {
    /// Owned schema. Referenced by every `send_map` call; never mutated after
    /// construction.
    schema: Vec<(String, DType)>,
    /// Precomputed schema fingerprint, embedded in every outgoing packet.
    fingerprint: u32,
    topic: String,
    reliable: bool,
    tx: mpsc::Sender<DataPacket>,
    task: Option<JoinHandle<()>>,
    metrics: Arc<MetricsRegistry>,
    stream: DataStream,
    // Per-field snapshot of the last sent value, stored as f64 for lossless
    // carry-forward. `send_map` carries these forward when a caller supplies
    // only a subset of the declared fields, so partial updates stay
    // consistent with the robot's actual state. Seeded with 0.0, so fields
    // never sent resolve to 0.
    last_values: Mutex<Vec<f64>>,
    /// Field indices already reported as saturating. Each field warns at
    /// most once per publisher lifetime to keep the hot path quiet.
    warned_saturated: Mutex<HashSet<usize>>,
    /// Keys the caller sent that aren't in the schema. Logged once each so
    /// typos are visible without spamming per packet.
    warned_unknown_keys: Mutex<HashSet<String>>,
}

impl DataPublisher {
    pub fn new(
        schema: &[(String, DType)],
        topic: &str,
        reliable: bool,
        local_participant: LocalParticipant,
        metrics: Arc<MetricsRegistry>,
        stream: DataStream,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<DataPacket>(PUBLISH_QUEUE_CAP);
        let task = tokio::spawn(async move {
            while let Some(packet) = rx.recv().await {
                if let Err(e) = local_participant.publish_data(packet).await {
                    log::warn!("failed to publish data: {e}");
                }
            }
        });
        let schema = schema.to_vec();
        let fingerprint = schema_fingerprint(&schema);
        let last_values = Mutex::new(vec![0.0; schema.len()]);
        Self {
            schema,
            fingerprint,
            topic: topic.to_string(),
            reliable,
            tx,
            task: Some(task),
            metrics,
            stream,
            last_values,
            warned_saturated: Mutex::new(HashSet::new()),
            warned_unknown_keys: Mutex::new(HashSet::new()),
        }
    }

    /// Send from a HashMap, reordering to declared field order. Missing fields
    /// inherit their last sent value (0.0 if never sent) — partial updates
    /// carry forward prior state instead of silently zeroing it. Keys absent
    /// from the schema are logged once per key, then ignored.
    pub fn send_map(
        &self,
        map: &HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        self.warn_unknown_keys(map);
        let ts = timestamp_us.unwrap_or_else(now_us);
        let (payload, saturated_indices) = {
            let mut last = self.last_values.lock();
            apply_carry_forward(&self.schema, &mut last, map);
            let out = serialize_values(self.fingerprint, ts, &last, &self.schema);
            (out.payload, out.saturated_indices)
        };
        if !saturated_indices.is_empty() {
            self.warn_saturated(&saturated_indices);
        }
        let packet = DataPacket {
            payload,
            topic: Some(self.topic.clone()),
            reliable: self.reliable,
            destination_identities: Vec::new(),
        };
        match self.tx.try_send(packet) {
            Ok(()) => {
                self.metrics.bump_sent(self.stream);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                log::warn!(
                    "publish queue full for topic '{}' (cap={}); dropping packet",
                    self.topic,
                    PUBLISH_QUEUE_CAP
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Send task is gone (disconnect / drop). Silent — caller is
                // already in teardown.
            }
        }
        Ok(())
    }

    fn warn_unknown_keys(&self, map: &HashMap<String, f64>) {
        // Small schemas make a linear scan faster than a HashSet lookup.
        for key in map.keys() {
            if self.schema.iter().any(|(n, _)| n == key) {
                continue;
            }
            let mut warned = self.warned_unknown_keys.lock();
            if warned.insert(key.clone()) {
                log::warn!(
                    "topic '{}': ignoring unknown field '{}' (not in schema)",
                    self.topic,
                    key
                );
            }
        }
    }

    fn warn_saturated(&self, indices: &[usize]) {
        let mut warned = self.warned_saturated.lock();
        for &i in indices {
            if warned.insert(i) {
                let (name, dtype) = &self.schema[i];
                log::warn!(
                    "topic '{}': field '{}' saturated at {:?} (first occurrence)",
                    self.topic,
                    name,
                    dtype
                );
            }
        }
    }
}

/// Update `last` in place with values from `map` for each declared field,
/// leaving other slots untouched (carry-forward).
fn apply_carry_forward(
    schema: &[(String, DType)],
    last: &mut [f64],
    map: &HashMap<String, f64>,
) {
    for (i, (name, _)) in schema.iter().enumerate() {
        if let Some(&v) = map.get(name) {
            last[i] = v;
        }
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

pub(crate) type DataCb = Box<dyn Fn(u64, &HashMap<String, f64>) + Send + Sync>;

/// Push callback + latest-wins slot for a single data stream (state or action),
/// paired so receivers and getters share one allocation. The `u64` is the
/// sender's wall-clock timestamp in microseconds, carried on the wire.
pub(crate) struct DataSlots {
    pub cb: Mutex<Option<DataCb>>,
    pub latest: Mutex<Option<(u64, HashMap<String, f64>)>>,
    /// Peer fingerprints already reported as mismatched. Logged once per
    /// unique offender to surface schema drift without spamming.
    warned_mismatches: Mutex<HashSet<u32>>,
}

impl DataSlots {
    pub fn new() -> Self {
        Self {
            cb: Mutex::new(None),
            latest: Mutex::new(None),
            warned_mismatches: Mutex::new(HashSet::new()),
        }
    }

    /// Build the field map from the schema once, fire the callback by
    /// reference, then hand ownership to the latest slot.
    fn deliver(&self, timestamp_us: u64, schema: &[(String, DType)], values: &[f64]) {
        let map: HashMap<String, f64> = schema
            .iter()
            .zip(values.iter())
            .map(|((n, _), v)| (n.clone(), *v))
            .collect();
        if let Some(cb) = self.cb.lock().as_ref() {
            cb(timestamp_us, &map);
        }
        *self.latest.lock() = Some((timestamp_us, map));
    }

    pub fn clear(&self) {
        *self.latest.lock() = None;
    }

    fn warn_mismatch(&self, topic: &str, expected: u32, got: u32) {
        let mut warned = self.warned_mismatches.lock();
        if warned.insert(got) {
            log::warn!(
                "topic '{topic}': dropping packet with schema fingerprint 0x{got:08x} (expected 0x{expected:08x}); peer's schema disagrees with ours"
            );
        }
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
    action_schema: &[(String, DType)],
    action_fp: u32,
    state_schema: &[(String, DType)],
    state_fp: u32,
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
        (Role::Robot, "portal_action") => {
            match deserialize_values(payload, action_fp, action_schema) {
                Ok((send_ts, values)) => {
                    metrics.record_action_received(send_ts, now_us());
                    action.deliver(send_ts, action_schema, &values);
                }
                Err(DecodeError::SchemaMismatch { expected, got }) => {
                    action.warn_mismatch(topic, expected, got);
                }
                Err(DecodeError::Malformed(e)) => {
                    log::warn!("failed to deserialize action payload: {e}");
                }
            }
        }
        (Role::Operator, "portal_state") => {
            match deserialize_values(payload, state_fp, state_schema) {
                Ok((timestamp_us, values)) => {
                    metrics.record_state_received(timestamp_us, now_us());
                    state.deliver(timestamp_us, state_schema, &values);
                    if let Some(sb) = sync_buffer {
                        return sb.lock().push_state(timestamp_us, values);
                    }
                }
                Err(DecodeError::SchemaMismatch { expected, got }) => {
                    state.warn_mismatch(topic, expected, got);
                }
                Err(DecodeError::Malformed(e)) => {
                    log::warn!("failed to deserialize state payload: {e}");
                }
            }
        }
        _ => {}
    }
    SyncOutput::empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carry_forward_fills_missing_fields() {
        let schema = vec![
            ("j1".to_string(), DType::F64),
            ("j2".to_string(), DType::F64),
            ("j3".to_string(), DType::F64),
        ];
        let mut last = vec![0.0; 3];

        let m: HashMap<_, _> = [("j1".to_string(), 1.0)].into_iter().collect();
        apply_carry_forward(&schema, &mut last, &m);
        assert_eq!(last, vec![1.0, 0.0, 0.0], "unsent fields start at seed (0.0)");

        let m: HashMap<_, _> = [("j2".to_string(), 2.5)].into_iter().collect();
        apply_carry_forward(&schema, &mut last, &m);
        assert_eq!(last, vec![1.0, 2.5, 0.0], "j1 carries forward; j2 updates; j3 still at seed");

        let m: HashMap<_, _> =
            [("j1".to_string(), -1.0), ("j3".to_string(), 7.0)].into_iter().collect();
        apply_carry_forward(&schema, &mut last, &m);
        assert_eq!(last, vec![-1.0, 2.5, 7.0], "j2 carries forward when omitted; others update");
    }
}
