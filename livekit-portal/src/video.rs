use std::mem::MaybeUninit;
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
use crate::metrics::TrackMetrics;
use crate::portal::ObservationSink;
use crate::sync_buffer::SyncBuffer;
use crate::types::VideoFrameData;

const DEFAULT_WIDTH: u32 = 640;
const DEFAULT_HEIGHT: u32 = 480;

// --- Publisher ---

pub(crate) struct VideoPublisher {
    source: NativeVideoSource,
    track: LocalVideoTrack,
    metrics: Arc<TrackMetrics>,
}

impl VideoPublisher {
    pub fn new(name: &str, metrics: Arc<TrackMetrics>) -> Self {
        let resolution = VideoResolution { width: DEFAULT_WIDTH, height: DEFAULT_HEIGHT };
        let source = NativeVideoSource::new(resolution, false);
        let rtc_source = RtcVideoSource::Native(source.clone());
        let track = LocalVideoTrack::create_video_track(name, rtc_source);
        Self { source, track, metrics }
    }

    pub async fn publish(&self, local_participant: &LocalParticipant) -> PortalResult<()> {
        // user_timestamp is mandatory: the receive path uses it to align frames
        // with state, and panics if it is missing. Subscribed tracks produced
        // by publishers that don't set this trailer are unsupported.
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
        rgb_data: &[u8],
        width: u32,
        height: u32,
        timestamp_us: Option<u64>,
    ) -> PortalResult<()> {
        // I420 packs U and V at half resolution in each axis. Odd dimensions
        // would silently desynchronize plane sizes (width/2 truncates), so
        // reject up front rather than copy garbage into the chroma planes.
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(PortalError::InvalidFrameDimensions { width, height });
        }
        let expected_size = (width * height * 3) as usize;
        if rgb_data.len() != expected_size {
            return Err(PortalError::WrongFrameSize {
                expected: expected_size,
                got: rgb_data.len(),
            });
        }
        let ts = timestamp_us.unwrap_or_else(now_us);
        let mut buffer = I420Buffer::new(width, height);
        rgb_to_i420(rgb_data, width as usize, height as usize, &mut buffer);
        let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
        frame.frame_metadata = Some(FrameMetadata { user_timestamp: Some(ts), frame_id: None });
        self.source.capture_frame(&frame);
        self.metrics.record_sent();
        Ok(())
    }
}

// --- Receiver ---

pub(crate) type VideoCb = Box<dyn Fn(&str, &VideoFrameData) + Send + Sync>;

/// Push callback + latest-wins slot for a single video track, paired so the
/// receiver task and `get_video_frame` share one allocation.
pub(crate) struct VideoTrackSlots {
    pub cb: Mutex<Option<VideoCb>>,
    pub latest: Mutex<Option<VideoFrameData>>,
}

impl VideoTrackSlots {
    pub fn new() -> Self {
        Self { cb: Mutex::new(None), latest: Mutex::new(None) }
    }

    pub fn clear(&self) {
        *self.latest.lock() = None;
    }
}

pub(crate) struct VideoReceiver {
    task_handle: JoinHandle<()>,
}

