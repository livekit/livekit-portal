use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use livekit::prelude::*;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::config::PortalConfig;
use crate::data::{handle_data_received, DataCb, DataPublisher};
use crate::error::{PortalError, PortalResult};
use crate::metrics::{DataStream, MetricsRegistry, PortalMetrics};
use crate::rtt::RttService;
use crate::sync_buffer::{SyncBuffer, SyncOutput};
use crate::types::*;
use crate::video::{VideoPublisher, VideoReceiver};

type ObservationCb = Box<dyn Fn(&Observation) + Send + Sync>;
type VideoCb = Box<dyn Fn(&str, &VideoFrameData) + Send + Sync>;
type DropCb = Box<dyn Fn(Vec<HashMap<String, f64>>) + Send + Sync>;

/// Drains the buffers returned by `SyncBuffer::push_*` and dispatches them to
/// the user — callback first (by reference, no clone), then into the pull-based
/// observation buffer. Kept separate from `SyncBuffer` so callbacks run with no
/// sync-buffer lock held.
pub(crate) struct ObservationSink {
    observation_cb: Mutex<Option<ObservationCb>>,
    drop_cb: Mutex<Option<DropCb>>,
    buffer: Mutex<VecDeque<Observation>>,
    buffer_size: usize,
    metrics: Arc<MetricsRegistry>,
}

impl ObservationSink {
    pub(crate) fn new(buffer_size: usize, metrics: Arc<MetricsRegistry>) -> Self {
        Self {
            observation_cb: Mutex::new(None),
            drop_cb: Mutex::new(None),
            buffer: Mutex::new(VecDeque::new()),
            buffer_size,
            metrics,
        }
    }

    pub(crate) fn dispatch(&self, output: SyncOutput) {
        let SyncOutput { observations, drops } = output;

        if !observations.is_empty() {
            // Fire callback by reference — no clone, even with a registered consumer.
            {
                let cb_slot = self.observation_cb.lock();
                if let Some(cb) = cb_slot.as_ref() {
                    for obs in &observations {
                        cb(obs);
                    }
                }
            }
            if self.buffer_size > 0 {
                let mut evicted = 0u64;
                {
                    let mut buf = self.buffer.lock();
                    for obs in observations {
                        buf.push_back(obs);
                        while buf.len() > self.buffer_size {
                            buf.pop_front();
                            evicted += 1;
                        }
                    }
                }
                if evicted > 0 {
                    self.metrics.record_observation_evicted(evicted);
                }
            }
        }

        if !drops.is_empty() {
            if let Some(cb) = self.drop_cb.lock().as_ref() {
                cb(drops);
            }
        }
    }

    pub(crate) fn take(&self) -> Vec<Observation> {
        self.buffer.lock().drain(..).collect()
    }

    pub(crate) fn clear(&self) {
        self.buffer.lock().clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.buffer.lock().len()
    }

    pub(crate) fn set_observation_cb(&self, cb: ObservationCb) {
        *self.observation_cb.lock() = Some(cb);
    }

    pub(crate) fn set_drop_cb(&self, cb: DropCb) {
        *self.drop_cb.lock() = Some(cb);
    }
}

struct ConnectionState {
    room: Option<Room>,
    event_task: Option<JoinHandle<()>>,
    rtt: Option<Arc<RttService>>,
}

pub struct Portal {
    config: PortalConfig,

    // Lifecycle state (connect/disconnect).
    state: Mutex<ConnectionState>,

    // Video receivers are spawned by the event loop (on TrackSubscribed) and
    // torn down by `disconnect`, so they live in an Arc shared with both.
    video_receivers: Arc<Mutex<HashMap<String, VideoReceiver>>>,

    // Hot-path publishers. Each is guarded by its own mutex so send methods
    // can clone the Arc out and drop the lock before doing any IO.
    video_publishers: Mutex<HashMap<String, Arc<VideoPublisher>>>,
    state_publisher: Mutex<Option<Arc<DataPublisher>>>,
    action_publisher: Mutex<Option<Arc<DataPublisher>>>,

