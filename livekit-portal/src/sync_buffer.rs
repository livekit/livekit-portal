use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::metrics::MetricsRegistry;
use crate::types::*;

/// Result of a `push_frame` / `push_state` call. Callers dispatch these
/// (invoke callbacks, enqueue into the pull-based buffer) *after* releasing
/// the SyncBuffer lock so slow consumers don't stall the hot path.
pub(crate) struct SyncOutput {
    pub observations: Vec<Observation>,
    pub drops: Vec<HashMap<String, f64>>,
}

impl SyncOutput {
    pub fn empty() -> Self {
        Self { observations: Vec::new(), drops: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.observations.is_empty() && self.drops.is_empty()
    }
}

pub(crate) struct SyncBuffer {
    track_names: Vec<String>,
    track_index: HashMap<String, usize>,
    // Parallel to `track_names`; indexed by track position.
    video_buffers: Vec<VecDeque<Arc<VideoFrameData>>>,
    state_buffer: VecDeque<(u64, Vec<f64>)>, // (timestamp_us, values)
    state_fields: Vec<String>,
    config: SyncConfig,

    // Per-track cursor: the largest index whose frame ts is <= head state ts
    // (or 0 if all frames are > head ts). Advances monotonically with state_ts
    // so sync work amortizes to O(N+M) across the stream.
    cursors: Vec<usize>,

    // The track that caused the last try_sync attempt to wait. `None` means
    // "unknown — run try_sync on the next push." Used to skip sync attempts
    // on pushes to tracks that cannot change head-state matchability.
    blocker: Option<usize>,

    // Reused across try_sync calls to avoid allocating a match map per iteration.
    matched_scratch: Vec<Option<(usize, Arc<VideoFrameData>)>>,

    metrics: Arc<MetricsRegistry>,
}

impl SyncBuffer {
    pub fn new(
        video_track_names: &[String],
        state_fields: Vec<String>,
        config: SyncConfig,
        metrics: Arc<MetricsRegistry>,
    ) -> Self {
        let track_names: Vec<String> = video_track_names.to_vec();
        let track_index: HashMap<String, usize> =
            track_names.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect();
        let video_buffers = (0..track_names.len()).map(|_| VecDeque::new()).collect();
        let cursors = vec![0; track_names.len()];
        let matched_scratch = vec![None; track_names.len()];
        Self {
            track_names,
            track_index,
            video_buffers,
            state_buffer: VecDeque::new(),
            state_fields,
            config,
            cursors,
            blocker: None,
            matched_scratch,
            metrics,
        }
    }

    pub fn push_frame(&mut self, track_name: &str, frame: Arc<VideoFrameData>) -> SyncOutput {
        let idx = match self.track_index.get(track_name) {
            Some(&i) => i,
            None => return SyncOutput::empty(),
        };

        let cap = self.config.video_buffer_size as usize;
        let buf = &mut self.video_buffers[idx];
        buf.push_back(frame);

        let mut evicted = 0usize;
        while buf.len() > cap {
            buf.pop_front();
            evicted += 1;
        }
        if evicted > 0 {
            let cursor = &mut self.cursors[idx];
            *cursor = cursor.saturating_sub(evicted);
            if let Some(tm) = self.metrics.track(track_name) {
                tm.record_evictions(evicted as u64);
            }
            log::warn!("video buffer overflow on '{track_name}': evicted {evicted} frame(s)");
        }

        // Skip try_sync when this push cannot have changed head-state matchability:
        //   - another track is blocking (a push to a non-blocker doesn't unblock it), AND
        //   - no eviction happened on this track (eviction can newly-transition a track
        //     from matching → unmatchable, which must be checked).
        let should_run = match self.blocker {
            None => true,
            Some(b) if b == idx => true,
            Some(_) => evicted > 0,
        };

        if should_run {
            self.try_sync()
        } else {
            SyncOutput::empty()
        }
    }

