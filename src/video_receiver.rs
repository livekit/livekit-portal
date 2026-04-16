use std::sync::Arc;

use futures_util::StreamExt;
use livekit::webrtc::prelude::{VideoBuffer, VideoFrame};
use livekit::webrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::sync_buffer::SyncBuffer;
use crate::types::VideoFrameData;

type VideoCallback = Box<dyn Fn(&str, &VideoFrameData) + Send + Sync>;

pub(crate) struct VideoReceiver {
    _name: String,
    task_handle: JoinHandle<()>,
}

impl VideoReceiver {
    pub fn spawn(
        name: String,
        stream: NativeVideoStream,
        sync_buffer: Arc<Mutex<SyncBuffer>>,
        raw_callback: Arc<Mutex<Option<VideoCallback>>>,
    ) -> Self {
        let track_name = name.clone();
        let handle = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(frame) = stream.next().await {
                let timestamp_us = frame.frame_metadata.and_then(|m| m.user_timestamp).unwrap_or(0);

                let frame_data = convert_frame(&frame, timestamp_us);
                let frame_arc = Arc::new(frame_data);

                if let Some(cb) = raw_callback.lock().as_ref() {
                    cb(&track_name, &frame_arc);
                }

                sync_buffer.lock().push_frame(&track_name, frame_arc);
            }
        });
        Self { _name: name, task_handle: handle }
    }

    pub fn abort(&self) {
        self.task_handle.abort();
    }
}

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
