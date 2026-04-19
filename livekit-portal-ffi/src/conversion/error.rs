use livekit_portal::PortalError;

use crate::proto;

impl From<PortalError> for proto::FfiError {
    fn from(err: PortalError) -> Self {
        let variant = match &err {
            PortalError::Room(_) => "Room",
            PortalError::AlreadyConnected => "AlreadyConnected",
            PortalError::UnknownVideoTrack { .. } => "UnknownVideoTrack",
            PortalError::WrongFrameSize { .. } => "WrongFrameSize",
            PortalError::InvalidFrameDimensions { .. } => "InvalidFrameDimensions",
            PortalError::Deserialization(_) => "Deserialization",
            PortalError::WrongRole(_) => "WrongRole",
        }
        .to_string();
        proto::FfiError { variant, message: err.to_string() }
    }
}