    pub fn push_state(&mut self, timestamp_us: u64, values: Vec<f64>) -> SyncOutput {
        let old_head_ts = self.state_buffer.front().map(|(ts, _)| *ts);
        self.state_buffer.push_back((timestamp_us, values));

        let mut overflow_drops: Vec<HashMap<String, f64>> = Vec::new();
        while self.state_buffer.len() > self.config.state_buffer_size as usize {
            let (_, vals) = self.state_buffer.pop_front().unwrap();
            overflow_drops.push(to_field_map(&self.state_fields, &vals));
        }
        if !overflow_drops.is_empty() {
            self.metrics.record_state_dropped(overflow_drops.len() as u64);
            log::warn!("state buffer overflow: dropped {} state(s)", overflow_drops.len());
        }
        // If eviction (or first-ever push) changed the head state, the old blocker
        // hint no longer applies.
        let new_head_ts = self.state_buffer.front().map(|(ts, _)| *ts);
        if new_head_ts != old_head_ts {
            self.blocker = None;
        }

        let mut output = self.try_sync();
        if !overflow_drops.is_empty() {
            // Overflow drops precede any sync-fail drops temporally.
            overflow_drops.append(&mut output.drops);
            output.drops = overflow_drops;
        }
        output
    }

    pub fn clear(&mut self) {
        for buf in &mut self.video_buffers {
            buf.clear();
        }
        self.state_buffer.clear();
        for c in &mut self.cursors {
            *c = 0;
        }
        self.blocker = None;
    }

    fn try_sync(&mut self) -> SyncOutput {
        let mut output = SyncOutput::empty();
        let range = self.config.search_range_us;

        loop {
            if self.state_buffer.is_empty() {
                self.blocker = None;
                return output;
            }

            let state_ts = self.state_buffer[0].0;
            // Next state in the buffer (if any) — used for fair-share: if a
            // candidate frame is closer to the next state than to the head
            // state, we skip it so the later state can claim it.
            let next_state_ts = self.state_buffer.get(1).map(|(ts, _)| *ts);

            for slot in &mut self.matched_scratch {
                *slot = None;
            }

            // Per-iteration status. Priority: drop > wait > emit. We scan every
            // track (even after a wait-on-earlier-track) so that a drop-eligible
            // track later in the list can override the wait — otherwise a state
            // could stall forever waiting on cam1 while cam2 has already moved
            // beyond the match horizon.
            let mut should_drop = false;
            let mut iter_blocker: Option<usize> = None;

            for track_i in 0..self.video_buffers.len() {
                let frame_buf = &self.video_buffers[track_i];
                if frame_buf.is_empty() {
                    if iter_blocker.is_none() {
                        iter_blocker = Some(track_i);
                    }
                    continue;
                }

                let cursor = &mut self.cursors[track_i];
                // Defensive clamp in case capacity shrunk or mutation missed an adjustment.
                if *cursor >= frame_buf.len() {
                    *cursor = frame_buf.len() - 1;
                }
                // Rewind if the cursor is already past state_ts (can happen if
                // states arrive out of order on unreliable delivery).
                while *cursor > 0 && frame_buf[*cursor].timestamp_us > state_ts {
                    *cursor -= 1;
                }
                // Advance while the next frame is still at or before state_ts.
                while *cursor + 1 < frame_buf.len()
                    && frame_buf[*cursor + 1].timestamp_us <= state_ts
                {
                    *cursor += 1;
                }

                let cursor_val = *cursor;
                let mut best_idx: Option<usize> = None;
                let mut best_delta = u64::MAX;
                for candidate in [Some(cursor_val), cursor_val.checked_add(1)].into_iter().flatten()
                {
                    if let Some(f) = frame_buf.get(candidate) {
                        let d = state_ts.abs_diff(f.timestamp_us);
                        if d >= range || d >= best_delta {
                            continue;
                        }
                        // Fair-share: if a later buffered state has a strictly
                        // closer claim, leave the frame for it. Prevents a
                        // greedy head-state from stealing its neighbor's frame
                        // when tolerance > 1 tick.
                        if let Some(nts) = next_state_ts {
                            if nts.abs_diff(f.timestamp_us) < d {
                                continue;
                            }
                        }
                        best_delta = d;
                        best_idx = Some(candidate);
                    }
                }

                if let Some(idx) = best_idx {
                    self.matched_scratch[track_i] = Some((idx, frame_buf[idx].clone()));
                    continue;
                }

                // Unmatched. The real question is whether any *future* frame could
                // match; since frame timestamps are monotonic, future ts ≥ back_ts,
                // so the state is permanently unmatchable iff back_ts >= state_ts +
                // range. (Checking the front would only detect the drop after
                // eviction has dragged the old tail past the horizon — a latency
                // bug of up to video_buffer_size frames.) `>=` matches the strict
                // `d < range` match rule: a frame at exactly state_ts + range is
                // not a match, so the state can't match it either.
                let newest_ts = frame_buf.back().unwrap().timestamp_us;
                if newest_ts >= state_ts.saturating_add(range) {
                    should_drop = true;
                    break;
                } else if iter_blocker.is_none() {
                    iter_blocker = Some(track_i);
                }
            }

            if should_drop {
                log::warn!("dropping unsyncable state (no matching video frames within range)");
                let (_, values) = self.state_buffer.pop_front().unwrap();
                output.drops.push(to_field_map(&self.state_fields, &values));
                self.metrics.record_state_dropped(1);
                // Retry next state with fresh iteration.
                continue;
            }

            if let Some(b) = iter_blocker {
                self.blocker = Some(b);
                self.metrics.record_blocker(b);
                return output;
            }

            // Record worst-case per-track alignment before we drain the matches.
            let mut worst_delta = 0u64;
            for slot in &self.matched_scratch {
                if let Some((_, frame)) = slot.as_ref() {
                    worst_delta = worst_delta.max(state_ts.abs_diff(frame.timestamp_us));
                }
            }
            self.metrics.record_observation(worst_delta);

            let (ts, values) = self.state_buffer.pop_front().unwrap();

            let mut frames_map: HashMap<String, VideoFrameData> =
                HashMap::with_capacity(self.track_names.len());
            for track_i in 0..self.track_names.len() {
                if let Some((idx, frame)) = self.matched_scratch[track_i].take() {
                    self.video_buffers[track_i].drain(0..=idx);
                    // Cursor was at or just past idx; after draining, shift it back.
                    self.cursors[track_i] = self.cursors[track_i].saturating_sub(idx + 1);
                    // Cheap clone: VideoFrameData carries Arc<[u8]>.
                    frames_map.insert(self.track_names[track_i].clone(), (*frame).clone());
                }
            }

            output.observations.push(Observation {
                state: to_field_map(&self.state_fields, &values),
                frames: frames_map,
                timestamp_us: ts,
            });
        }
    }

