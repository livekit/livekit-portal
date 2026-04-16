use crate::types::Role;
use thiserror::Error;

pub type PortalResult<T> = Result<T, PortalError>;

#[derive(Error, Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum PortalError {
    #[error("room error: {0}")]
    Room(String),

    #[error("portal is not connected")]
    NotConnected,

    #[error("portal is already connected")]
    AlreadyConnected,

    #[error("unknown video track: {name}")]
    UnknownVideoTrack { name: String },

    #[error("wrong number of values: expected {expected}, got {got}")]
    WrongValueCount { expected: usize, got: usize },

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("data track error: {0}")]
    DataTrack(String),

    #[error("operation not available for role {0:?}")]
    WrongRole(Role),

    #[error("internal error: {0}")]
    Internal(String),
}
