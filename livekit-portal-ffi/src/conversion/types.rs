use livekit_portal::{
    BufferMetrics, Observation, PortalMetrics, Role, RttMetrics, SyncMetrics, TransportMetrics,
    VideoFrameData,
};

use crate::proto;

// --- Role ---

impl From<Role> for proto::Role {
    fn from(r: Role) -> Self {
        match r {
            Role::Robot => proto::Role::Robot,
            Role::Operator => proto::Role::Operator,
        }
    }
}

impl From<proto::Role> for Role {
    fn from(r: proto::Role) -> Self {
        match r {
            proto::Role::Robot => Role::Robot,
            proto::Role::Operator => Role::Operator,
        }
    }
}

// --- VideoFrameData ---

impl From<VideoFrameData> for proto::VideoFrameData {
    fn from(f: VideoFrameData) -> Self {
        proto::VideoFrameData {
            width: f.width,
            height: f.height,
            data: f.data.to_vec(),
            timestamp_us: f.timestamp_us,
        }
    }
}

// --- Observation ---

impl From<Observation> for proto::Observation {
    fn from(o: Observation) -> Self {
        proto::Observation {
            timestamp_us: o.timestamp_us,
            state: o.state,
            frames: o.frames.into_iter().map(|(k, v)| (k, v.into())).collect(),
        }
    }
}

// --- Metrics ---

impl From<SyncMetrics> for proto::SyncMetrics {
    fn from(m: SyncMetrics) -> Self {
        proto::SyncMetrics {
            observations_emitted: m.observations_emitted,
            states_dropped: m.states_dropped,
            match_delta_us_p50: m.match_delta_us_p50,
            match_delta_us_p95: m.match_delta_us_p95,
            last_blocker_track: m.last_blocker_track,
        }
    }
}

impl From<TransportMetrics> for proto::TransportMetrics {
    fn from(m: TransportMetrics) -> Self {
        proto::TransportMetrics {
            frames_sent: m.frames_sent,
            frames_received: m.frames_received,
            states_sent: m.states_sent,
            states_received: m.states_received,
            actions_sent: m.actions_sent,
            actions_received: m.actions_received,
            frame_jitter_us: m.frame_jitter_us,
            state_jitter_us: m.state_jitter_us,
            action_jitter_us: m.action_jitter_us,
        }
    }
}

impl From<BufferMetrics> for proto::BufferMetrics {
    fn from(m: BufferMetrics) -> Self {
        proto::BufferMetrics {
            video_fill: m.video_fill.into_iter().map(|(k, v)| (k, v as u64)).collect(),
            state_fill: m.state_fill as u64,
            evictions: m.evictions,
        }
    }
}

impl From<RttMetrics> for proto::RttMetrics {
    fn from(m: RttMetrics) -> Self {
        proto::RttMetrics {
            rtt_us_last: m.rtt_us_last,
            rtt_us_mean: m.rtt_us_mean,
            rtt_us_p95: m.rtt_us_p95,
            pings_sent: m.pings_sent,
            pongs_received: m.pongs_received,
        }
    }
}

impl From<PortalMetrics> for proto::PortalMetrics {
    fn from(m: PortalMetrics) -> Self {
        proto::PortalMetrics {
            sync: Some(m.sync.into()),
            transport: Some(m.transport.into()),
            buffers: Some(m.buffers.into()),
            rtt: Some(m.rtt.into()),
        }
    }
}