    // Operator-side sync + dispatch.
    sync_buffer: Mutex<Option<Arc<Mutex<SyncBuffer>>>>,
    obs_sink: Arc<ObservationSink>,

    // Data callbacks.
    action_cb: Arc<Mutex<Option<DataCb>>>,
    state_cb: Arc<Mutex<Option<DataCb>>>,
    // Fixed at construction (keyed by declared video_tracks) — no lock on the map itself.
    video_cbs: HashMap<String, Arc<Mutex<Option<VideoCb>>>>,

    metrics: Arc<MetricsRegistry>,
}

impl Portal {
    pub fn new(config: PortalConfig) -> Self {
        let video_cbs: HashMap<_, _> = config
            .video_tracks
            .iter()
            .map(|name| (name.clone(), Arc::new(Mutex::new(None))))
            .collect();

        let metrics = Arc::new(MetricsRegistry::new(&config.video_tracks));
        let obs_sink = Arc::new(ObservationSink::new(
            config.sync_config.observation_buffer_size as usize,
            metrics.clone(),
        ));

        Self {
            config,
            state: Mutex::new(ConnectionState { room: None, event_task: None, rtt: None }),
            video_receivers: Arc::new(Mutex::new(HashMap::new())),
            video_publishers: Mutex::new(HashMap::new()),
            state_publisher: Mutex::new(None),
            action_publisher: Mutex::new(None),
            sync_buffer: Mutex::new(None),
            obs_sink,
            action_cb: Arc::new(Mutex::new(None)),
            state_cb: Arc::new(Mutex::new(None)),
            video_cbs,
            metrics,
        }
    }

    pub async fn connect(&self, url: &str, token: &str) -> PortalResult<()> {
        if self.state.lock().room.is_some() {
            return Err(PortalError::AlreadyConnected);
        }

        let mut options = RoomOptions::default();
        options.auto_subscribe = true;

        log::info!("[{}] connecting as {:?} to {}", self.config.session, self.config.role, url);

        let (room, events) = Room::connect(url, token, options)
            .await
            .map_err(|e| PortalError::Room(e.to_string()))?;

        match self.config.role {
            Role::Robot => self.setup_robot(&room).await?,
            Role::Operator => self.setup_operator(&room),
        }

        let rtt = Arc::new(RttService::spawn(
            room.local_participant(),
            self.config.ping_interval_ms,
            self.metrics.clone(),
        ));

        log::info!("[{}] connected as {:?}", self.config.session, self.config.role);

        // Event dispatch runs off a snapshot of the fields it touches, not the
        // whole Portal, so it doesn't need any outer lock.
        let ctx = EventContext {
            config: self.config.clone(),
            sync_buffer: self.sync_buffer.lock().clone(),
            obs_sink: self.obs_sink.clone(),
            action_cb: self.action_cb.clone(),
            state_cb: self.state_cb.clone(),
            video_cbs: self.video_cbs.clone(),
            video_receivers: self.video_receivers.clone(),
            metrics: self.metrics.clone(),
            rtt: rtt.clone(),
        };
        let event_handle = tokio::spawn(async move {
            let mut events = events;
            while let Some(event) = events.recv().await {
                handle_room_event(&ctx, event);
            }
        });

        let mut state = self.state.lock();
        state.room = Some(room);
        state.event_task = Some(event_handle);
        state.rtt = Some(rtt);
        Ok(())
    }

    pub fn send_video_frame(
        &self,
        track_name: &str,
        i420_data: &[u8],
        width: u32,
        height: u32,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let publisher = {
            let map = self.video_publishers.lock();
            map.get(track_name).cloned().ok_or_else(|| PortalError::UnknownVideoTrack {
                name: track_name.to_string(),
            })?
        };
        publisher.send_frame(i420_data, width, height, timestamp_us)
    }

