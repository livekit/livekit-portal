use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::types::*;

type ObservationCallback = Box<dyn Fn(Observation) + Send + Sync>;
type DropCallback = Box<dyn Fn(Vec<HashMap<String, f64>>) + Send + Sync>;

pub(crate) struct SyncBuffer {
    video_buffers: HashMap<String, VecDeque<TimestampedFrame>>,
    state_buffer: VecDeque<TimestampedState>,
    state_fields: Vec<String>,
    config: SyncConfig,
    observation_callback: Option<ObservationCallback>,
    drop_callback: Option<DropCallback>,
}

impl SyncBuffer {
    pub fn new(
        video_track_names: &[String],
        state_fields: Vec<String>,
        config: SyncConfig,
    ) -> Self {
        let mut video_buffers = HashMap::new();
        for name in video_track_names {
            video_buffers.insert(name.clone(), VecDeque::new());
        }
        Self {
            video_buffers,
            state_buffer: VecDeque::new(),
            state_fields,
            config,
            observation_callback: None,
            drop_callback: None,
        }
    }

    pub fn set_observation_callback(&mut self, cb: ObservationCallback) {
        self.observation_callback = Some(cb);
    }

    pub fn set_drop_callback(&mut self, cb: DropCallback) {
        self.drop_callback = Some(cb);
    }

    pub fn push_frame(&mut self, track_name: &str, frame: Arc<VideoFrameData>) {
        if let Some(buf) = self.video_buffers.get_mut(track_name) {
            buf.push_back(TimestampedFrame { timestamp_us: frame.timestamp_us, frame });
            while buf.len() > self.config.video_buffer_size as usize {
                buf.pop_front();
            }
        }
        self.try_sync();
    }

    pub fn push_state(&mut self, timestamp_us: u64, values: Vec<f64>) {
        self.state_buffer.push_back(TimestampedState { timestamp_us, values });
        while self.state_buffer.len() > self.config.state_buffer_size as usize {
            self.state_buffer.pop_front();
        }
        self.try_sync();
    }

    pub fn clear(&mut self) {
        for buf in self.video_buffers.values_mut() {
            buf.clear();
        }
        self.state_buffer.clear();
    }

    fn try_sync(&mut self) {
        loop {
            if self.state_buffer.is_empty() {
                break;
            }

            let state_ts = self.state_buffer[0].timestamp_us;
            let mut matched_frames: HashMap<String, (usize, Arc<VideoFrameData>)> = HashMap::new();
            let mut should_drop = false;
            let mut should_wait = false;

            for (track_name, frame_buf) in &self.video_buffers {
                let mut best_idx: Option<usize> = None;
                let mut best_delta: u64 = u64::MAX;

                for (i, tf) in frame_buf.iter().enumerate() {
                    let delta = abs_diff(tf.timestamp_us, state_ts);
                    if delta < self.config.search_range_us && delta < best_delta {
                        best_delta = delta;
                        best_idx = Some(i);
                    }
                }

                if let Some(idx) = best_idx {
                    matched_frames.insert(track_name.clone(), (idx, frame_buf[idx].frame.clone()));
                } else if !frame_buf.is_empty()
                    && frame_buf
                        .iter()
                        .all(|f| f.timestamp_us > state_ts + self.config.search_range_us)
                {
                    should_drop = true;
                    break;
                } else {
                    should_wait = true;
                    break;
                }
            }

            if should_wait {
                break;
            }

            if should_drop {
                let dropped = self.drain_states_through(0);
                if let Some(ref cb) = self.drop_callback {
                    cb(dropped);
                }
                continue;
            }

            if matched_frames.len() == self.video_buffers.len() {
                let state = self.state_buffer.pop_front().unwrap();

                // Remove matched frames and all older frames from each buffer
                for (track_name, (idx, _)) in &matched_frames {
                    if let Some(buf) = self.video_buffers.get_mut(track_name) {
                        buf.drain(0..=*idx);
                    }
                }

                let state_map: HashMap<String, f64> = self
                    .state_fields
                    .iter()
                    .zip(state.values.into_iter())
                    .map(|(k, v)| (k.clone(), v))
                    .collect();

                let frame_map: HashMap<String, VideoFrameData> = matched_frames
                    .into_iter()
                    .map(|(name, (_, frame))| (name, (*frame).clone()))
                    .collect();

                let observation = Observation {
                    state: state_map,
                    frames: frame_map,
                    timestamp_us: state.timestamp_us,
                };

                if let Some(ref cb) = self.observation_callback {
                    cb(observation);
                }
            } else {
                break;
            }
        }
    }

    /// Drain state_buffer[0..=through_idx], returning dropped states as HashMaps.
    fn drain_states_through(&mut self, through_idx: usize) -> Vec<HashMap<String, f64>> {
        self.state_buffer
            .drain(0..=through_idx)
            .map(|s| {
                self.state_fields
                    .iter()
                    .zip(s.values.into_iter())
                    .map(|(k, v)| (k.clone(), v))
                    .collect()
            })
            .collect()
    }
}

