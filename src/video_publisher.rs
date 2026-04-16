use std::time::{SystemTime, UNIX_EPOCH};

use livekit::options::{PacketTrailerFeatures, TrackPublishOptions, VideoCodec};
use livekit::prelude::*;
use livekit::webrtc::prelude::{
    I420Buffer, RtcVideoSource, VideoBuffer, VideoFrame, VideoResolution, VideoRotation,
};
use livekit::webrtc::video_frame::FrameMetadata;
use livekit::webrtc::video_source::native::NativeVideoSource;

use crate::error::{PortalError, PortalResult};

const DEFAULT_WIDTH: u32 = 640;
const DEFAULT_HEIGHT: u32 = 480;

pub(crate) struct VideoPublisher {
    name: String,
    source: NativeVideoSource,
    track: LocalVideoTrack,
}

impl VideoPublisher {
    pub fn new(name: &str) -> Self {
        let resolution = VideoResolution {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        };
        let source = NativeVideoSource::new(resolution, false);
        let rtc_source = RtcVideoSource::Native(source.clone());
        let track = LocalVideoTrack::create_video_track(name, rtc_source);
        Self {
            name: name.to_string(),
            source,
            track,
        }
    }

    pub async fn publish(&self, local_participant: &LocalParticipant) -> PortalResult<()> {
        let mut packet_trailer_features = PacketTrailerFeatures::default();
        packet_trailer_features.user_timestamp = true;

        let options = TrackPublishOptions {
            video_codec: VideoCodec::H264,
            simulcast: false,
            packet_trailer_features,
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
        frame.frame_metadata = Some(FrameMetadata {
            user_timestamp: Some(ts),
            frame_id: None,
        });

        self.source.capture_frame(&frame);
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}
