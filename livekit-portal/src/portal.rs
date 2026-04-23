use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use livekit::prelude::*;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::config::PortalConfig;
use crate::data::{handle_data_received, DataPublisher, DataSlots};
use crate::error::{PortalError, PortalResult};
use crate::metrics::{DataStream, MetricsRegistry, PortalMetrics};
use crate::rpc::RpcHandler;
use crate::rtt::RttService;
use crate::serialization::schema_fingerprint;
use crate::sync_buffer::{SyncBuffer, SyncOutput};
use crate::types::*;
use crate::video::{VideoPublisher, VideoReceiver, VideoTrackSlots};

type ObservationCb = Box<dyn Fn(&Observation) + Send + Sync>;
type DropCb = Box<dyn Fn(Vec<HashMap<String, f64>>) + Send + Sync>;

/// Drains the buffers returned by `SyncBuffer::push_*` and dispatches them to
/// the user — callback first (by reference, no clone), then into the pull-based
/// observation buffer. Kept separate from `SyncBuffer` so callbacks run with no
/// sync-buffer lock held.
pub(crate) struct ObservationSink {
    observation_cb: Mutex<Option<ObservationCb>>,
    drop_cb: Mutex<Option<DropCb>>,
    // Latest-wins slot. Consumers peek via `get()` (clone). Consumers that
    // want history register `on_observation` and buffer on their own side.
    latest: Mutex<Option<Observation>>,
}

impl ObservationSink {
    pub(crate) fn new() -> Self {
        Self {
            observation_cb: Mutex::new(None),
            drop_cb: Mutex::new(None),
            latest: Mutex::new(None),
        }
    }

    pub(crate) fn dispatch(&self, output: SyncOutput) {
        let SyncOutput { observations, drops } = output;

        if !observations.is_empty() {
            {
                let cb_slot = self.observation_cb.lock();
                if let Some(cb) = cb_slot.as_ref() {
                    for obs in &observations {
                        cb(obs);
                    }
                }
            }
            // Latest-wins: only the final observation needs to reach the pull
            // slot — intermediates are discarded either way.
            if let Some(last_obs) = observations.into_iter().last() {
                *self.latest.lock() = Some(last_obs);
            }
        }

        if !drops.is_empty() {
            if let Some(cb) = self.drop_cb.lock().as_ref() {
                cb(drops);
            }
        }
    }

    pub(crate) fn get(&self) -> Option<Observation> {
        self.latest.lock().clone()
    }

    pub(crate) fn clear(&self) {
        *self.latest.lock() = None;
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
    local_participant: Option<LocalParticipant>,
    event_task: Option<JoinHandle<()>>,
    rtt: Option<Arc<RttService>>,
}

pub struct Portal {
    config: PortalConfig,

    // Serializes connect()/disconnect() so a disconnect() yielding on
    // room.close().await can't be overtaken by a concurrent connect()
    // whose newly-populated state would then be clobbered by the
    // disconnect's cleanup path.
    lifecycle: tokio::sync::Mutex<()>,

    // Lifecycle state (connect/disconnect).
    conn: Mutex<ConnectionState>,

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

    // Push callback + pull latest-wins slot, bundled per stream.
    action: Arc<DataSlots>,
    state: Arc<DataSlots>,
    // Fixed at construction (keyed by declared video_tracks) — no lock on the map itself.
    video_tracks: HashMap<String, Arc<VideoTrackSlots>>,

    metrics: Arc<MetricsRegistry>,

    // The opposite-role participant, if one has been observed via Portal
    // traffic (data topic or video subscription). Set lazily on the first
    // matching event; cleared on disconnect, reconnect, and when that
    // participant leaves the room.
    peer_identity: Arc<Mutex<Option<ParticipantIdentity>>>,

