use std::collections::HashMap;

use livekit_portal::{Portal, PortalConfig, Role};

use super::config::FfiPortalConfig;
use super::portal::FfiPortal;
use super::FfiServer;
use crate::proto;
use crate::{FfiError, FfiResult};

/// Dispatch an FfiRequest to the right handler.
pub fn handle_request(
    server: &'static FfiServer,
    request: proto::FfiRequest,
) -> FfiResult<proto::FfiResponse> {
    let _async_guard = server.async_runtime.enter();

    let message = request
        .message
        .ok_or_else(|| FfiError::InvalidRequest("FfiRequest.message empty".into()))?;

    use proto::ffi_request::Message as Req;
    use proto::ffi_response::Message as Res;

    let resp: Res = match message {
        Req::NewConfig(r) => Res::NewConfig(on_new_config(server, r)?),
        Req::ConfigAddVideo(r) => Res::ConfigAddVideo(on_config_add_video(server, r)?),
        Req::ConfigAddState(r) => Res::ConfigAddState(on_config_add_state(server, r)?),
        Req::ConfigAddAction(r) => Res::ConfigAddAction(on_config_add_action(server, r)?),
        Req::ConfigSetFps(r) => Res::ConfigSetFps(on_config_set_fps(server, r)?),
        Req::ConfigSetSlack(r) => Res::ConfigSetSlack(on_config_set_slack(server, r)?),
        Req::ConfigSetTolerance(r) => Res::ConfigSetTolerance(on_config_set_tolerance(server, r)?),
        Req::ConfigSetStateReliable(r) => {
            Res::ConfigSetStateReliable(on_config_set_state_reliable(server, r)?)
        }
        Req::ConfigSetActionReliable(r) => {
            Res::ConfigSetActionReliable(on_config_set_action_reliable(server, r)?)
        }
        Req::ConfigSetPingMs(r) => Res::ConfigSetPingMs(on_config_set_ping_ms(server, r)?),

        Req::NewPortal(r) => Res::NewPortal(on_new_portal(server, r)?),
        Req::Connect(r) => Res::Connect(on_connect(server, r)?),
        Req::Disconnect(r) => Res::Disconnect(on_disconnect(server, r)?),
        Req::SendVideoFrame(r) => Res::SendVideoFrame(on_send_video_frame(server, r)?),
        Req::SendState(r) => Res::SendState(on_send_state(server, r)?),
        Req::SendAction(r) => Res::SendAction(on_send_action(server, r)?),
        Req::GetObservation(r) => Res::GetObservation(on_get_observation(server, r)?),
        Req::GetAction(r) => Res::GetAction(on_get_action(server, r)?),
        Req::GetState(r) => Res::GetState(on_get_state(server, r)?),
        Req::GetVideoFrame(r) => Res::GetVideoFrame(on_get_video_frame(server, r)?),
        Req::Metrics(r) => Res::Metrics(on_metrics(server, r)?),
        Req::ResetMetrics(r) => Res::ResetMetrics(on_reset_metrics(server, r)?),

        Req::DisposeHandle(r) => Res::DisposeHandle(on_dispose_handle(server, r)?),
    };

    Ok(proto::FfiResponse { message: Some(resp) })
}

// -------- Config handlers --------

fn on_new_config(
    server: &'static FfiServer,
    req: proto::NewPortalConfigRequest,
) -> FfiResult<proto::NewPortalConfigResponse> {
    let role: Role = proto::Role::try_from(req.role)
        .map_err(|_| FfiError::InvalidRequest(format!("unknown role {}", req.role)))?
        .into();
    let config = PortalConfig::new(req.session, role);
    let ffi = FfiPortalConfig::new(config);
    let id = server.next_id();
    server.store_handle(id, ffi);
    Ok(proto::NewPortalConfigResponse { handle: Some(proto::FfiOwnedHandle { id }) })
}