    pub fn send_state(
        &self,
        values: &HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let publisher = self
            .state_publisher
            .lock()
            .clone()
            .ok_or(PortalError::WrongRole(Role::Operator))?;
        publisher.send_map(values, timestamp_us)
    }

    pub fn send_action(
        &self,
        values: &HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let publisher =
            self.action_publisher.lock().clone().ok_or(PortalError::WrongRole(Role::Robot))?;
        publisher.send_map(values, timestamp_us)
    }

    pub async fn disconnect(&self) -> PortalResult<()> {
        let room = self.state.lock().room.take();
        log::info!("disconnecting");
        if let Some(room) = room {
            room.close().await.map_err(|e| PortalError::Room(e.to_string()))?;
        }

        {
            let mut state = self.state.lock();
            if let Some(task) = state.event_task.take() {
                task.abort();
            }
            state.rtt = None;
        }
        {
            let mut receivers = self.video_receivers.lock();
            for receiver in receivers.values() {
                receiver.abort();
            }
            receivers.clear();
        }

        self.video_publishers.lock().clear();
        *self.state_publisher.lock() = None;
        *self.action_publisher.lock() = None;

        if let Some(sb) = self.sync_buffer.lock().take() {
            sb.lock().clear();
        }
        self.obs_sink.clear();

        Ok(())
    }

    pub fn take_observations(&self) -> Vec<Observation> {
        self.obs_sink.take()
    }

    // --- Callback registration ---

    pub fn on_action(&self, callback: impl Fn(HashMap<String, f64>) + Send + Sync + 'static) {
        *self.action_cb.lock() = Some(Box::new(callback));
    }

    pub fn on_observation(&self, callback: impl Fn(&Observation) + Send + Sync + 'static) {
        self.obs_sink.set_observation_cb(Box::new(callback));
    }

    pub fn on_state(&self, callback: impl Fn(HashMap<String, f64>) + Send + Sync + 'static) {
        *self.state_cb.lock() = Some(Box::new(callback));
    }

    pub fn on_video(
        &self,
        track_name: &str,
        callback: impl Fn(&str, &VideoFrameData) + Send + Sync + 'static,
    ) {
        match self.video_cbs.get(track_name) {
            Some(cb_slot) => *cb_slot.lock() = Some(Box::new(callback)),
            None => {
                log::warn!("on_video: track '{track_name}' is not registered — callback ignored")
            }
        }
    }

    pub fn on_drop(&self, callback: impl Fn(Vec<HashMap<String, f64>>) + Send + Sync + 'static) {
        self.obs_sink.set_drop_cb(Box::new(callback));
    }

    // --- Internal ---

    async fn setup_robot(&self, room: &Room) -> PortalResult<()> {
        let lp = room.local_participant();

        for track_name in &self.config.video_tracks {
            let track_metrics = self
                .metrics
                .track(track_name)
                .expect("track metrics registered at construction");
            let publisher = VideoPublisher::new(track_name, track_metrics);
            publisher.publish(&lp).await?;
            log::info!("[{}] published video track '{track_name}'", self.config.session);
            self.video_publishers.lock().insert(track_name.clone(), Arc::new(publisher));
        }

        if !self.config.state_fields.is_empty() {
            let publisher = DataPublisher::new(
                self.config.state_fields.clone(),
                "portal_state",
                self.config.state_reliable,
                lp.clone(),
                self.metrics.clone(),
                DataStream::State,
            );
            let mode = if self.config.state_reliable { "reliable" } else { "unreliable" };
            log::info!(
                "[{}] ready to publish state via {mode} data ({} fields)",
                self.config.session,
                self.config.state_fields.len()
            );
            *self.state_publisher.lock() = Some(Arc::new(publisher));
        }

        Ok(())
    }

