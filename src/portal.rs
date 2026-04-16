use std::collections::HashMap;
use std::sync::Arc;

use livekit::data_track::{DataTrack, Local};
use livekit::prelude::*;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::config::PortalConfig;
use crate::data_publisher::DataPublisher;
use crate::data_receiver::DataReceiver;
use crate::error::{PortalError, PortalResult};
use crate::sync_buffer::SyncBuffer;
use crate::types::*;
use crate::video_publisher::VideoPublisher;
use crate::video_receiver::VideoReceiver;

type ActionCb = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;
type ObservationCb = Box<dyn Fn(Observation) + Send + Sync>;
type StateCb = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;
type VideoCb = Box<dyn Fn(&str, &VideoFrameData) + Send + Sync>;
type DropCb = Box<dyn Fn(Vec<HashMap<String, f64>>) + Send + Sync>;

struct PortalInner {
    config: PortalConfig,
    room: Option<Room>,

    // Robot side
    video_publishers: HashMap<String, VideoPublisher>,
    state_publisher: Option<DataPublisher>,
    action_receiver: Option<DataReceiver>,

    // Operator side
    video_receivers: HashMap<String, VideoReceiver>,
    state_receiver: Option<DataReceiver>,
    action_publisher: Option<DataPublisher>,
    sync_buffer: Option<Arc<Mutex<SyncBuffer>>>,

    // Callbacks
    action_cb: Arc<Mutex<Option<ActionCb>>>,
    observation_cb: Arc<Mutex<Option<ObservationCb>>>,
    state_cb: Arc<Mutex<Option<StateCb>>>,
    video_cbs: HashMap<String, Arc<Mutex<Option<VideoCb>>>>,
    drop_cb: Arc<Mutex<Option<DropCb>>>,

    event_task: Option<JoinHandle<()>>,
}

pub struct Portal {
    inner: Arc<Mutex<PortalInner>>,
}

impl Portal {
    pub fn new(config: PortalConfig) -> Self {
        let video_cbs: HashMap<_, _> = config
            .video_tracks
            .iter()
            .map(|name| (name.clone(), Arc::new(Mutex::new(None))))
            .collect();

        Self {
            inner: Arc::new(Mutex::new(PortalInner {
                config,
                room: None,
                video_publishers: HashMap::new(),
                state_publisher: None,
                action_receiver: None,
                video_receivers: HashMap::new(),
                state_receiver: None,
                action_publisher: None,
                sync_buffer: None,
                action_cb: Arc::new(Mutex::new(None)),
                observation_cb: Arc::new(Mutex::new(None)),
                state_cb: Arc::new(Mutex::new(None)),
                video_cbs,
                drop_cb: Arc::new(Mutex::new(None)),
                event_task: None,
            })),
        }
    }