fn on_config_add_video(
    server: &'static FfiServer,
    req: proto::ConfigAddVideoRequest,
) -> FfiResult<proto::ConfigAddVideoResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.add_video(req.name);
    Ok(proto::ConfigAddVideoResponse {})
}

fn on_config_add_state(
    server: &'static FfiServer,
    req: proto::ConfigAddStateRequest,
) -> FfiResult<proto::ConfigAddStateResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.add_state(req.fields);
    Ok(proto::ConfigAddStateResponse {})
}

fn on_config_add_action(
    server: &'static FfiServer,
    req: proto::ConfigAddActionRequest,
) -> FfiResult<proto::ConfigAddActionResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.add_action(req.fields);
    Ok(proto::ConfigAddActionResponse {})
}

fn on_config_set_fps(
    server: &'static FfiServer,
    req: proto::ConfigSetFpsRequest,
) -> FfiResult<proto::ConfigSetFpsResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.with_mut(|c| c.set_fps(req.fps));
    Ok(proto::ConfigSetFpsResponse {})
}

fn on_config_set_slack(
    server: &'static FfiServer,
    req: proto::ConfigSetSlackRequest,
) -> FfiResult<proto::ConfigSetSlackResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.with_mut(|c| c.set_slack(req.ticks));
    Ok(proto::ConfigSetSlackResponse {})
}

fn on_config_set_tolerance(
    server: &'static FfiServer,
    req: proto::ConfigSetToleranceRequest,
) -> FfiResult<proto::ConfigSetToleranceResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.with_mut(|c| c.set_tolerance(req.ticks));
    Ok(proto::ConfigSetToleranceResponse {})
}

fn on_config_set_state_reliable(
    server: &'static FfiServer,
    req: proto::ConfigSetStateReliableRequest,
) -> FfiResult<proto::ConfigSetStateReliableResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.with_mut(|c| c.set_state_reliable(req.reliable));
    Ok(proto::ConfigSetStateReliableResponse {})
}

fn on_config_set_action_reliable(
    server: &'static FfiServer,
    req: proto::ConfigSetActionReliableRequest,
) -> FfiResult<proto::ConfigSetActionReliableResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.with_mut(|c| c.set_action_reliable(req.reliable));
    Ok(proto::ConfigSetActionReliableResponse {})
}

fn on_config_set_ping_ms(
    server: &'static FfiServer,
    req: proto::ConfigSetPingMsRequest,
) -> FfiResult<proto::ConfigSetPingMsResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    cfg.with_mut(|c| c.set_ping_ms(req.ms));
    Ok(proto::ConfigSetPingMsResponse {})
}

// -------- Portal lifecycle --------

fn on_new_portal(
    server: &'static FfiServer,
    req: proto::NewPortalRequest,
) -> FfiResult<proto::NewPortalResponse> {
    let cfg = server.retrieve_handle::<FfiPortalConfig>(req.config_handle)?;
    let declared = cfg.declared_fields();
    let snapshot = cfg.snapshot();
    let portal = Portal::new(snapshot);

    let id = server.next_id();
    let ffi = FfiPortal::new(
        id,
        portal,
        declared.video_tracks.clone(),
        declared.state_fields.clone(),
        declared.action_fields.clone(),
    );
    server.store_handle(id, ffi);

    Ok(proto::NewPortalResponse {
        handle: Some(proto::FfiOwnedHandle { id }),
        state_fields: declared.state_fields,
        action_fields: declared.action_fields,
        video_tracks: declared.video_tracks,
    })
}

fn on_connect(
    server: &'static FfiServer,
    req: proto::ConnectRequest,
) -> FfiResult<proto::ConnectResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    let async_id = server.next_id();
    let url = req.url;
    let token = req.token;

    server.async_runtime.spawn(async move {
        let error = ffi.inner.connect(&url, &token).await.err().map(Into::into);
        server.send_event(proto::ffi_event::Message::Connect(proto::ConnectCallback {
            async_id,
            error,
        }));
    });

    Ok(proto::ConnectResponse { async_id })
}