fn abs_diff(a: u64, b: u64) -> u64 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc as StdArc, Mutex};

    fn make_frame(track: &str, ts: u64) -> (String, Arc<VideoFrameData>) {
        (
            track.to_string(),
            Arc::new(VideoFrameData {
                width: 2,
                height: 2,
                data: vec![0u8; 6], // 2x2 I420 = 6 bytes
                timestamp_us: ts,
            }),
        )
    }

    #[test]
    fn sync_single_track() {
        let observations = StdArc::new(Mutex::new(Vec::new()));
        let obs_clone = observations.clone();

        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string(), "j2".to_string()];
        let mut buf = SyncBuffer::new(&tracks, fields, SyncConfig::default());

        buf.set_observation_callback(Box::new(move |obs| {
            obs_clone.lock().unwrap().push(obs);
        }));

        let (name, frame) = make_frame("cam1", 1000);
        buf.push_frame(&name, frame);
        buf.push_state(1010, vec![1.0, 2.0]); // within 30ms range

        let obs = observations.lock().unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].state["j1"], 1.0);
        assert_eq!(obs[0].state["j2"], 2.0);
        assert_eq!(obs[0].timestamp_us, 1010);
    }

    #[test]
    fn sync_multi_track() {
        let observations = StdArc::new(Mutex::new(Vec::new()));
        let obs_clone = observations.clone();

        let tracks = vec!["cam1".to_string(), "cam2".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = SyncBuffer::new(&tracks, fields, SyncConfig::default());

        buf.set_observation_callback(Box::new(move |obs| {
            obs_clone.lock().unwrap().push(obs);
        }));

        let (n1, f1) = make_frame("cam1", 1000);
        buf.push_frame(&n1, f1);
        buf.push_state(1005, vec![5.0]);

        // Not synced yet -- cam2 missing
        assert_eq!(observations.lock().unwrap().len(), 0);

        let (n2, f2) = make_frame("cam2", 1002);
        buf.push_frame(&n2, f2);

        // Now both tracks matched
        assert_eq!(observations.lock().unwrap().len(), 1);
    }

    #[test]
    fn drop_unsyncable_state() {
        let drops = StdArc::new(Mutex::new(Vec::new()));
        let drops_clone = drops.clone();

        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = SyncBuffer::new(&tracks, fields, SyncConfig::default());

        buf.set_drop_callback(Box::new(move |dropped| {
            drops_clone.lock().unwrap().extend(dropped);
        }));

        // State at t=100
        buf.push_state(100, vec![1.0]);

        // Frame at t=200_000 (200ms) -- way beyond 30ms range, all frames newer
        let (name, frame) = make_frame("cam1", 200_000);
        buf.push_frame(&name, frame);

        let d = drops.lock().unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0]["j1"], 1.0);
    }

    #[test]
    fn out_of_range_waits() {
        let observations = StdArc::new(Mutex::new(Vec::new()));
        let obs_clone = observations.clone();

        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = SyncBuffer::new(&tracks, fields, SyncConfig::default());

        buf.set_observation_callback(Box::new(move |obs| {
            obs_clone.lock().unwrap().push(obs);
        }));

        // State at t=50_000 but no frames yet -- should wait
        buf.push_state(50_000, vec![1.0]);
        assert_eq!(observations.lock().unwrap().len(), 0);

        // Frame arrives in range
        let (name, frame) = make_frame("cam1", 50_010);
        buf.push_frame(&name, frame);
        assert_eq!(observations.lock().unwrap().len(), 1);
    }

    #[test]
    fn buffer_overflow_evicts_oldest() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config =
            SyncConfig { video_buffer_size: 2, state_buffer_size: 2, ..Default::default() };
        let mut buf = SyncBuffer::new(&tracks, fields, config);

        // Push 3 frames, buffer holds 2
        for ts in [100, 200, 300] {
            let (name, frame) = make_frame("cam1", ts);
            buf.push_frame(&name, frame);
        }

        // The buffer should have frames at 200 and 300 (100 evicted)
        let cam_buf = buf.video_buffers.get("cam1").unwrap();
        assert_eq!(cam_buf.len(), 2);
        assert_eq!(cam_buf[0].timestamp_us, 200);
        assert_eq!(cam_buf[1].timestamp_us, 300);
    }

    #[test]
    fn clear_flushes_all() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = SyncBuffer::new(&tracks, fields, SyncConfig::default());

        let (name, frame) = make_frame("cam1", 1000);
        buf.push_frame(&name, frame);
        buf.push_state(1000, vec![1.0]);

        buf.clear();

        assert!(buf.video_buffers.get("cam1").unwrap().is_empty());
        assert!(buf.state_buffer.is_empty());
    }
}
