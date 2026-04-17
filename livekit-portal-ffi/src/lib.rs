use lazy_static::lazy_static;
use thiserror::Error;

pub mod cabi;
pub mod conversion;
pub mod proto;
pub mod server;

lazy_static! {
    pub static ref FFI_SERVER: server::FfiServer = server::FfiServer::new();
}

pub type FfiResult<T> = Result<T, FfiError>;

#[derive(Error, Debug)]
pub enum FfiError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("handle not found: {0}")]
    HandleNotFound(u64),

    #[error("handle type mismatch for id {0}")]
    HandleTypeMismatch(u64),

    #[error("ffi not initialized. call livekit_portal_ffi_initialize first")]
    NotConfigured,

    #[error("proto decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("portal error: {0}")]
    Portal(#[from] livekit_portal::PortalError),
}
