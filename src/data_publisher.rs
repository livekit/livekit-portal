use std::collections::HashMap;

use bytes::Bytes;
use livekit::data_track::{DataTrack, DataTrackFrame, Local, PushFrameError};

use crate::error::{PortalError, PortalResult};
use crate::serialization::serialize_values;
use crate::video_publisher::now_us;

pub(crate) struct DataPublisher {
    fields: Vec<String>,
    track: DataTrack<Local>,
}

impl DataPublisher {
    pub fn new(fields: Vec<String>, track: DataTrack<Local>) -> Self {
        Self { fields, track }
    }

    /// Send values in declared field order.
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

    pub fn fields(&self) -> &[String] {
        &self.fields
    }
}