    // RPC methods the caller has registered. Applied to the LocalParticipant
    // on connect(); survives disconnect so reconnects reapply them.
    rpc_handlers: Arc<Mutex<HashMap<String, RpcHandler>>>,
}

impl Portal {
    pub fn new(config: PortalConfig) -> Self {
        let video_tracks: HashMap<_, _> = config
            .video_tracks
            .iter()
            .map(|name| (name.clone(), Arc::new(VideoTrackSlots::new())))
            .collect();

        let metrics = Arc::new(MetricsRegistry::new(&config.video_tracks));
        let obs_sink = Arc::new(ObservationSink::new());

        Self {
            config,
            lifecycle: tokio::sync::Mutex::new(()),
            conn: Mutex::new(ConnectionState {
                room: None,
                local_participant: None,
                event_task: None,
                rtt: None,
            }),
            video_receivers: Arc::new(Mutex::new(HashMap::new())),
            video_publishers: Mutex::new(HashMap::new()),
            state_publisher: Mutex::new(None),
            action_publisher: Mutex::new(None),
            sync_buffer: Mutex::new(None),
            obs_sink,
            action: Arc::new(DataSlots::new()),
            state: Arc::new(DataSlots::new()),
            video_tracks,
            metrics,
            peer_identity: Arc::new(Mutex::new(None)),
            rpc_handlers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn connect(&self, url: &str, token: &str) -> PortalResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.conn.lock().room.is_some() {
            return Err(PortalError::AlreadyConnected);
        }

        let mut options = RoomOptions::default();
        options.auto_subscribe = true;

        log::info!("[{}] connecting as {:?} to {}", self.config.session, self.config.role, url);

        let (room, events) = Room::connect(url, token, options)
            .await
            .map_err(|e| PortalError::Room(e.to_string()))?;

        // Store the LocalParticipant before applying handlers so a concurrent
        // `register_rpc_method` either (a) inserts before we iterate and gets
        // picked up, or (b) inserts after we've stored LP and forwards the
        // handler itself. Overlap is idempotent — the SDK's rpc handler map
        // is last-writer-wins.
        let local_participant = room.local_participant();
        self.conn.lock().local_participant = Some(local_participant.clone());
        self.apply_rpc_handlers(&local_participant);

        match self.config.role {
            Role::Robot => self.setup_robot(&room).await?,
            Role::Operator => self.setup_operator(&room),
        }

        let rtt = Arc::new(RttService::spawn(
            local_participant.clone(),
            self.config.ping_ms,
            self.metrics.clone(),
        ));

        log::info!("[{}] connected as {:?}", self.config.session, self.config.role);

        // Event dispatch runs off a snapshot of the fields it touches, not the
        // whole Portal, so it doesn't need any outer lock.
        let action_schema_fp = schema_fingerprint(&self.config.action_schema);
        let state_schema_fp = schema_fingerprint(&self.config.state_schema);
        let ctx = EventContext {
            config: self.config.clone(),
            action_schema_fp,
            state_schema_fp,
            sync_buffer: self.sync_buffer.lock().clone(),
            obs_sink: self.obs_sink.clone(),
            action: self.action.clone(),
            state: self.state.clone(),
            video_tracks: self.video_tracks.clone(),
            video_receivers: self.video_receivers.clone(),
            metrics: self.metrics.clone(),
            rtt: rtt.clone(),
            peer_identity: self.peer_identity.clone(),
        };
        let event_handle = tokio::spawn(async move {
            let mut events = events;
            while let Some(event) = events.recv().await {
                handle_room_event(&ctx, event);
            }
        });

        let mut state = self.conn.lock();
        state.room = Some(room);
        // local_participant was set earlier (before apply_rpc_handlers).
        state.event_task = Some(event_handle);
        state.rtt = Some(rtt);
        Ok(())
    }

    pub fn send_video_frame(
        &self,
        track_name: &str,
        rgb_data: &[u8],
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
        publisher.send_frame(rgb_data, width, height, timestamp_us)
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

    // --- RPC ---

    /// Identity of the opposite-role participant Portal has identified, if
    /// any. Latches on the first Portal-topic data packet or video track
    /// subscription from a remote. `None` before the peer has spoken.
    pub fn peer_identity(&self) -> Option<String> {
        self.peer_identity.lock().as_ref().map(|p| p.as_str().to_string())
    }

    /// Register an RPC method handler. Handlers can be registered before or
    /// after `connect()`; stored handlers are (re)applied to the
    /// `LocalParticipant` on each connect.
    pub fn register_rpc_method(&self, method: &str, handler: RpcHandler) {
        {
            let mut map = self.rpc_handlers.lock();
            map.insert(method.to_string(), handler.clone());
        }
        if let Some(lp) = self.conn.lock().local_participant.clone() {
            register_handler_on(&lp, method.to_string(), handler);
        }
    }

    /// Remove a previously registered RPC method handler.
    pub fn unregister_rpc_method(&self, method: &str) {
        self.rpc_handlers.lock().remove(method);
        if let Some(lp) = self.conn.lock().local_participant.clone() {
            lp.unregister_rpc_method(method.to_string());
        }
    }

    /// Invoke a registered method on the peer. `destination` is optional;
    /// when omitted, the call is routed to Portal's identified peer (see
    /// `peer_identity`), falling back to the single remote participant if
    /// no peer has been identified yet. Errors with `NoPeer` or
    /// `AmbiguousPeer` when neither resolves to a unique destination.
    pub async fn perform_rpc(
        &self,
        destination: Option<&str>,
        method: &str,
        payload: String,
        response_timeout: Option<Duration>,
    ) -> PortalResult<String> {
        let destination = match destination {
            Some(id) => id.to_string(),
            None => self.resolve_peer()?,
        };
        let lp = self
            .conn
            .lock()
            .local_participant
            .clone()
            .ok_or(PortalError::NotConnected)?;

        let mut data = PerformRpcData {
            destination_identity: destination,
            method: method.to_string(),
            payload,
            ..Default::default()
        };
        if let Some(t) = response_timeout {
            data.response_timeout = t;
        }

        lp.perform_rpc(data).await.map_err(|e| PortalError::Rpc(e.into()))
    }

    /// Pick a destination identity from `peer_identity` if latched, else fall
    /// back to the room's remote-participant snapshot (single-peer → use it,
    /// empty → NoPeer, multiple → AmbiguousPeer).
    fn resolve_peer(&self) -> PortalResult<String> {
        if let Some(id) = self.peer_identity.lock().as_ref() {
            return Ok(id.as_str().to_string());
        }
        let conn = self.conn.lock();
        let room = conn.room.as_ref().ok_or(PortalError::NotConnected)?;
        let remotes = room.remote_participants();
        match remotes.len() {
            0 => Err(PortalError::NoPeer),
            1 => {
                let (id, _) = remotes.into_iter().next().expect("remotes has one entry");
                Ok(id.as_str().to_string())
            }
            _ => Err(PortalError::AmbiguousPeer),
        }
    }

    /// Apply every stored handler to a freshly-connected LocalParticipant.
    /// Called once from `connect()` after the Room is up.
    fn apply_rpc_handlers(&self, lp: &LocalParticipant) {
        let handlers = self.rpc_handlers.lock().clone();
        for (method, handler) in handlers {
            register_handler_on(lp, method, handler);
        }
    }

    pub async fn disconnect(&self) -> PortalResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let room = self.conn.lock().room.take();
        log::info!("disconnecting");

        // close() is best-effort; cleanup must happen even if it errors,
        // otherwise the Portal would be half-disconnected (room=None but
        // tasks/publishers still running) and the next connect() would race.
        let close_result = match room {
            Some(room) => room.close().await.map_err(|e| PortalError::Room(e.to_string())),
            None => Ok(()),
        };

        {
            let mut state = self.conn.lock();
            if let Some(task) = state.event_task.take() {
                task.abort();
            }
            state.rtt = None;
            state.local_participant = None;
        }
        *self.peer_identity.lock() = None;
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
        self.action.clear();
        self.state.clear();
        for slots in self.video_tracks.values() {
            slots.clear();
        }

        close_result
    }

    // --- Pull API (latest-wins, peek semantics) ---

    /// Clone of the latest observation, or `None` if none received yet.
    /// Consumers wanting a history of observations should register
    /// `on_observation` and buffer on their own side.
    pub fn get_observation(&self) -> Option<Observation> {
        self.obs_sink.get()
    }

    /// Clone of the latest action received (Robot side), or `None`. The
    /// `u64` is the sender's wall-clock timestamp in microseconds.
    pub fn get_action(&self) -> Option<(u64, HashMap<String, f64>)> {
        self.action.latest.lock().clone()
    }

    /// Clone of the latest state received (Operator side), or `None`. The
    /// `u64` is the sender's wall-clock timestamp in microseconds.
    pub fn get_state(&self) -> Option<(u64, HashMap<String, f64>)> {
        self.state.latest.lock().clone()
    }

    /// Clone of the latest frame received for `track_name`, or `None`.
    pub fn get_video_frame(&self, track_name: &str) -> Option<VideoFrameData> {
        self.video_tracks.get(track_name).and_then(|s| s.latest.lock().clone())
    }

    // --- Callback registration (push API) ---

    pub fn on_action(
        &self,
        callback: impl Fn(u64, &HashMap<String, f64>) + Send + Sync + 'static,
    ) {
        *self.action.cb.lock() = Some(Box::new(callback));
    }

    pub fn on_observation(&self, callback: impl Fn(&Observation) + Send + Sync + 'static) {
        self.obs_sink.set_observation_cb(Box::new(callback));
    }

    pub fn on_state(
        &self,
        callback: impl Fn(u64, &HashMap<String, f64>) + Send + Sync + 'static,
    ) {
        *self.state.cb.lock() = Some(Box::new(callback));
    }

    pub fn on_video_frame(
        &self,
        track_name: &str,
        callback: impl Fn(&str, &VideoFrameData) + Send + Sync + 'static,
    ) {
        match self.video_tracks.get(track_name) {
            Some(slots) => *slots.cb.lock() = Some(Box::new(callback)),
            None => log::warn!(
                "on_video_frame: track '{track_name}' is not registered — callback ignored"
            ),
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
            let publisher = VideoPublisher::new(track_name, track_metrics, self.config.fps);
            if let Err(e) = publisher.publish(&lp).await {
                // Roll back any earlier publishers so their send tasks stop
                // and connect() leaves Portal in a clean state.
                self.video_publishers.lock().clear();
                return Err(e);
            }
            log::info!("[{}] published video track '{track_name}'", self.config.session);
            self.video_publishers.lock().insert(track_name.clone(), Arc::new(publisher));
        }

        if !self.config.state_schema.is_empty() {
            let publisher = DataPublisher::new(
                &self.config.state_schema,
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
                self.config.state_schema.len()
            );
            *self.state_publisher.lock() = Some(Arc::new(publisher));
        }

        Ok(())
    }

    fn setup_operator(&self, room: &Room) {
        let lp = room.local_participant();

        let sync_buffer = Arc::new(Mutex::new(SyncBuffer::new(
            &self.config.video_tracks,
            self.config.state_fields().map(String::from).collect(),
            self.config.sync_config(),
            self.metrics.clone(),
        )));
        *self.sync_buffer.lock() = Some(sync_buffer);

        if !self.config.action_schema.is_empty() {
            let mode = if self.config.action_reliable { "reliable" } else { "unreliable" };
            log::info!(
                "[{}] ready to publish action via {mode} data ({} fields)",
                self.config.session,
                self.config.action_schema.len()
            );
            let publisher = DataPublisher::new(
                &self.config.action_schema,
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
        self.metrics.snapshot(video_fill, state_fill)
    }

    pub fn reset_metrics(&self) {
        self.metrics.reset();
    }
}

/// Wrap a Portal `RpcHandler` in the signature the SDK expects and install
/// it on the given LocalParticipant. Payload types are converted at the
/// boundary — the SDK's `RpcInvocationData` / `RpcError` never leak into
/// caller-facing code.
fn register_handler_on(lp: &LocalParticipant, method: String, handler: RpcHandler) {
    lp.register_rpc_method(method, move |data| {
        let handler = handler.clone();
        Box::pin(async move {
            let core_data: crate::rpc::RpcInvocationData = data.into();
            handler(core_data).await.map_err(Into::into)
        })
    });
}

/// Snapshot of the fields the room event loop needs, so it doesn't take any
/// Portal-level lock on the hot path.
struct EventContext {
    config: PortalConfig,
    /// Cached schema fingerprints so the receive hot path doesn't recompute
    /// them per packet. Matches the peer's fingerprint when schemas agree;
    /// a mismatch logs once per offending value and drops the packet.
    action_schema_fp: u32,
    state_schema_fp: u32,
    sync_buffer: Option<Arc<Mutex<SyncBuffer>>>,
    obs_sink: Arc<ObservationSink>,
    action: Arc<DataSlots>,
    state: Arc<DataSlots>,
    video_tracks: HashMap<String, Arc<VideoTrackSlots>>,
    video_receivers: Arc<Mutex<HashMap<String, VideoReceiver>>>,
    metrics: Arc<MetricsRegistry>,
    rtt: Arc<RttService>,
    peer_identity: Arc<Mutex<Option<ParticipantIdentity>>>,
}

/// Latch `identity` as the peer if we haven't identified one yet. Logged once
/// per connection — subsequent calls are cheap no-ops.
fn latch_peer(
    peer_identity: &Mutex<Option<ParticipantIdentity>>,
    session: &str,
    identity: ParticipantIdentity,
) {
    let mut slot = peer_identity.lock();
    if slot.is_none() {
        log::info!("[{session}] identified peer '{}'", identity.as_str());
        *slot = Some(identity);
    }
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
                        let slots = ctx
                            .video_tracks
                            .get(track_name.as_str())
                            .cloned()
                            .unwrap_or_else(|| Arc::new(VideoTrackSlots::new()));
                        let track_metrics = ctx
                            .metrics
                            .track(track_name.as_str())
                            .expect("track metrics registered at construction");

                        let stream = NativeVideoStream::new(video_track.rtc_track());
                        let receiver = VideoReceiver::spawn(
                            track_name.to_string(),
                            stream,
                            sync_buffer.clone(),
                            slots,
                            ctx.obs_sink.clone(),
                            track_metrics,
                        );
                        ctx.video_receivers.lock().insert(track_name.to_string(), receiver);
                    }
                }
            }
        }
        RoomEvent::DataReceived { payload, topic: Some(topic), participant, .. } => {
            // Latch peer on the first Portal-topic packet from a remote.
            // RTT packets count too — they originate from the same peer.
            if let Some(p) = &participant {
                if matches!(
                    (ctx.config.role, topic.as_str()),
                    (Role::Robot, "portal_action")
                        | (Role::Operator, "portal_state")
                        | (_, "portal_rtt")
                ) {
                    latch_peer(&ctx.peer_identity, &ctx.config.session, p.identity());
                }
            }
            let output = handle_data_received(
                &payload,
                &topic,
                ctx.config.role,
                &ctx.config.action_schema,
                ctx.action_schema_fp,
                &ctx.config.state_schema,
                ctx.state_schema_fp,
                &ctx.action,
                &ctx.state,
                ctx.sync_buffer.as_ref(),
                &ctx.metrics,
                &ctx.rtt,
            );
            if !output.is_empty() {
                ctx.obs_sink.dispatch(output);
            }
        }
        RoomEvent::ParticipantDisconnected(participant) => {
            let mut slot = ctx.peer_identity.lock();
            if slot.as_ref() == Some(&participant.identity()) {
                log::info!(
                    "[{}] peer '{}' disconnected",
                    ctx.config.session,
                    participant.identity().as_str()
                );
                *slot = None;
            }
        }
        RoomEvent::Reconnected => {
            log::info!(
                "[{}] reconnected, clearing sync buffers and latest slots",
                ctx.config.session
            );
            if let Some(sb) = &ctx.sync_buffer {
                sb.lock().clear();
            }
            // Pre-reconnect data is stale by definition; consumers calling
            // get_* after a reconnect should see None until fresh packets
            // arrive, matching the semantics already applied to sync_buffer.
            ctx.obs_sink.clear();
            ctx.action.clear();
            ctx.state.clear();
            for slots in ctx.video_tracks.values() {
                slots.clear();
            }
            *ctx.peer_identity.lock() = None;
        }
        _ => {}
    }
}