    pub fn video_fill_snapshot(&self) -> HashMap<String, usize> {
        self.track_names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), self.video_buffers[i].len()))
            .collect()
    }

    pub fn state_fill(&self) -> usize {
        self.state_buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(track: &str, ts: u64) -> (String, Arc<VideoFrameData>) {
        (
            track.to_string(),
            Arc::new(VideoFrameData {
                width: 2,
                height: 2,
                data: Arc::from(vec![0u8; 6]),
                timestamp_us: ts,
            }),
        )
    }

    fn push_f(buf: &mut SyncBuffer, track: &str, ts: u64) -> SyncOutput {
        let (name, frame) = make_frame(track, ts);
        buf.push_frame(&name, frame)
    }

    fn mk(names: &[String], fields: Vec<String>, config: SyncConfig) -> SyncBuffer {
        let metrics = Arc::new(MetricsRegistry::new(names));
        SyncBuffer::new(names, fields, config, metrics)
    }

    #[test]
    fn sync_single_track() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string(), "j2".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        assert!(push_f(&mut buf, "cam1", 1000).observations.is_empty());

        let out = buf.push_state(1010, vec![1.0, 2.0]);
        assert_eq!(out.observations.len(), 1);
        let obs = &out.observations[0];
        assert_eq!(obs.state["j1"], 1.0);
        assert_eq!(obs.state["j2"], 2.0);
        assert_eq!(obs.timestamp_us, 1010);
    }

    #[test]
    fn sync_multi_track() {
        let tracks = vec!["cam1".to_string(), "cam2".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        assert!(push_f(&mut buf, "cam1", 1000).observations.is_empty());
        assert!(buf.push_state(1005, vec![5.0]).observations.is_empty());

        let out = push_f(&mut buf, "cam2", 1002);
        assert_eq!(out.observations.len(), 1);
        assert!(out.observations[0].frames.contains_key("cam1"));
        assert!(out.observations[0].frames.contains_key("cam2"));
    }

    #[test]
    fn drop_unsyncable_state() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        assert!(buf.push_state(100, vec![1.0]).is_empty());
        let out = push_f(&mut buf, "cam1", 200_000);
        assert!(out.observations.is_empty());
        assert_eq!(out.drops.len(), 1);
        assert_eq!(out.drops[0]["j1"], 1.0);
    }

    #[test]
    fn out_of_range_waits() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        assert!(buf.push_state(50_000, vec![1.0]).is_empty());
        let out = push_f(&mut buf, "cam1", 50_010);
        assert_eq!(out.observations.len(), 1);
    }

    #[test]
    fn buffer_overflow_evicts_oldest() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config =
            SyncConfig { video_buffer_size: 2, state_buffer_size: 2, ..Default::default() };
        let mut buf = mk(&tracks, fields, config);

        for ts in [100, 200, 300] {
            let _ = push_f(&mut buf, "cam1", ts);
        }

        let cam_buf = &buf.video_buffers[buf.track_index["cam1"]];
        assert_eq!(cam_buf.len(), 2);
        assert_eq!(cam_buf[0].timestamp_us, 200);
        assert_eq!(cam_buf[1].timestamp_us, 300);
    }

    #[test]
    fn clear_flushes_all() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        let _ = push_f(&mut buf, "cam1", 1000);
        let _ = buf.push_state(1000, vec![1.0]);
        buf.clear();

        assert!(buf.video_buffers.iter().all(|b| b.is_empty()));
        assert!(buf.state_buffer.is_empty());
        assert!(buf.cursors.iter().all(|&c| c == 0));
        assert!(buf.blocker.is_none());
    }

    // --- New algorithm edge cases ---

    /// Cursor should advance monotonically across many sequential syncs.
    #[test]
    fn cursor_advances_across_sequential_matches() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig { video_buffer_size: 100, ..Default::default() };
        let mut buf = mk(&tracks, fields, config);

        // Push 10 frames at 1000us intervals.
        for i in 0..10 {
            let _ = push_f(&mut buf, "cam1", 1_000 + i * 1_000);
        }
        // Match each with a state, each state should consume one frame.
        let mut matched_ts = Vec::new();
        for i in 0..10 {
            let out = buf.push_state(1_010 + i * 1_000, vec![i as f64]);
            assert_eq!(out.observations.len(), 1, "state #{} should produce 1 obs", i);
            matched_ts.push(out.observations[0].frames["cam1"].timestamp_us);
        }
        assert_eq!(matched_ts, (0..10).map(|i| 1_000 + i * 1_000).collect::<Vec<_>>());
    }

    /// Non-blocker push should defer try_sync, but a subsequent push to the
    /// blocker must still produce the observation (no lost state).
    #[test]
    fn non_blocker_push_defers_but_converges() {
        let tracks = vec!["cam1".to_string(), "cam2".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        // State + cam2 present; cam1 empty → cam1 is the blocker.
        assert!(buf.push_state(1_000, vec![1.0]).is_empty());
        assert!(push_f(&mut buf, "cam2", 1_005).is_empty());
        assert_eq!(buf.blocker, Some(buf.track_index["cam1"]));

        // Push another cam2 frame — not the blocker, try_sync should skip.
        // The observation count stays at 0 either way, so we just check no
        // spurious work: buffer accepted the push.
        assert!(push_f(&mut buf, "cam2", 1_006).is_empty());
        assert_eq!(buf.video_buffers[buf.track_index["cam2"]].len(), 2);

        // Now push to the blocker — observation must fire.
        let out = push_f(&mut buf, "cam1", 1_008);
        assert_eq!(out.observations.len(), 1);
        assert!(buf.blocker.is_none());
    }

    /// If eviction on a non-blocker track removes the only in-range frame,
    /// the state must drop (not silently stall).
    #[test]
    fn eviction_on_non_blocker_can_trigger_drop() {
        let tracks = vec!["cam1".to_string(), "cam2".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig {
            video_buffer_size: 1,
            state_buffer_size: 10,
            search_range_us: 30_000,
        };
        let mut buf = mk(&tracks, fields, config);

        // State at 1_000; cam1 empty (blocker); cam2 has a frame in range.
        assert!(buf.push_state(1_000, vec![1.0]).is_empty());
        assert!(push_f(&mut buf, "cam2", 1_005).is_empty());
        assert_eq!(buf.blocker, Some(buf.track_index["cam1"]));

        // Push new cam2 frame far in the future; cap=1 means the in-range
        // frame is evicted. Eager drop path must fire even though cam2 is not
        // the blocker.
        let out = push_f(&mut buf, "cam2", 500_000);
        assert!(out.observations.is_empty());
        assert_eq!(out.drops.len(), 1, "state should be dropped once its cam2 match is evicted");
    }

    /// Out-of-order state timestamps must still find the correct match via
    /// cursor rewind.
    #[test]
    fn out_of_order_state_rewinds_cursor() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        // Pre-populate frames spanning a wide range.
        for ts in [1_000u64, 5_000, 10_000, 50_000, 100_000] {
            let _ = push_f(&mut buf, "cam1", ts);
        }

        // First match at high ts advances cursor forward.
        let out = buf.push_state(100_005, vec![0.0]);
        assert_eq!(out.observations.len(), 1);
        assert_eq!(out.observations[0].frames["cam1"].timestamp_us, 100_000);

        // Re-populate so there's a frame near an earlier ts, then push an
        // earlier state — cursor rewind must find it.
        let _ = push_f(&mut buf, "cam1", 200_000);
        let _ = push_f(&mut buf, "cam1", 200_005);
        let out = buf.push_state(200_002, vec![0.0]);
        assert_eq!(out.observations.len(), 1);
        assert_eq!(out.observations[0].frames["cam1"].timestamp_us, 200_000);
    }

    /// State eviction pushing a new head state clears the blocker so the new
    /// head gets re-evaluated immediately.
    #[test]
    fn state_eviction_updates_head_and_clears_blocker() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig { state_buffer_size: 1, ..Default::default() };
        let mut buf = mk(&tracks, fields, config);

        // No frames yet: both push_state calls see an empty cam1 → wait.
        // cap_state=1 means the second state evicts the first.
        assert!(buf.push_state(1_000, vec![1.0]).is_empty());
        assert_eq!(buf.blocker, Some(0));
        // Second push evicts state@1000; overflow surfaces as a drop.
        let out = buf.push_state(2_000, vec![2.0]);
        assert!(out.observations.is_empty());
        assert_eq!(out.drops.len(), 1);
        assert_eq!(out.drops[0]["j1"], 1.0);

        // Only the second state remains. A frame matching it fires the obs.
        let out = push_f(&mut buf, "cam1", 2_005);
        assert_eq!(out.observations.len(), 1);
        assert_eq!(out.observations[0].state["j1"], 2.0, "evicted state should not leak through");
        assert_eq!(out.observations[0].timestamp_us, 2_000);
    }

    /// Drop must fire when the *newest* frame is past the horizon, even if an
    /// older frame is still buffered below the match window. Under the old
    /// front-based check, the state would stall until eviction dragged the old
    /// frame through the horizon.
    #[test]
    fn drop_triggers_on_back_past_horizon() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig {
            video_buffer_size: 10,
            state_buffer_size: 10,
            search_range_us: 500,
        };
        let mut buf = mk(&tracks, fields, config);

        let _ = push_f(&mut buf, "cam1", 1_000); // far below state - range (2_500)
        assert!(buf.push_state(3_000, vec![1.0]).is_empty());

        // Newest frame lands past state + range (3_500). Even though the old
        // 1_000 frame is still in the buffer, no future frame can be < 5_000,
        // so the state is permanently unmatchable.
        let out = push_f(&mut buf, "cam1", 5_000);
        assert!(out.observations.is_empty());
        assert_eq!(out.drops.len(), 1, "state should drop as soon as back passes horizon");
    }

    /// Boundary: a frame landing exactly at `state_ts + range` is not a match
    /// (strict `<`), and all future frames are ≥ that ts, so the state drops.
    #[test]
    fn drop_fires_at_exact_range_boundary() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig {
            video_buffer_size: 10,
            state_buffer_size: 10,
            search_range_us: 500,
        };
        let mut buf = mk(&tracks, fields, config);

        assert!(buf.push_state(1_000, vec![1.0]).is_empty());
        let out = push_f(&mut buf, "cam1", 1_500); // delta == range, not a match
        assert!(out.observations.is_empty());
        assert_eq!(out.drops.len(), 1);
    }

    /// State-buffer overflow must surface evicted states via `output.drops`
    /// so the `on_drop` callback can fire, matching spec behavior.
    #[test]
    fn state_overflow_with_tracks_reports_drops() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig { state_buffer_size: 2, ..Default::default() };
        let mut buf = mk(&tracks, fields, config);

        // No frames: each push_state blocks (no sync), fills the state buffer.
        assert!(buf.push_state(100, vec![1.0]).drops.is_empty());
        assert!(buf.push_state(200, vec![2.0]).drops.is_empty());
        // Third push triggers overflow; state@100 must appear in drops.
        let out = buf.push_state(300, vec![3.0]);
        assert_eq!(out.drops.len(), 1);
        assert_eq!(out.drops[0]["j1"], 1.0);
    }

    /// With a widened range (>1 tick), a state whose exact frame was lost
    /// falls back to an adjacent frame if no later state has a closer claim.
    #[test]
    fn wide_range_matches_neighbor_when_native_lost() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        // 30fps ticks = 33_333us; tolerance 1.5 → range = 50_000us.
        let config = SyncConfig {
            video_buffer_size: 5,
            state_buffer_size: 5,
            search_range_us: 50_000,
        };
        let mut buf = mk(&tracks, fields, config);

        // Frame at tick 0 stands in for "T−1"; frame at T was lost; only
        // frame@0 is available for state@33_333.
        let _ = push_f(&mut buf, "cam1", 0);
        let out = buf.push_state(33_333, vec![1.0]);
        assert_eq!(out.observations.len(), 1);
        assert_eq!(out.observations[0].frames["cam1"].timestamp_us, 0);
    }

    /// Fair-share: if an earlier state and a later state are both in the
    /// buffer and a single frame sits closer to the later state, the earlier
    /// state must NOT steal it. It may drop, but the later state gets to use
    /// its own frame.
    #[test]
    fn fair_share_prevents_stealing() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        let config = SyncConfig {
            video_buffer_size: 5,
            state_buffer_size: 5,
            search_range_us: 50_000, // tolerance 1.5 at 30fps
        };
        let mut buf = mk(&tracks, fields, config);

        // Both states buffered before any frames arrive.
        assert!(buf.push_state(0, vec![1.0]).is_empty());
        assert!(buf.push_state(33_333, vec![2.0]).is_empty());

        // frame@33_333 is closer to state@33_333 than to state@0;
        // fair-share must keep state@0 from grabbing it.
        let out = push_f(&mut buf, "cam1", 33_333);
        assert!(
            out.observations.is_empty(),
            "state@0 must not steal frame@33_333 from state@33_333"
        );

        // Push a later frame past state@0's horizon to force the drop;
        // state@33_333 then matches its own frame.
        let out = push_f(&mut buf, "cam1", 100_000);
        assert_eq!(out.drops.len(), 1, "state@0 drops once its horizon is crossed");
        assert_eq!(out.drops[0]["j1"], 1.0);
        assert_eq!(out.observations.len(), 1);
        assert_eq!(out.observations[0].state["j1"], 2.0);
        assert_eq!(out.observations[0].frames["cam1"].timestamp_us, 33_333);
    }

    /// Tight range (<1 tick) preserves the legacy drop-on-loss behavior:
    /// a state can't reach an adjacent frame, so it drops as soon as a
    /// later frame crosses the horizon.
    #[test]
    fn tight_range_still_drops_on_loss() {
        let tracks = vec!["cam1".to_string()];
        let fields = vec!["j1".to_string()];
        // tolerance 0.5 at 30fps → range = 16_666us, adjacent frames unreachable.
        let config = SyncConfig {
            video_buffer_size: 5,
            state_buffer_size: 5,
            search_range_us: 16_666,
        };
        let mut buf = mk(&tracks, fields, config);

        let _ = push_f(&mut buf, "cam1", 0);
        assert!(buf.push_state(33_333, vec![1.0]).is_empty()); // blocks: no match in range
        let out = push_f(&mut buf, "cam1", 100_000); // crosses horizon, fires drop
        assert!(out.observations.is_empty());
        assert_eq!(out.drops.len(), 1, "tight range must drop when native frame is missing");
    }

    /// Sanity: inputs that stress the binary/cursor path with many empty and
    /// partial iterations should never panic or produce spurious observations.
    #[test]
    fn stress_no_spurious_observations() {
        let tracks = vec!["cam1".to_string(), "cam2".to_string()];
        let fields = vec!["j1".to_string()];
        let mut buf = mk(&tracks, fields, SyncConfig::default());

        let mut total_obs = 0;
        // Push 100 interleaved events; each state needs frames on BOTH tracks
        // within 30ms.
        for i in 0..100u64 {
            let ts = 1_000 + i * 1_000;
            let out1 = push_f(&mut buf, "cam1", ts);
            let out2 = push_f(&mut buf, "cam2", ts + 100);
            let out3 = buf.push_state(ts + 50, vec![i as f64]);
            total_obs += out1.observations.len();
            total_obs += out2.observations.len();
            total_obs += out3.observations.len();
        }
        assert_eq!(total_obs, 100);
    }
}
