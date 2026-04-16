use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::types::*;

type ObservationCb = Box<dyn Fn(Observation) + Send + Sync>;
type DropCb = Box<dyn Fn(Vec<HashMap<String, f64>>) + Send + Sync>;

pub(crate) struct SyncBuffer {
    video_buffers: HashMap<String, VecDeque<Arc<VideoFrameData>>>,
    state_buffer: VecDeque<(u64, Vec<f64>)>, // (timestamp_us, values)
    observation_buffer: VecDeque<Observation>,
    state_fields: Vec<String>,
    config: SyncConfig,
    observation_cb: Option<ObservationCb>,
    drop_cb: Option<DropCb>,
}

impl SyncBuffer {
    pub fn new(
        video_track_names: &[String],
        state_fields: Vec<String>,
        config: SyncConfig,
    ) -> Self {
        let video_buffers =
            video_track_names.iter().map(|n| (n.clone(), VecDeque::new())).collect();
        Self {
            video_buffers,
            state_buffer: VecDeque::new(),
            observation_buffer: VecDeque::new(),
            state_fields,
            config,
            observation_cb: None,
            drop_cb: None,
        }
    }

    pub fn set_observation_callback(&mut self, cb: ObservationCb) {
        self.observation_cb = Some(cb);
    }

    pub fn set_drop_callback(&mut self, cb: DropCb) {
        self.drop_cb = Some(cb);
    }

    pub fn push_frame(&mut self, track_name: &str, frame: Arc<VideoFrameData>) {
        if let Some(buf) = self.video_buffers.get_mut(track_name) {
            buf.push_back(frame);
            while buf.len() > self.config.video_buffer_size as usize {
                buf.pop_front();
            }
        }
        self.try_sync();
    }

    pub fn push_state(&mut self, timestamp_us: u64, values: Vec<f64>) {
        self.state_buffer.push_back((timestamp_us, values));
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
        self.observation_buffer.clear();
    }

    /// Drain all buffered observations. Intended for pull-based consumers
    /// that want to batch-retrieve observations instead of reacting via callback.
    pub fn take_observations(&mut self) -> Vec<Observation> {
        self.observation_buffer.drain(..).collect()
    }

    fn try_sync(&mut self) {
        loop {
            if self.state_buffer.is_empty() {
                break;
            }

            let state_ts = self.state_buffer[0].0;
            let mut matched: HashMap<String, (usize, Arc<VideoFrameData>)> = HashMap::new();
            let mut should_drop = false;
            let mut should_wait = false;

            for (track_name, frame_buf) in &self.video_buffers {
                let mut best_idx: Option<usize> = None;
                let mut best_delta = u64::MAX;

                for (i, frame) in frame_buf.iter().enumerate() {
                    let delta = state_ts.abs_diff(frame.timestamp_us);
                    if delta < self.config.search_range_us && delta < best_delta {
                        best_delta = delta;
                        best_idx = Some(i);
                    }
                }

                if let Some(idx) = best_idx {
                    matched.insert(track_name.clone(), (idx, frame_buf[idx].clone()));
                } else if !frame_buf.is_empty()
                    && frame_buf
                        .iter()
                        .all(|f| f.timestamp_us > state_ts + self.config.search_range_us)
                {
                    // All buffered frames are newer than the state's match window.
                    // Future frames will be even newer, so this state can never match — drop it.
                    // (The mirror case — all frames older — is a "wait": newer frames may still arrive.)
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
                log::warn!("dropping unsyncable state (no matching video frames within range)");
                let (_, values) = self.state_buffer.pop_front().unwrap();
                let dropped = vec![to_field_map(&self.state_fields, values)];
                if let Some(ref cb) = self.drop_cb {
                    cb(dropped);
                }
                continue;
            }

            if matched.len() == self.video_buffers.len() {
                let (ts, values) = self.state_buffer.pop_front().unwrap();

                for (track_name, (idx, _)) in &matched {
                    if let Some(buf) = self.video_buffers.get_mut(track_name) {
                        buf.drain(0..=*idx);
                    }
                }

                let observation = Observation {
                    state: to_field_map(&self.state_fields, values),
                    frames: matched
                        .into_iter()
                        .map(|(name, (_, frame))| (name, (*frame).clone()))
                        .collect(),
                    timestamp_us: ts,
                };

                if let Some(ref cb) = self.observation_cb {
                    cb(observation.clone());
                }

                self.observation_buffer.push_back(observation);
                while self.observation_buffer.len() > self.config.observation_buffer_size as usize {
                    self.observation_buffer.pop_front();
                }
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc as StdArc, Mutex};

    fn make_frame(track: &str, ts: u64) -> (String, Arc<VideoFrameData>) {
        (
            track.to_string(),
            Arc::new(VideoFrameData { width: 2, height: 2, data: vec![0u8; 6], timestamp_us: ts }),
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
        buf.push_state(1010, vec![1.0, 2.0]);

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
        assert_eq!(observations.lock().unwrap().len(), 0);

        let (n2, f2) = make_frame("cam2", 1002);
        buf.push_frame(&n2, f2);
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

        buf.push_state(100, vec![1.0]);
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

        buf.push_state(50_000, vec![1.0]);
        assert_eq!(observations.lock().unwrap().len(), 0);

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

        for ts in [100, 200, 300] {
            let (name, frame) = make_frame("cam1", ts);
            buf.push_frame(&name, frame);
        }

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
        assert!(buf.observation_buffer.is_empty());
    }

    #[test]
    fn take_observations_drains_buffer() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = SyncBuffer::new(&tracks, fields, SyncConfig::default());

        for ts in [1000u64, 2000, 3000] {
            let (name, frame) = make_frame("cam1", ts);
            buf.push_frame(&name, frame);
            buf.push_state(ts, vec![ts as f64]);
        }

        let obs = buf.take_observations();
        assert_eq!(obs.len(), 3);
        assert!(buf.take_observations().is_empty());
    }

    #[test]
    fn observation_buffer_evicts_oldest() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig { observation_buffer_size: 2, ..Default::default() };
        let mut buf = SyncBuffer::new(&tracks, fields, config);

        for ts in [1000u64, 2000, 3000] {
            let (name, frame) = make_frame("cam1", ts);
            buf.push_frame(&name, frame);
            buf.push_state(ts, vec![ts as f64]);
        }

        let obs = buf.take_observations();
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].timestamp_us, 2000);
        assert_eq!(obs[1].timestamp_us, 3000);
    }
}