    fn setup_operator(&self, room: &Room) {
        let lp = room.local_participant();

        let sync_buffer = Arc::new(Mutex::new(SyncBuffer::new(
            &self.config.video_tracks,
            self.config.state_fields.clone(),
            self.config.sync_config,
            self.metrics.clone(),
        )));
        *self.sync_buffer.lock() = Some(sync_buffer);

        if !self.config.action_fields.is_empty() {
            let mode = if self.config.action_reliable { "reliable" } else { "unreliable" };
            log::info!(
                "[{}] ready to publish action via {mode} data ({} fields)",
                self.config.session,
                self.config.action_fields.len()
            );
            let publisher = DataPublisher::new(
                self.config.action_fields.clone(),
                "portal_action",
                self.config.action_reliable,
                lp,
                self.metrics.clone(),
                DataStream::Action,
            );
            *self.action_publisher.lock() = Some(Arc::new(publisher));
        }
    }

    /// Snapshot of metrics since construction or the last `reset_metrics()`.
    pub fn metrics(&self) -> PortalMetrics {
        let (video_fill, state_fill) = match self.sync_buffer.lock().as_ref() {
            Some(sb) => {
                let sb = sb.lock();
                (sb.video_fill_snapshot(), sb.state_fill())
            }
            None => (HashMap::new(), 0),
        };
        let observation_fill = self.obs_sink.len();
        self.metrics.snapshot(video_fill, state_fill, observation_fill)
    }

    pub fn reset_metrics(&self) {
        self.metrics.reset();
    }
}

/// Snapshot of the fields the room event loop needs, so it doesn't take any
/// Portal-level lock on the hot path.
struct EventContext {
    config: PortalConfig,
    sync_buffer: Option<Arc<Mutex<SyncBuffer>>>,
    obs_sink: Arc<ObservationSink>,
    action_cb: Arc<Mutex<Option<DataCb>>>,
    state_cb: Arc<Mutex<Option<DataCb>>>,
    video_cbs: HashMap<String, Arc<Mutex<Option<VideoCb>>>>,
    video_receivers: Arc<Mutex<HashMap<String, VideoReceiver>>>,
    metrics: Arc<MetricsRegistry>,
    rtt: Arc<RttService>,
}

fn handle_room_event(ctx: &EventContext, event: RoomEvent) {
    match event {
        RoomEvent::TrackSubscribed { track, publication, .. } => {
            if ctx.config.role != Role::Operator {
                return;
            }
            if let RemoteTrack::Video(video_track) = track {
                let track_name = publication.name();
                if ctx.config.video_tracks.contains(&track_name.to_string()) {
                    log::info!(
                        "[{}] subscribed to video track '{track_name}'",
                        ctx.config.session
                    );
                    if let Some(sync_buffer) = &ctx.sync_buffer {
                        let raw_cb = ctx
                            .video_cbs
                            .get(track_name.as_str())
                            .cloned()
                            .unwrap_or_else(|| Arc::new(Mutex::new(None)));
                        let track_metrics = ctx
                            .metrics
                            .track(track_name.as_str())
                            .expect("track metrics registered at construction");

                        let stream = NativeVideoStream::new(video_track.rtc_track());
                        let receiver = VideoReceiver::spawn(
                            track_name.to_string(),
                            stream,
                            sync_buffer.clone(),
                            raw_cb,
                            ctx.obs_sink.clone(),
                            track_metrics,
                        );
                        ctx.video_receivers.lock().insert(track_name.to_string(), receiver);
                    }
                }
            }
        }
        RoomEvent::DataReceived { payload, topic, .. } => {
            if let Some(topic) = &topic {
                let output = handle_data_received(
                    &payload,
                    topic,
                    ctx.config.role,
                    &ctx.config.action_fields,
                    &ctx.config.state_fields,
                    &ctx.action_cb,
                    &ctx.state_cb,
                    ctx.sync_buffer.as_ref(),
                    &ctx.metrics,
                    &ctx.rtt,
                );
                if !output.is_empty() {
                    ctx.obs_sink.dispatch(output);
                }
            }
        }
        RoomEvent::Reconnected => {
            log::info!("[{}] reconnected, clearing sync buffers", ctx.config.session);
            if let Some(sb) = &ctx.sync_buffer {
                sb.lock().clear();
            }
        }
        _ => {}
    }
}