fn on_disconnect(
    server: &'static FfiServer,
    req: proto::DisconnectRequest,
) -> FfiResult<proto::DisconnectResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    let async_id = server.next_id();

    server.async_runtime.spawn(async move {
        let error = ffi.inner.disconnect().await.err().map(Into::into);
        server.send_event(proto::ffi_event::Message::Disconnect(proto::DisconnectCallback {
            async_id,
            error,
        }));
    });

    Ok(proto::DisconnectResponse { async_id })
}

// -------- Send API --------

fn on_send_video_frame(
    server: &'static FfiServer,
    req: proto::SendVideoFrameRequest,
) -> FfiResult<proto::SendVideoFrameResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    let res = ffi.inner.send_video_frame(
        &req.track_name,
        &req.rgb_data,
        req.width,
        req.height,
        req.timestamp_us,
    );
    Ok(proto::SendVideoFrameResponse { error: res.err().map(Into::into) })
}

fn on_send_state(
    server: &'static FfiServer,
    req: proto::SendStateRequest,
) -> FfiResult<proto::SendStateResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    let values: HashMap<String, f64> = req.values;
    let res = ffi.inner.send_state(&values, req.timestamp_us);
    Ok(proto::SendStateResponse { error: res.err().map(Into::into) })
}

fn on_send_action(
    server: &'static FfiServer,
    req: proto::SendActionRequest,
) -> FfiResult<proto::SendActionResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    let values: HashMap<String, f64> = req.values;
    let res = ffi.inner.send_action(&values, req.timestamp_us);
    Ok(proto::SendActionResponse { error: res.err().map(Into::into) })
}

// -------- Pull API --------

fn on_get_observation(
    server: &'static FfiServer,
    req: proto::GetObservationRequest,
) -> FfiResult<proto::GetObservationResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    Ok(proto::GetObservationResponse { observation: ffi.inner.get_observation().map(Into::into) })
}

fn on_get_action(
    server: &'static FfiServer,
    req: proto::GetActionRequest,
) -> FfiResult<proto::GetActionResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    match ffi.inner.get_action() {
        Some(map) => Ok(proto::GetActionResponse { values: map, present: true }),
        None => Ok(proto::GetActionResponse { values: HashMap::new(), present: false }),
    }
}

fn on_get_state(
    server: &'static FfiServer,
    req: proto::GetStateRequest,
) -> FfiResult<proto::GetStateResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    match ffi.inner.get_state() {
        Some(map) => Ok(proto::GetStateResponse { values: map, present: true }),
        None => Ok(proto::GetStateResponse { values: HashMap::new(), present: false }),
    }
}

fn on_get_video_frame(
    server: &'static FfiServer,
    req: proto::GetVideoFrameRequest,
) -> FfiResult<proto::GetVideoFrameResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    Ok(proto::GetVideoFrameResponse {
        frame: ffi.inner.get_video_frame(&req.track_name).map(Into::into),
    })
}

// -------- Metrics --------

fn on_metrics(
    server: &'static FfiServer,
    req: proto::MetricsRequest,
) -> FfiResult<proto::MetricsResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    Ok(proto::MetricsResponse { metrics: Some(ffi.inner.metrics().into()) })
}

fn on_reset_metrics(
    server: &'static FfiServer,
    req: proto::ResetMetricsRequest,
) -> FfiResult<proto::ResetMetricsResponse> {
    let ffi = server.retrieve_handle::<FfiPortal>(req.portal_handle)?;
    ffi.inner.reset_metrics();
    Ok(proto::ResetMetricsResponse {})
}

// -------- Handle disposal --------

fn on_dispose_handle(
    server: &'static FfiServer,
    req: proto::DisposeHandleRequest,
) -> FfiResult<proto::DisposeHandleResponse> {
    Ok(proto::DisposeHandleResponse { existed: server.drop_handle(req.handle) })
}
