use std::collections::HashMap;
use std::sync::Arc;

use livekit::data_track::{DataTrack, Local};
use livekit::prelude::*;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::config::{PortalConfig, PortalConfigData};
use crate::data_publisher::DataPublisher;
use crate::data_receiver::DataReceiver;
use crate::error::{PortalError, PortalResult};
use crate::sync_buffer::SyncBuffer;
use crate::types::*;
use crate::video_publisher::VideoPublisher;
use crate::video_receiver::VideoReceiver;

// --- Internal callback types ---

type ActionCb = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;
type ObservationCb = Box<dyn Fn(Observation) + Send + Sync>;
type StateCb = Box<dyn Fn(HashMap<String, f64>) + Send + Sync>;
type VideoCb = Box<dyn Fn(&str, &VideoFrameData) + Send + Sync>;
type DropCb = Box<dyn Fn(Vec<HashMap<String, f64>>) + Send + Sync>;

struct PortalInner {
    config: PortalConfigData,
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

#[derive(uniffi::Object)]
pub struct Portal {
    inner: Arc<Mutex<PortalInner>>,
}

// --- UniFFI-exported methods ---

#[uniffi::export]
impl Portal {
    #[uniffi::constructor]
    pub fn new(config: Arc<PortalConfig>) -> Arc<Self> {
        let data = config.snapshot();
        let video_cbs: HashMap<_, _> = data
            .video_tracks
            .iter()
            .map(|name| (name.clone(), Arc::new(Mutex::new(None))))
            .collect();

        Arc::new(Self {
            inner: Arc::new(Mutex::new(PortalInner {
                config: data,
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
        })
    }

    pub async fn connect(&self, url: String, token: String) -> Result<(), PortalError> {
        let config = {
            let inner = self.inner.lock();
            if inner.room.is_some() {
                return Err(PortalError::AlreadyConnected);
            }
            inner.config.clone()
        };

        let mut options = RoomOptions::default();
        options.auto_subscribe = true;

        let (room, events) = Room::connect(&url, &token, options)
            .await
            .map_err(|e| PortalError::Room(e.to_string()))?;

        match config.role {
            Role::Robot => self.setup_robot(&room, &config).await?,
            Role::Operator => self.setup_operator(&room, &config).await?,
        }

        let inner_ref = self.inner.clone();
        let config_clone = config.clone();
        let event_handle = tokio::spawn(async move {
            let mut events = events;
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

    pub fn send_video_frame(
        &self,
        track_name: String,
        i420_data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp_us: Option<u64>,
    ) -> Result<(), PortalError> {
        let inner = self.inner.lock();
        let publisher = inner
            .video_publishers
            .get(&track_name)
            .ok_or_else(|| PortalError::UnknownVideoTrack { name: track_name.clone() })?;
        publisher.send_frame(&i420_data, width, height, timestamp_us)
    }

    pub fn send_state(
        &self,
        values: HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> Result<(), PortalError> {
        let inner = self.inner.lock();
        let publisher =
            inner.state_publisher.as_ref().ok_or(PortalError::WrongRole(Role::Operator))?;
        publisher.send_map(&values, timestamp_us)
    }

    pub fn send_action(
        &self,
        values: HashMap<String, f64>,
        timestamp_us: Option<u64>,
    ) -> Result<(), PortalError> {
        let inner = self.inner.lock();
        let publisher =
            inner.action_publisher.as_ref().ok_or(PortalError::WrongRole(Role::Robot))?;
        publisher.send_map(&values, timestamp_us)
    }

    pub async fn disconnect(&self) -> Result<(), PortalError> {
        let room = self.inner.lock().room.take();
        if let Some(room) = room {
            room.close().await.map_err(|e| PortalError::Room(e.to_string()))?;
        }

        let mut inner = self.inner.lock();
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

// --- Rust-only callback registration (closures) ---

impl Portal {
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

    pub fn on_drop(&self, callback: impl Fn(Vec<HashMap<String, f64>>) + Send + Sync + 'static) {
        *self.inner.lock().drop_cb.lock() = Some(Box::new(callback));
    }
}

// --- Internal helpers ---

impl Portal {
    async fn setup_robot(&self, room: &Room, config: &PortalConfigData) -> PortalResult<()> {
        let lp = room.local_participant();

        for track_name in &config.video_tracks {
            let publisher = VideoPublisher::new(track_name);
            publisher.publish(&lp).await?;
            self.inner.lock().video_publishers.insert(track_name.clone(), publisher);
        }

        if !config.state_fields.is_empty() {
            let track: DataTrack<Local> = lp
                .publish_data_track("portal_state")
                .await
                .map_err(|e| PortalError::DataTrack(e.to_string()))?;
            self.inner.lock().state_publisher =
                Some(DataPublisher::new(config.state_fields.clone(), track));
        }

        Ok(())
    }

    async fn setup_operator(&self, room: &Room, config: &PortalConfigData) -> PortalResult<()> {
        let lp = room.local_participant();

        let sync_buffer = Arc::new(Mutex::new(SyncBuffer::new(
            &config.video_tracks,
            config.state_fields.clone(),
            config.sync_config.clone(),
        )));

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

        if !config.action_fields.is_empty() {
            let track: DataTrack<Local> = lp
                .publish_data_track("portal_action")
                .await
                .map_err(|e| PortalError::DataTrack(e.to_string()))?;
            self.inner.lock().action_publisher =
                Some(DataPublisher::new(config.action_fields.clone(), track));
        }

        Ok(())
    }
}

async fn handle_room_event(
    inner_ref: &Arc<Mutex<PortalInner>>,
    config: &PortalConfigData,
    event: RoomEvent,
) {
    match event {
        // Video tracks arrive via TrackSubscribed (operator subscribes to robot's video)
        RoomEvent::TrackSubscribed { track, publication, .. } => {
            if config.role != Role::Operator {
                return;
            }
            if let RemoteTrack::Video(video_track) = track {
                let track_name = publication.name();
                if config.video_tracks.contains(&track_name.to_string()) {
                    let inner = inner_ref.lock();
                    if let Some(sync_buffer) = &inner.sync_buffer {
                        let raw_cb = inner
                            .video_cbs
                            .get(track_name.as_str())
                            .cloned()
                            .unwrap_or_else(|| Arc::new(Mutex::new(None)));

                        let stream = NativeVideoStream::new(video_track.rtc_track());
                        let receiver = VideoReceiver::spawn(
                            track_name.to_string(),
                            stream,
                            sync_buffer.clone(),
                            raw_cb,
                        );
                        drop(inner);
                        inner_ref.lock().video_receivers.insert(track_name.to_string(), receiver);
                    }
                }
            }
        }
        // Data tracks arrive via DataTrackPublished (state and action)
        RoomEvent::DataTrackPublished(remote_data_track) => {
            let track_name = remote_data_track.info().name().to_string();
            match (config.role, track_name.as_str()) {
                (Role::Robot, "portal_action") => {
                    subscribe_action_track(inner_ref, config, remote_data_track).await;
                }
                (Role::Operator, "portal_state") => {
                    subscribe_state_track(inner_ref, config, remote_data_track).await;
                }
                _ => {}
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

async fn subscribe_action_track(
    inner_ref: &Arc<Mutex<PortalInner>>,
    config: &PortalConfigData,
    track: RemoteDataTrack,
) {
    let stream = match track.subscribe().await {
        Ok(s) => s,
        Err(e) => {
            log::error!("failed to subscribe to action data track: {e}");
            return;
        }
    };

    let action_cb = inner_ref.lock().action_cb.clone();
    let receiver = DataReceiver::spawn_action(config.action_fields.clone(), stream, action_cb);
    inner_ref.lock().action_receiver = Some(receiver);
}

async fn subscribe_state_track(
    inner_ref: &Arc<Mutex<PortalInner>>,
    config: &PortalConfigData,
    track: RemoteDataTrack,
) {
    let stream = match track.subscribe().await {
        Ok(s) => s,
        Err(e) => {
            log::error!("failed to subscribe to state data track: {e}");
            return;
        }
    };

    let inner = inner_ref.lock();
    let sync_buffer = match &inner.sync_buffer {
        Some(sb) => sb.clone(),
        None => return,
    };
    let state_cb = inner.state_cb.clone();
    drop(inner);

    let receiver =
        DataReceiver::spawn_state(config.state_fields.clone(), stream, sync_buffer, state_cb);
    inner_ref.lock().state_receiver = Some(receiver);
}