    pub async fn connect(&self, url: &str, token: &str) -> PortalResult<()> {
        let config = {
            let inner = self.inner.lock();
            if inner.room.is_some() {
                return Err(PortalError::AlreadyConnected);
            }
            inner.config.clone()
        };

        let mut options = RoomOptions::default();
        options.auto_subscribe = true;

        let (room, mut events) =
            Room::connect(url, token, options)
                .await
                .map_err(|e| PortalError::Room(e.to_string()))?;

        match config.role {
            Role::Robot => self.setup_robot(&room, &config).await?,
            Role::Operator => self.setup_operator(&room, &config).await?,
        }

        // Spawn event handler for dynamic track subscription
        let inner_ref = self.inner.clone();
        let config_clone = config.clone();
        let event_handle = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                handle_room_event(&inner_ref, &config_clone, event).await;
            }
        });

        {
            let mut inner = self.inner.lock();
            inner.room = Some(room);
            inner.event_task = Some(event_handle);
        }

        Ok(())
    }

    async fn setup_robot(&self, room: &Room, config: &PortalConfig) -> PortalResult<()> {
        let lp = room.local_participant();

        // Publish video tracks
        for track_name in &config.video_tracks {
            let publisher = VideoPublisher::new(track_name);
            publisher.publish(&lp).await?;
            self.inner
                .lock()
                .video_publishers
                .insert(track_name.clone(), publisher);
        }

        // Publish state data track
        if !config.state_fields.is_empty() {
            let track: DataTrack<Local> = lp
                .publish_data_track("portal_state")
                .await
                .map_err(|e| PortalError::DataTrack(e.to_string()))?;
            self.inner.lock().state_publisher =
                Some(DataPublisher::new(config.state_fields.clone(), track));
        }

        // Action subscription handled via room events (TrackSubscribed)
        Ok(())
    }

    async fn setup_operator(&self, room: &Room, config: &PortalConfig) -> PortalResult<()> {
        let lp = room.local_participant();

        // Create sync buffer
        let sync_buffer = Arc::new(Mutex::new(SyncBuffer::new(
            &config.video_tracks,
            config.state_fields.clone(),
            config.sync_config.clone(),
        )));

        // Wire callbacks into sync buffer
        {
            let obs_cb = self.inner.lock().observation_cb.clone();
            let drop_cb = self.inner.lock().drop_cb.clone();
            let mut sb = sync_buffer.lock();
            sb.set_observation_callback(Box::new(move |obs| {
                if let Some(cb) = obs_cb.lock().as_ref() {
                    cb(obs);
                }
            }));
            sb.set_drop_callback(Box::new(move |dropped| {
                if let Some(cb) = drop_cb.lock().as_ref() {
                    cb(dropped);
                }
            }));
        }

        self.inner.lock().sync_buffer = Some(sync_buffer);

        // Publish action data track
        if !config.action_fields.is_empty() {
            let track: DataTrack<Local> = lp
                .publish_data_track("portal_action")
                .await
                .map_err(|e| PortalError::DataTrack(e.to_string()))?;
            self.inner.lock().action_publisher =
                Some(DataPublisher::new(config.action_fields.clone(), track));
        }

        // Video and state subscription handled via room events (TrackSubscribed)
        Ok(())
    }

    // --- Public API: Send ---

    pub fn send_video_frame(
        &self,
        track_name: &str,
        i420_data: &[u8],
        width: u32,
        height: u32,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let inner = self.inner.lock();
        let publisher = inner.video_publishers.get(track_name).ok_or_else(|| {
            PortalError::UnknownVideoTrack {
                name: track_name.to_string(),
            }
        })?;
        publisher.send_frame(i420_data, width, height, timestamp_us)
    }

    pub fn send_state(
        &self,
        values: &HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let inner = self.inner.lock();
        let publisher = inner
            .state_publisher
            .as_ref()
            .ok_or(PortalError::WrongRole(Role::Operator))?;
        publisher.send_map(values, timestamp_us)
    }

    pub fn send_action(
        &self,
        values: &HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let inner = self.inner.lock();
        let publisher = inner
            .action_publisher
            .as_ref()
            .ok_or(PortalError::WrongRole(Role::Robot))?;
        publisher.send_map(values, timestamp_us)
    }

    // --- Public API: Callbacks ---

    pub fn on_action(&self, callback: impl Fn(HashMap<String, f64>) + Send + Sync + 'static) {
        *self.inner.lock().action_cb.lock() = Some(Box::new(callback));
    }

    pub fn on_observation(&self, callback: impl Fn(Observation) + Send + Sync + 'static) {
        *self.inner.lock().observation_cb.lock() = Some(Box::new(callback));
    }

    pub fn on_state(&self, callback: impl Fn(HashMap<String, f64>) + Send + Sync + 'static) {
        *self.inner.lock().state_cb.lock() = Some(Box::new(callback));
    }

    pub fn on_video(
        &self,
        track_name: &str,
        callback: impl Fn(&str, &VideoFrameData) + Send + Sync + 'static,
    ) {
        if let Some(cb_slot) = self.inner.lock().video_cbs.get(track_name) {
            *cb_slot.lock() = Some(Box::new(callback));
        }
    }

    pub fn on_drop(
        &self,
        callback: impl Fn(Vec<HashMap<String, f64>>) + Send + Sync + 'static,
    ) {
        *self.inner.lock().drop_cb.lock() = Some(Box::new(callback));
    }

    pub async fn disconnect(&self) -> PortalResult<()> {
        let mut inner = self.inner.lock();
        if let Some(room) = inner.room.take() {
            room.close()
                .await
                .map_err(|e| PortalError::Room(e.to_string()))?;
        }
        if let Some(task) = inner.event_task.take() {
            task.abort();
        }
        for receiver in inner.video_receivers.values() {
            receiver.abort();
        }
        if let Some(receiver) = &inner.action_receiver {
            receiver.abort();
        }
        if let Some(receiver) = &inner.state_receiver {
            receiver.abort();
        }
        if let Some(sb) = &inner.sync_buffer {
            sb.lock().clear();
        }
        inner.video_publishers.clear();
        inner.video_receivers.clear();
        inner.state_publisher = None;
        inner.state_receiver = None;
        inner.action_publisher = None;
        inner.action_receiver = None;
        Ok(())
    }
}

