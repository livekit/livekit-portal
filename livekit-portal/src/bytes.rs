//! Byte-stream passthrough: a thin layer over the LiveKit SDK's
//! `LocalParticipant::send_bytes` / `ByteStreamOpened` room event.
//!
//! Users register a handler per topic; on an incoming `ByteStreamOpened`
//! whose topic matches a registered handler, Portal reads the full payload
//! (`reader.read_all().await`) and fires the handler with the sender
//! identity plus the assembled bytes. Outbound sends go through the SDK's
//! one-shot `send_bytes`.
//!
//! Handlers are kept in a `Portal`-owned map so they survive disconnect and
//! get re-dispatched from a fresh room event loop after reconnect.

use std::collections::HashMap;
use std::sync::Arc;

use livekit::prelude::*;
use livekit::data_stream::StreamByteOptions;
use parking_lot::Mutex;

use crate::error::{PortalError, PortalResult};

/// Foreign-facing byte-stream handler. Receives the sender's identity and
/// the assembled payload for a completed incoming stream on its topic.
pub type ByteStreamHandler = Arc<dyn Fn(String, Vec<u8>) + Send + Sync + 'static>;

/// Topic-indexed registry of byte-stream handlers. Cloned cheaply via `Arc`
/// for the Portal event loop.
#[derive(Default)]
pub(crate) struct ByteStreamRegistry {
    handlers: Mutex<HashMap<String, ByteStreamHandler>>,
}

impl ByteStreamRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn insert(&self, topic: &str, handler: ByteStreamHandler) {
        self.handlers.lock().insert(topic.to_string(), handler);
    }

    pub fn remove(&self, topic: &str) {
        self.handlers.lock().remove(topic);
    }

    pub fn get(&self, topic: &str) -> Option<ByteStreamHandler> {
        self.handlers.lock().get(topic).cloned()
    }
}

/// Send a one-shot byte payload on `topic`. When `destination` is `Some`,
/// the send targets that participant identity only; when `None`, the stream
/// is broadcast to the room (matching the SDK default).
pub(crate) async fn send_bytes(
    lp: &LocalParticipant,
    topic: &str,
    data: &[u8],
    destination: Option<&str>,
) -> PortalResult<()> {
    let mut options = StreamByteOptions::default();
    options.topic = topic.to_string();
    if let Some(id) = destination {
        options.destination_identities = vec![ParticipantIdentity(id.to_string())];
    }
    lp.send_bytes(data, options)
        .await
        .map(|_info| ())
        .map_err(|e| PortalError::Room(format!("send_bytes: {e}")))
}

/// Handle a `ByteStreamOpened` room event. If a handler is registered for
/// the topic, spawn a task that drains the reader and hands the full
/// payload to the handler. Non-matching topics are ignored so multiple
/// concurrent subsystems on the room (file transfer, chat, etc.) don't
/// fight for the same reader.
pub(crate) fn dispatch_byte_stream_opened(
    registry: &ByteStreamRegistry,
    reader: livekit::data_stream::ByteStreamReader,
    topic: String,
    participant_identity: ParticipantIdentity,
) {
    let Some(handler) = registry.get(&topic) else {
        return;
    };
    let identity = participant_identity.as_str().to_string();
    tokio::spawn(async move {
        match read_all(reader).await {
            Ok(bytes) => handler(identity, bytes),
            Err(e) => log::warn!("byte stream on topic '{topic}' failed to read: {e}"),
        }
    });
}

async fn read_all(
    reader: livekit::data_stream::ByteStreamReader,
) -> Result<Vec<u8>, livekit::data_stream::StreamError> {
    use livekit::data_stream::StreamReader;
    let bytes = reader.read_all().await?;
    Ok(bytes.to_vec())
}
