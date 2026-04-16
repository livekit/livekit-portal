pub mod config;
mod data;
pub mod error;
mod portal;
pub mod serialization;
mod sync_buffer;
pub mod types;
mod video;

pub use config::PortalConfig;
pub use error::{PortalError, PortalResult};
pub use portal::*;
pub use types::{Observation, Role, SyncConfig, VideoFrameData};

uniffi::setup_scaffolding!();