async fn handle_room_event(
    inner_ref: &Arc<Mutex<PortalInner>>,
    config: &PortalConfig,
    event: RoomEvent,
) {
    match event {
        RoomEvent::TrackSubscribed {
            track, publication, ..
        } => {
            let track_name = publication.name();
            match config.role {
                Role::Robot => {
                    // Robot subscribes to action data track
                    if track_name == "portal_action" {
                        if let RemoteTrack::Audio(_) = track {
                            // ignore audio
                        } else {
                            subscribe_action_track(inner_ref, config).await;
                        }
                    }
                }
                Role::Operator => {
                    match track {
                        RemoteTrack::Video(video_track) => {
                            // Match against registered video track names
                            if config.video_tracks.contains(&track_name.to_string()) {
                                let inner = inner_ref.lock();
                                if let Some(sync_buffer) = &inner.sync_buffer {
                                    let raw_cb = inner
                                        .video_cbs
                                        .get(track_name.as_str())
                                        .cloned()
                                        .unwrap_or_else(|| Arc::new(Mutex::new(None)));

                                    let stream =
                                        NativeVideoStream::new(video_track.rtc_track());
                                    let receiver = VideoReceiver::spawn(
                                        track_name.to_string(),
                                        stream,
                                        sync_buffer.clone(),
                                        raw_cb,
                                    );
                                    drop(inner);
                                    inner_ref
                                        .lock()
                                        .video_receivers
                                        .insert(track_name.to_string(), receiver);
                                }
                            }
                        }
                        RemoteTrack::Audio(_) => {}
                    }
                    // Operator subscribes to state data track
                    if track_name == "portal_state" {
                        subscribe_state_track(inner_ref, config).await;
                    }
                }
            }
        }
        RoomEvent::Reconnected => {
            let inner = inner_ref.lock();
            if let Some(sb) = &inner.sync_buffer {
                sb.lock().clear();
            }
        }
        _ => {}
    }
}

async fn subscribe_action_track(inner_ref: &Arc<Mutex<PortalInner>>, config: &PortalConfig) {
    let inner = inner_ref.lock();
    let room = match &inner.room {
        Some(r) => r,
        None => return,
    };

    // Find the remote data track named "portal_action" and subscribe
    for (_, participant) in room.remote_participants() {
        for (_, publication) in participant.track_publications() {
            if publication.name() == "portal_action" {
                if let Some(track) = publication.track() {
                    if let RemoteTrack::Audio(_) = track {
                        continue;
                    }
                    // Subscribe to the data track to get a stream
                    // The data track subscription is handled through the data track API
                    // For now, we need to get the DataTrack<Remote> and subscribe
                    // This will be wired when we can access the data track from the publication
                }
            }
        }
    }

    // TODO: Wire data track subscription once the API is clarified
    // For now, action data track subscription needs the DataTrack<Remote>::subscribe() API
    let _ = (inner_ref, config);
}

async fn subscribe_state_track(inner_ref: &Arc<Mutex<PortalInner>>, config: &PortalConfig) {
    // TODO: Wire data track subscription once the API is clarified
    // Similar to subscribe_action_track but feeds into SyncBuffer
    let _ = (inner_ref, config);
}
