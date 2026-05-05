use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use livekit::prelude::*;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::config::{ChunkSpec, FieldSpec, PortalConfig};
use crate::data::{
    dispatch_chunk_payload, handle_data_received, ActionSlot, ChunkPublisher, ChunkSlot,
    DataPublisher, StateSlot, ACTION_CHUNK_TOPIC, ACTION_TOPIC, STATE_TOPIC,
};
use crate::error::{PortalError, PortalResult};
use crate::frame_video::{
    dispatch_frame_payload, FrameVideoPublisher, FrameVideoTrackEntry, FRAME_VIDEO_TOPIC,
};
use crate::metrics::{DataStream, MetricsRegistry, PortalMetrics};
use crate::rpc::RpcHandler;
use crate::rtt::RttService;
use crate::serialization::{action_fingerprint, schema_fingerprint};
use crate::sync_buffer::{SyncBuffer, SyncOutput};
use crate::types::*;
use crate::video::{VideoPublisher, VideoReceiver, VideoTrackSlots};

type ObservationCb = Box<dyn Fn(&Observation) + Send + Sync>;
type DropCb = Box<dyn Fn(Vec<HashMap<String, TypedValue>>) + Send + Sync>;

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

        // User callbacks run on the tokio worker dispatching room events.
        // A panic here would abort the whole event loop, so we catch and
        // log and keep going.
        if !observations.is_empty() {
            {
                let cb_slot = self.observation_cb.lock();
                if let Some(cb) = cb_slot.as_ref() {
                    for obs in &observations {
                        let result = catch_unwind(AssertUnwindSafe(|| cb(obs)));
                        if result.is_err() {
                            log::error!(
                                "observation callback panicked; event loop continues"
                            );
                        }
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
                let result = catch_unwind(AssertUnwindSafe(|| cb(drops)));
                if result.is_err() {
                    log::error!("drop callback panicked; event loop continues");
                }
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
    /// Robot-side: one publisher per declared frame-video track. Frame-video
    /// frames travel as byte streams (per-frame RGB encode), bypassing the
    /// WebRTC media path.
    frame_video_publishers: Mutex<HashMap<String, Arc<FrameVideoPublisher>>>,
    state_publisher: Mutex<Option<Arc<DataPublisher>>>,
    action_publisher: Mutex<Option<Arc<DataPublisher>>>,
    /// Operator-side: one publisher per declared action chunk.
    chunk_publishers: Mutex<HashMap<String, Arc<ChunkPublisher>>>,

    // Operator-side sync + dispatch.
    sync_buffer: Mutex<Option<Arc<Mutex<SyncBuffer>>>>,
    obs_sink: Arc<ObservationSink>,

    // Push callback + pull latest-wins slot, bundled per stream.
    action: Arc<ActionSlot>,
    state: Arc<StateSlot>,
    /// Robot-side: one slot per declared action chunk. Fixed at construction
    /// (keyed by chunk name) so the receive path doesn't lock the map.
    chunk_slots: HashMap<String, Arc<ChunkSlot>>,
    /// Rate-limit set for unknown chunk fingerprints — the byte-stream
    /// equivalent of `DataSlot::warned_mismatches`, but lives at the
    /// dispatcher level because no slot owns "unknown" packets.
    unknown_chunk_fp_warns: Arc<Mutex<HashSet<u32>>>,
    // Fixed at construction (keyed by declared video_tracks) — no lock on the map itself.
    video_tracks: HashMap<String, Arc<VideoTrackSlots>>,
    /// Names of all video tracks (WebRTC + frame video) in declaration
    /// order. Used by `setup_operator` to size the sync buffer over the
    /// union of transports. Computed once at `Portal::new` so the connect
    /// hot path doesn't re-walk the config.
    all_track_names: Vec<String>,
    /// Per-track frame-video entries (spec + slots + metrics fused). Fixed
    /// at construction and shared as an `Arc<HashMap>` so the receive
    /// dispatch can fan out to per-frame spawn tasks via a refcount bump
    /// instead of cloning the whole map (which would allocate one `String`
    /// per declared track per received frame).
    frame_video_entries: Arc<HashMap<String, Arc<FrameVideoTrackEntry>>>,

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
        // Slots and metrics cover both transports. Frame-video and WebRTC
        // tracks share the same VideoFrameData / VideoTrackSlots / sync
        // buffer, so the consumer-facing API is identical.
        let all_track_names = combined_track_names(&config);
        let video_tracks: HashMap<String, Arc<VideoTrackSlots>> = all_track_names
            .iter()
            .map(|name| (name.clone(), Arc::new(VideoTrackSlots::new())))
            .collect();

        let metrics = Arc::new(MetricsRegistry::new(&all_track_names));
        let obs_sink = Arc::new(ObservationSink::new());

        // Build chunk slots once at construction so the dispatch table is
        // immutable for the Portal's lifetime — `handle_room_event` reads
        // them without taking any Portal-level lock.
        let chunk_slots: HashMap<String, Arc<ChunkSlot>> = config
            .action_chunks
            .iter()
            .map(|spec| (spec.name.clone(), Arc::new(ChunkSlot::new(spec.clone()))))
            .collect();

        // Same idea for frame-video entries: the dispatch path reads them
        // per packet, so freezing the map at construction lets the hot path
        // skip a Portal-level lock and the per-connect rebuild. Each entry
        // bundles spec + slots + metrics so dispatch is a single lookup.
        // Wrapped in `Arc<HashMap>` so per-frame fan-out is a refcount bump
        // rather than a `String`-cloning map clone.
        let frame_video_entries: Arc<HashMap<String, Arc<FrameVideoTrackEntry>>> = Arc::new(
            config
                .frame_video_tracks
                .iter()
                .map(|spec| {
                    let slots = video_tracks
                        .get(&spec.name)
                        .expect("video_tracks contains every frame-video name")
                        .clone();
                    let track_metrics = metrics
                        .track(&spec.name)
                        .expect("track metrics registered for every frame-video name");
                    (
                        spec.name.clone(),
                        Arc::new(FrameVideoTrackEntry {
                            spec: spec.clone(),
                            metrics: track_metrics,
                            slots,
                        }),
                    )
                })
                .collect(),
        );

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
            frame_video_publishers: Mutex::new(HashMap::new()),
            state_publisher: Mutex::new(None),
            action_publisher: Mutex::new(None),
            chunk_publishers: Mutex::new(HashMap::new()),
            sync_buffer: Mutex::new(None),
            obs_sink,
            action: Arc::new(ActionSlot::new()),
            state: Arc::new(StateSlot::new()),
            chunk_slots,
            unknown_chunk_fp_warns: Arc::new(Mutex::new(HashSet::new())),
            video_tracks,
            all_track_names,
            frame_video_entries,
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
        if let Some(key) = &self.config.shared_key {
            use livekit::e2ee::{key_provider::{KeyProvider, KeyProviderOptions}, EncryptionType};
            use livekit::E2eeOptions;
            let key_provider = KeyProvider::with_shared_key(KeyProviderOptions::default(), key.clone());
            options.encryption = Some(E2eeOptions { key_provider, encryption_type: EncryptionType::Gcm });
        }

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
        let action_schema_fp = action_fingerprint(&self.config.action_schema);
        let state_schema_fp = schema_fingerprint(&self.config.state_schema);
        // The dispatch path needs a slice for fingerprint lookup; the map
        // form is for `get_action_chunk` / `on_action_chunk` name lookups.
        // Build the slice once per connect so the event loop iterates a
        // plain Vec, not a HashMap.
        let chunk_slots_for_dispatch: Vec<Arc<ChunkSlot>> =
            self.chunk_slots.values().cloned().collect();
        let ctx = EventContext {
            config: self.config.clone(),
            action_schema_fp,
            state_schema_fp,
            sync_buffer: self.sync_buffer.lock().clone(),
            obs_sink: self.obs_sink.clone(),
            action: self.action.clone(),
            state: self.state.clone(),
            chunk_slots: chunk_slots_for_dispatch,
            unknown_chunk_fp_warns: self.unknown_chunk_fp_warns.clone(),
            video_tracks: self.video_tracks.clone(),
            video_receivers: self.video_receivers.clone(),
            frame_video_entries: self.frame_video_entries.clone(),
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
        // Two transports, one user-facing method. WebRTC publishers and
        // frame-video publishers are populated by `add_video` at config
        // time — codec selection routes the spec to one list or the
        // other — and names are unique across both, so a track lives in
        // exactly one map.
        if let Some(publisher) = self.video_publishers.lock().get(track_name).cloned() {
            return publisher.send_frame(rgb_data, width, height, timestamp_us);
        }
        if let Some(publisher) = self.frame_video_publishers.lock().get(track_name).cloned() {
            return publisher.send_frame(rgb_data, width, height, timestamp_us);
        }
        // Distinguish wrong-role (track is declared but no publisher exists
        // because send is operator-side) from genuinely unknown-track. The
        // operator never spawns video publishers, so a declared name with
        // no publisher means "wrong role" — same shape as `send_state` /
        // `send_action_chunk`.
        if self.config.role != Role::Robot
            && (self.config.video_tracks.iter().any(|n| n == track_name)
                || self
                    .config
                    .frame_video_tracks
                    .iter()
                    .any(|s| s.name == track_name))
        {
            return Err(PortalError::WrongRole(self.config.role));
        }
        Err(PortalError::UnknownVideoTrack { name: track_name.to_string() })
    }

    /// Publish a state sample (robot only). Values are typed — build the
    /// map with `TypedValue::Bool(true)`, `0.5f32.into()`, etc. The
    /// pipeline internally widens to `f64` for carry-forward and casts
    /// back to the declared dtype at the wire boundary.
    pub fn send_state(
        &self,
        values: &HashMap<String, TypedValue>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let publisher = self
            .state_publisher
            .lock()
            .clone()
            .ok_or(PortalError::WrongRole(Role::Operator))?;
        publisher.send_map(values, timestamp_us, None)
    }

    /// Publish an action (operator only).
    ///
    /// `in_reply_to_ts_us` is the timestamp of the observation this action
    /// was produced from — pass `Some(obs.timestamp_us)` to give the
    /// receiver the data it needs to compute true end-to-end policy
    /// latency (`metrics.policy.e2e_us_*`). Pass `None` for unsolicited
    /// publishes (teleop, idle commands).
    pub fn send_action(
        &self,
        values: &HashMap<String, TypedValue>,
        timestamp_us: Option<u64>,
        in_reply_to_ts_us: Option<u64>,
    ) -> PortalResult<()> {
        let publisher =
            self.action_publisher.lock().clone().ok_or(PortalError::WrongRole(Role::Robot))?;
        publisher.send_map(values, timestamp_us, in_reply_to_ts_us)
    }

    /// Publish an action chunk on the named chunk schema (operator only).
    ///
    /// `data` is `field -> column of length horizon`. Columns shorter than
    /// `horizon` are zero-padded, longer columns are truncated, and unknown
    /// keys are warned-and-ignored once each. Use `in_reply_to_ts_us` the
    /// same way as `send_action` to feed `metrics.policy.e2e_us_*`.
    pub fn send_action_chunk(
        &self,
        chunk_name: &str,
        data: &HashMap<String, Vec<f64>>,
        timestamp_us: Option<u64>,
        in_reply_to_ts_us: Option<u64>,
    ) -> PortalResult<()> {
        let publisher = {
            let map = self.chunk_publishers.lock();
            map.get(chunk_name).cloned()
        };
        let Some(publisher) = publisher else {
            // No publisher resolves to one of three precise errors so the
            // caller sees the actual mistake instead of a generic refusal:
            // wrong role, undeclared chunk name, or operator-but-not-yet
            // connected (publishers are spawned in `setup_operator`).
            return if self.config.role != Role::Operator {
                Err(PortalError::WrongRole(Role::Robot))
            } else if !self.chunk_slots.contains_key(chunk_name) {
                Err(PortalError::UnknownChunk { name: chunk_name.to_string() })
            } else {
                Err(PortalError::NotConnected)
            };
        };
        publisher.send(data, timestamp_us, in_reply_to_ts_us)
    }

    // --- RPC ---

    /// Declared state schema (field names + dtypes), in declaration order.
    /// Bindings mirror this snapshot internally; reading from the Portal
    /// keeps the snapshot single-sourced.
    pub fn state_schema(&self) -> &[FieldSpec] {
        self.config.state_schema()
    }

    /// Declared action schema, same semantics as `state_schema`.
    pub fn action_schema(&self) -> &[FieldSpec] {
        self.config.action_schema()
    }

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
        self.frame_video_publishers.lock().clear();
        *self.state_publisher.lock() = None;
        *self.action_publisher.lock() = None;
        self.chunk_publishers.lock().clear();

        if let Some(sb) = self.sync_buffer.lock().take() {
            sb.lock().clear();
        }
        self.obs_sink.clear();
        self.action.clear();
        self.state.clear();
        for slot in self.chunk_slots.values() {
            slot.clear();
        }
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

    /// Clone of the latest action received (Robot side), or `None`.
    /// `.values` holds typed values per the declared schema; `.raw_values`
    /// is the lossless `f64` view.
    pub fn get_action(&self) -> Option<Action> {
        self.action.get()
    }

    /// Clone of the latest state received (Operator side), or `None`.
    /// Typed per the declared schema.
    pub fn get_state(&self) -> Option<State> {
        self.state.get()
    }

    /// Clone of the latest frame received for `track_name`, or `None`.
    pub fn get_video_frame(&self, track_name: &str) -> Option<VideoFrameData> {
        self.video_tracks.get(track_name).and_then(|s| s.latest.lock().clone())
    }

    /// Clone of the latest chunk received for `chunk_name`, or `None` if
    /// none received yet (or the chunk wasn't declared).
    pub fn get_action_chunk(&self, chunk_name: &str) -> Option<ActionChunk> {
        self.chunk_slots.get(chunk_name).and_then(|s| s.get())
    }

    /// All declared action chunk schemas, in declaration order.
    pub fn action_chunks(&self) -> &[ChunkSpec] {
        self.config.action_chunks()
    }

    // --- Callback registration (push API) ---

    /// Fire on every received action. The `Action` record exposes typed
    /// values per the declared schema plus `raw_values` for the lossless
    /// `f64` view.
    pub fn on_action(&self, callback: impl Fn(&Action) + Send + Sync + 'static) {
        *self.action.cb.lock() = Some(Box::new(callback));
    }

    /// Fire on every received chunk for the named declaration. Only one
    /// callback per chunk; calling twice overwrites. Unknown names are
    /// logged and ignored — they aren't a hard error because the chunk
    /// schema may have been intentionally omitted on this peer.
    pub fn on_action_chunk(
        &self,
        chunk_name: &str,
        callback: impl Fn(&ActionChunk) + Send + Sync + 'static,
    ) {
        match self.chunk_slots.get(chunk_name) {
            Some(slot) => slot.set_callback(Box::new(callback)),
            None => log::warn!(
                "on_action_chunk: chunk '{chunk_name}' is not declared — callback ignored"
            ),
        }
    }

    pub fn on_observation(&self, callback: impl Fn(&Observation) + Send + Sync + 'static) {
        self.obs_sink.set_observation_cb(Box::new(callback));
    }

    /// Fire on every received state. Semantics mirror `on_action`.
    pub fn on_state(&self, callback: impl Fn(&State) + Send + Sync + 'static) {
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

    /// Fire on every batch of state samples that couldn't be matched to a
    /// video frame. Each entry is the typed state payload (same shape as
    /// `Observation.state`).
    pub fn on_drop(
        &self,
        callback: impl Fn(Vec<HashMap<String, TypedValue>>) + Send + Sync + 'static,
    ) {
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

        // Frame-video publishers don't go through `LocalParticipant.publish_track`
        // — they emit one byte stream per frame instead. So no async setup
        // here, just spawn the per-track drainer task.
        for spec in &self.config.frame_video_tracks {
            let track_metrics = self
                .metrics
                .track(&spec.name)
                .expect("track metrics registered at construction");
            let publisher =
                FrameVideoPublisher::new(spec.clone(), lp.clone(), track_metrics);
            log::info!(
                "[{}] ready to publish frame-video track '{}' via byte stream (codec={:?}, quality={})",
                self.config.session,
                spec.name,
                spec.codec,
                spec.quality
            );
            self.frame_video_publishers
                .lock()
                .insert(spec.name.clone(), Arc::new(publisher));
        }

        if !self.config.state_schema.is_empty() {
            let publisher = DataPublisher::new(
                &self.config.state_schema,
                STATE_TOPIC,
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

        // Sync buffer treats both transports the same way — it tracks frame
        // arrivals by name, regardless of whether they came from a WebRTC
        // RTP track or a frame-video byte stream. `all_track_names` was
        // computed once at construction.
        let sync_buffer = Arc::new(Mutex::new(SyncBuffer::new(
            &self.all_track_names,
            self.config.state_schema.clone(),
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
                ACTION_TOPIC,
                self.config.action_reliable,
                lp.clone(),
                self.metrics.clone(),
                DataStream::Action,
            );
            *self.action_publisher.lock() = Some(Arc::new(publisher));
        }

        if !self.config.action_chunks.is_empty() {
            for spec in &self.config.action_chunks {
                log::info!(
                    "[{}] ready to publish chunk '{}' via byte stream (horizon={}, {} fields)",
                    self.config.session,
                    spec.name,
                    spec.horizon,
                    spec.fields.len()
                );
                let publisher = ChunkPublisher::new(
                    spec.clone(),
                    lp.clone(),
                    self.metrics.clone(),
                );
                self.chunk_publishers
                    .lock()
                    .insert(spec.name.clone(), Arc::new(publisher));
            }
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
/// Names of every video track on a config, regardless of transport. Used
/// when registering metrics and sync-buffer slots, since the consumer-facing
/// API doesn't distinguish WebRTC and frame-video tracks.
fn combined_track_names(config: &PortalConfig) -> Vec<String> {
    let mut names: Vec<String> = config.video_tracks.clone();
    names.extend(config.frame_video_tracks.iter().map(|s| s.name.clone()));
    names
}

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
    action: Arc<ActionSlot>,
    state: Arc<StateSlot>,
    chunk_slots: Vec<Arc<ChunkSlot>>,
    unknown_chunk_fp_warns: Arc<Mutex<HashSet<u32>>>,
    video_tracks: HashMap<String, Arc<VideoTrackSlots>>,
    video_receivers: Arc<Mutex<HashMap<String, VideoReceiver>>>,
    /// Frame-video entries (spec + slots + metrics fused) keyed by track
    /// name. Shared as `Arc<HashMap>` so per-frame fan-out into spawn
    /// tasks bumps a refcount instead of cloning the map.
    frame_video_entries: Arc<HashMap<String, Arc<FrameVideoTrackEntry>>>,
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
                    (Role::Robot, ACTION_TOPIC)
                        | (Role::Operator, STATE_TOPIC)
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
        RoomEvent::ByteStreamOpened { reader, topic, participant_identity } => {
            // Two Portal byte-stream topics, each owned by a different role:
            //   * `portal_action_chunk` — operator → robot. Action chunks
            //     too big to fit in a 15 KB data packet.
            //   * `portal_frame_video`  — robot → operator. Per-frame
            //     RGB/PNG/MJPEG payloads that bypass the WebRTC media path.
            // We `take_if` on the topic so this Portal only consumes streams
            // it owns; other applications using byte streams on unrelated
            // topics are left untouched.
            match (ctx.config.role, topic.as_str()) {
                (Role::Robot, ACTION_CHUNK_TOPIC) => {
                    let Some(reader) =
                        reader.take_if(|info| info.topic == ACTION_CHUNK_TOPIC)
                    else {
                        return;
                    };
                    latch_peer(
                        &ctx.peer_identity,
                        &ctx.config.session,
                        participant_identity,
                    );
                    let chunk_slots = ctx.chunk_slots.clone();
                    let unknown_fp_warns = ctx.unknown_chunk_fp_warns.clone();
                    let metrics = ctx.metrics.clone();
                    tokio::spawn(async move {
                        use livekit::StreamReader;
                        match reader.read_all().await {
                            Ok(payload) => dispatch_chunk_payload(
                                &payload,
                                &chunk_slots,
                                &unknown_fp_warns,
                                &metrics,
                            ),
                            Err(e) => {
                                log::warn!("failed to read chunk byte stream: {e}")
                            }
                        }
                    });
                }
                (Role::Operator, FRAME_VIDEO_TOPIC) => {
                    // Operator-side: each byte stream carries one frame for
                    // some declared frame-video track. The header in the
                    // payload routes it to the right entry (spec + slots
                    // + metrics fused; one HashMap lookup at dispatch).
                    if ctx.frame_video_entries.is_empty() {
                        return;
                    }
                    let Some(reader) =
                        reader.take_if(|info| info.topic == FRAME_VIDEO_TOPIC)
                    else {
                        return;
                    };
                    let Some(sync_buffer) = ctx.sync_buffer.clone() else {
                        return;
                    };
                    latch_peer(
                        &ctx.peer_identity,
                        &ctx.config.session,
                        participant_identity,
                    );
                    // Refcount bumps only — no map or HashMap clone.
                    let entries = ctx.frame_video_entries.clone();
                    let obs_sink = ctx.obs_sink.clone();
                    tokio::spawn(async move {
                        use livekit::StreamReader;
                        match reader.read_all().await {
                            // `Bytes::from(Vec)` is a move. Subsequent
                            // `Bytes::slice(...)` in the dispatch path is a
                            // refcount bump, so the `Raw` codec gets a
                            // zero-copy view of the wire payload all the
                            // way to `VideoFrameData.data`.
                            Ok(payload) => dispatch_frame_payload(
                                Bytes::from(payload),
                                &entries,
                                &sync_buffer,
                                &obs_sink,
                            ),
                            Err(e) => log::warn!(
                                "failed to read frame_video byte stream: {e}"
                            ),
                        }
                    });
                }
                _ => {}
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
            for slot in &ctx.chunk_slots {
                slot.clear();
            }
            for slots in ctx.video_tracks.values() {
                slots.clear();
            }
            *ctx.peer_identity.lock() = None;
        }
        _ => {}
    }
}
