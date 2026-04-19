use std::collections::HashMap;

use livekit_portal_ffi::proto;
use prost::Message;

fn roundtrip<T: Message + Default + PartialEq + std::fmt::Debug>(msg: &T) {
    let bytes = msg.encode_to_vec();
    let decoded = T::decode(&*bytes).expect("decode round-trip");
    assert_eq!(*msg, decoded);
}

#[test]
fn new_config_request_roundtrips() {
    let req = proto::FfiRequest {
        message: Some(proto::ffi_request::Message::NewConfig(proto::NewPortalConfigRequest {
            session: "demo".into(),
            role: proto::Role::Operator as i32,
        })),
    };
    roundtrip(&req);
}

#[test]
fn new_portal_response_carries_declared_orders() {
    let resp = proto::FfiResponse {
        message: Some(proto::ffi_response::Message::NewPortal(proto::NewPortalResponse {
            handle: Some(proto::FfiOwnedHandle { id: 42 }),
            state_fields: vec!["j1".into(), "j2".into()],
            action_fields: vec!["j1".into(), "j2".into()],
            video_tracks: vec!["cam1".into()],
        })),
    };
    roundtrip(&resp);
}

#[test]
fn observation_event_roundtrips() {
    let mut state = HashMap::new();
    state.insert("j1".to_string(), 1.5);
    state.insert("j2".to_string(), -2.25);

    let mut frames = HashMap::new();
    frames.insert(
        "cam1".to_string(),
        proto::VideoFrameData {
            width: 640,
            height: 480,
            data: vec![0u8; 640 * 480 * 3 / 2], // I420 byte count
            timestamp_us: 1_000_000,
        },
    );

    let event = proto::FfiEvent {
        message: Some(proto::ffi_event::Message::Observation(proto::ObservationEvent {
            portal_handle: 7,
            observation: Some(proto::Observation {
                timestamp_us: 1_000_000,
                state,
                frames,
            }),
        })),
    };
    roundtrip(&event);
}

#[test]
fn connect_callback_with_error_roundtrips() {
    let event = proto::FfiEvent {
        message: Some(proto::ffi_event::Message::Connect(proto::ConnectCallback {
            async_id: 99,
            error: Some(proto::FfiError {
                variant: "AlreadyConnected".into(),
                message: "portal is already connected".into(),
            }),
        })),
    };
    roundtrip(&event);
}

#[test]
fn drop_event_roundtrips() {
    let mut values = HashMap::new();
    values.insert("j1".to_string(), 0.0);
    let event = proto::FfiEvent {
        message: Some(proto::ffi_event::Message::Drop(proto::DropEvent {
            portal_handle: 3,
            dropped: vec![proto::DroppedState { values }],
        })),
    };
    roundtrip(&event);
}