impl VideoReceiver {
    pub fn spawn(
        name: String,
        stream: NativeVideoStream,
        sync_buffer: Arc<Mutex<SyncBuffer>>,
        slots: Arc<VideoTrackSlots>,
        obs_sink: Arc<ObservationSink>,
        metrics: Arc<TrackMetrics>,
    ) -> Self {
        let handle = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(frame) = stream.next().await {
                // Hard requirement: every frame must carry a user_timestamp.
                // Portal-published tracks set this automatically; subscribed
                // tracks from other publishers must do the same. See the
                // "Sender requirement" note in README.md.
                let timestamp_us = frame
                    .frame_metadata
                    .and_then(|m| m.user_timestamp)
                    .expect(
                        "video frame missing user_timestamp — \
                         sender must enable PacketTrailerFeatures.user_timestamp",
                    );
                let frame_data = convert_frame(&frame, timestamp_us);
                let frame_arc = Arc::new(frame_data);

                metrics.record_received(timestamp_us, now_us());

                if let Some(cb) = slots.cb.lock().as_ref() {
                    cb(&name, &frame_arc);
                }
                // VideoFrameData clone is cheap — pixel buffer is Arc<[u8]>.
                *slots.latest.lock() = Some((*frame_arc).clone());
                let output = sync_buffer.lock().push_frame(&name, frame_arc);
                if !output.is_empty() {
                    obs_sink.dispatch(output);
                }
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
    let total = y.len() + u.len() + v.len();

    // Build the Arc payload in a single allocation/copy. The naive
    // `Vec::extend_from_slice * 3` followed by `Arc::from(vec)` allocates
    // twice and copies twice — at 640x480 that's ~460KB doubled per frame.
    let mut data: Arc<[MaybeUninit<u8>]> = Arc::new_uninit_slice(total);
    {
        // SAFETY: freshly allocated Arc, no other references exist.
        let dst = Arc::get_mut(&mut data).expect("freshly allocated Arc has unique ownership");
        // SAFETY: dst is exactly `total` bytes, sources are independent of
        // dst, and MaybeUninit<u8> shares u8's layout.
        unsafe {
            let p = dst.as_mut_ptr() as *mut u8;
            std::ptr::copy_nonoverlapping(y.as_ptr(), p, y.len());
            std::ptr::copy_nonoverlapping(u.as_ptr(), p.add(y.len()), u.len());
            std::ptr::copy_nonoverlapping(v.as_ptr(), p.add(y.len() + u.len()), v.len());
        }
    }
    // SAFETY: every byte was initialized by the three copies above.
    let data: Arc<[u8]> = unsafe { data.assume_init() };

    VideoFrameData { width: i420.width(), height: i420.height(), data, timestamp_us }
}

// BT.601 limited-range RGB24 -> I420, matching libyuv's RAWToI420. Chroma is
// 2x2-box-averaged so hard edges don't leak into adjacent blocks.
fn rgb_to_i420(src: &[u8], width: usize, height: usize, buffer: &mut I420Buffer) {
    let stride_y = buffer.stride_y() as usize;
    let stride_u = buffer.stride_u() as usize;
    let stride_v = buffer.stride_v() as usize;
    let (y_dst, u_dst, v_dst) = buffer.data_mut();
    let src_stride = width * 3;

    for y in 0..height {
        let src_row = &src[y * src_stride..y * src_stride + src_stride];
        let y_row = &mut y_dst[y * stride_y..y * stride_y + width];
        for x in 0..width {
            let r = src_row[x * 3] as i32;
            let g = src_row[x * 3 + 1] as i32;
            let b = src_row[x * 3 + 2] as i32;
            y_row[x] = clamp_u8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
        }
    }

    let cw = width / 2;
    let ch = height / 2;
    for cy in 0..ch {
        let row0 = &src[(2 * cy) * src_stride..(2 * cy) * src_stride + src_stride];
        let row1 = &src[(2 * cy + 1) * src_stride..(2 * cy + 1) * src_stride + src_stride];
        let u_row = &mut u_dst[cy * stride_u..cy * stride_u + cw];
        let v_row = &mut v_dst[cy * stride_v..cy * stride_v + cw];
        for cx in 0..cw {
            let i0 = 2 * cx * 3;
            let i1 = i0 + 3;
            let r = (row0[i0] as i32 + row0[i1] as i32 + row1[i0] as i32 + row1[i1] as i32 + 2) >> 2;
            let g = (row0[i0 + 1] as i32 + row0[i1 + 1] as i32 + row1[i0 + 1] as i32 + row1[i1 + 1] as i32 + 2) >> 2;
            let b = (row0[i0 + 2] as i32 + row0[i1 + 2] as i32 + row1[i0 + 2] as i32 + row1[i1 + 2] as i32 + 2) >> 2;
            u_row[cx] = clamp_u8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
            v_row[cx] = clamp_u8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
        }
    }
}

fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

pub(crate) fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64
}
