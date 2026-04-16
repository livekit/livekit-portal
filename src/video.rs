use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use livekit::options::{PacketTrailerFeatures, TrackPublishOptions, VideoCodec};
use livekit::prelude::*;
use livekit::webrtc::prelude::{
    I420Buffer, RtcVideoSource, VideoBuffer, VideoFrame, VideoResolution, VideoRotation,
};
use livekit::webrtc::video_frame::FrameMetadata;
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::error::{PortalError, PortalResult};
use crate::sync_buffer::SyncBuffer;
use crate::types::VideoFrameData;

const DEFAULT_WIDTH: u32 = 640;
const DEFAULT_HEIGHT: u32 = 480;

// --- Publisher ---

pub(crate) struct VideoPublisher {
    source: NativeVideoSource,
    track: LocalVideoTrack,
}

impl VideoPublisher {
    pub fn new(name: &str) -> Self {
        let resolution = VideoResolution { width: DEFAULT_WIDTH, height: DEFAULT_HEIGHT };
        let source = NativeVideoSource::new(resolution, false);
        let rtc_source = RtcVideoSource::Native(source.clone());
        let track = LocalVideoTrack::create_video_track(name, rtc_source);
        Self { source, track }
    }

    pub async fn publish(&self, local_participant: &LocalParticipant) -> PortalResult<()> {
        let mut features = PacketTrailerFeatures::default();
        features.user_timestamp = true;
        let options = TrackPublishOptions {
            video_codec: VideoCodec::H264,
            simulcast: false,
            packet_trailer_features: features,
            ..Default::default()
        };
        local_participant
            .publish_track(LocalTrack::Video(self.track.clone()), options)
            .await
            .map_err(|e| PortalError::Room(e.to_string()))?;
        Ok(())
    }

    pub fn send_frame(
        &self,
        i420_data: &[u8],
        width: u32,
        height: u32,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        let expected_size = (width * height * 3 / 2) as usize;
        if i420_data.len() != expected_size {
            return Err(PortalError::WrongValueCount {
                expected: expected_size,
                got: i420_data.len(),
            });
        }
        let ts = timestamp_us.unwrap_or_else(now_us);
        let mut buffer = I420Buffer::new(width, height);
        copy_i420_data(i420_data, &mut buffer);
        let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
        frame.frame_metadata = Some(FrameMetadata { user_timestamp: Some(ts), frame_id: None });
        self.source.capture_frame(&frame);
        Ok(())
    }
}

// --- Receiver ---

type VideoCb = Box<dyn Fn(&str, &VideoFrameData) + Send + Sync>;

pub(crate) struct VideoReceiver {
    task_handle: JoinHandle<()>,
}

impl VideoReceiver {
    pub fn spawn(
        name: String,
        stream: NativeVideoStream,
        sync_buffer: Arc<Mutex<SyncBuffer>>,
        raw_callback: Arc<Mutex<Option<VideoCb>>>,
    ) -> Self {
        let handle = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(frame) = stream.next().await {
                let timestamp_us = frame.frame_metadata.and_then(|m| m.user_timestamp).unwrap_or(0);
                let frame_data = convert_frame(&frame, timestamp_us);
                let frame_arc = Arc::new(frame_data);

                if let Some(cb) = raw_callback.lock().as_ref() {
                    cb(&name, &frame_arc);
                }
                sync_buffer.lock().push_frame(&name, frame_arc);
            }
        });
        Self { task_handle: handle }
    }

    pub fn abort(&self) {
        self.task_handle.abort();
    }
}

// --- Helpers ---

fn convert_frame<T: AsRef<dyn VideoBuffer>>(
    frame: &VideoFrame<T>,
    timestamp_us: u64,
) -> VideoFrameData {
    let i420 = frame.buffer.as_ref().to_i420();
    let (y, u, v) = i420.data();
    let mut data = Vec::with_capacity(y.len() + u.len() + v.len());
    data.extend_from_slice(y);
    data.extend_from_slice(u);
    data.extend_from_slice(v);
    VideoFrameData { width: i420.width(), height: i420.height(), data, timestamp_us }
}

fn copy_i420_data(src: &[u8], buffer: &mut I420Buffer) {
    let width = buffer.width() as usize;
    let height = buffer.height() as usize;
    let y_size = width * height;
    let uv_size = (width / 2) * (height / 2);
    let (y_dst, u_dst, v_dst) = buffer.data_mut();
    y_dst[..y_size].copy_from_slice(&src[..y_size]);
    u_dst[..uv_size].copy_from_slice(&src[y_size..y_size + uv_size]);
    v_dst[..uv_size].copy_from_slice(&src[y_size + uv_size..y_size + 2 * uv_size]);
}

pub(crate) fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64
}
