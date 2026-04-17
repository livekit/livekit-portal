use std::collections::HashMap;
use std::sync::Arc;

use livekit_portal::{Observation, Portal, VideoFrameData};

use super::{FfiHandle, FfiHandleId};
use crate::proto;
use crate::FFI_SERVER;

/// Handle-registry wrapper around a fully-constructed `livekit_portal::Portal`.
/// The declared field orders are captured at construction so event-emitting
/// closures (installed below) can serialize maps into the protobuf shape
/// without locking back through the config.
#[derive(Clone)]
pub struct FfiPortal {
    pub handle: FfiHandleId,
    pub inner: Arc<Portal>,
    pub state_fields: Arc<Vec<String>>,
    pub action_fields: Arc<Vec<String>>,
    pub video_tracks: Arc<Vec<String>>,
}

impl FfiHandle for FfiPortal {}

impl FfiPortal {
    /// Constructs the wrapper and wires core push callbacks to emit protobuf
    /// events via `FFI_SERVER.send_event`. Always-on emission. Python filters.
    pub fn new(
        handle: FfiHandleId,
        inner: Portal,
        video_tracks: Vec<String>,
        state_fields: Vec<String>,
        action_fields: Vec<String>,
    ) -> Self {
        let inner = Arc::new(inner);
        let state_fields = Arc::new(state_fields);
        let action_fields = Arc::new(action_fields);
        let video_tracks = Arc::new(video_tracks);

        // on_action: Robot-side. Emit the received action values, ordered by
        // declared action_fields.
        {
            let fields = action_fields.clone();
            inner.on_action(move |map| {
                FFI_SERVER.send_event(proto::ffi_event::Message::Action(
                    proto::ActionEvent { portal_handle: handle, values: clone_map(map, &fields) },
                ));
            });
        }

        // on_state: Operator-side. Emit the raw received state (unsynced).
        {
            let fields = state_fields.clone();
            inner.on_state(move |map| {
                FFI_SERVER.send_event(proto::ffi_event::Message::State(
                    proto::StateEvent { portal_handle: handle, values: clone_map(map, &fields) },
                ));
            });
        }

        // on_observation: Operator-side. Emit the synced observation.
        inner.on_observation(move |obs: &Observation| {
            FFI_SERVER.send_event(proto::ffi_event::Message::Observation(
                proto::ObservationEvent {
                    portal_handle: handle,
                    observation: Some(obs.clone().into()),
                },
            ));
        });

        // on_drop: Operator-side. Emit the list of dropped state maps.
        {
            let fields = state_fields.clone();
            inner.on_drop(move |drops: Vec<HashMap<String, f64>>| {
                let dropped = drops
                    .into_iter()
                    .map(|m| proto::DroppedState { values: reorder_map(m, &fields) })
                    .collect();
                FFI_SERVER.send_event(proto::ffi_event::Message::Drop(proto::DropEvent {
                    portal_handle: handle,
                    dropped,
                }));
            });
        }

        // on_video_frame: Operator-side. One registration per declared track.
        for name in video_tracks.iter() {
            let track_name = name.clone();
            inner.on_video_frame(&track_name, move |track: &str, frame: &VideoFrameData| {
                FFI_SERVER.send_event(proto::ffi_event::Message::VideoFrame(
                    proto::VideoFrameEvent {
                        portal_handle: handle,
                        track_name: track.to_string(),
                        frame: Some(frame.clone().into()),
                    },
                ));
            });
        }

        Self { handle, inner, state_fields, action_fields, video_tracks }
    }
}

/// Clone a field→value map into a new map, in arbitrary order. We re-emit the
/// map as-is for transport. the field list is carried on the declaration side.
fn clone_map(map: &HashMap<String, f64>, _fields: &[String]) -> HashMap<String, f64> {
    map.clone()
}

/// Move a map into a new map (same layout). Kept as a separate helper to match
/// the Drop callback's ownership shape.
fn reorder_map(map: HashMap<String, f64>, _fields: &[String]) -> HashMap<String, f64> {
    map
}
