use crate::types::Role;
use thiserror::Error;

pub type PortalResult<T> = Result<T, PortalError>;

#[derive(Error, Debug)]
pub enum PortalError {
    #[error("room error: {0}")]
    Room(String),

    #[error("portal is already connected")]
    AlreadyConnected,

    #[error("unknown video track: {name}")]
    UnknownVideoTrack { name: String },

    #[error("wrong number of values: expected {expected}, got {got}")]
    WrongValueCount { expected: usize, got: usize },

    #[error("wrong frame size: expected {expected} bytes, got {got}")]
    WrongFrameSize { expected: usize, got: usize },

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("operation not available for role {0:?}")]
    WrongRole(Role),
}
