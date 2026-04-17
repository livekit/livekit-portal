use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use downcast_rs::{impl_downcast, DowncastSync};
use parking_lot::Mutex;
use prost::Message;
use tokio::runtime::Runtime;

use crate::proto;
use crate::{FfiError, FfiResult};

pub mod config;
pub mod portal;
pub mod requests;

pub type FfiHandleId = u64;
pub const INVALID_HANDLE: FfiHandleId = 0;

pub type FfiCallbackFn = unsafe extern "C" fn(*const u8, usize);

/// Every object stored in the handle registry implements this.
pub trait FfiHandle: DowncastSync + Send + Sync + 'static {}
impl_downcast!(sync FfiHandle);

#[derive(Clone)]
pub(crate) struct FfiConfig {
    pub callback_fn: FfiCallbackFn,
    #[allow(dead_code)]
    pub sdk: String,
    #[allow(dead_code)]
    pub sdk_version: String,
}

pub struct FfiServer {
    handles: DashMap<FfiHandleId, Box<dyn FfiHandle>>,
    pub async_runtime: Runtime,
    next_id: AtomicU64,
    pub(crate) config: Mutex<Option<FfiConfig>>,
}

impl FfiServer {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("livekit-portal-ffi")
            .build()
            .expect("failed to build tokio runtime");

        Self {
            handles: DashMap::new(),
            async_runtime: runtime,
            // Start at 1; 0 is reserved as INVALID_HANDLE.
            next_id: AtomicU64::new(1),
            config: Mutex::new(None),
        }
    }

    pub fn next_id(&self) -> FfiHandleId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns the resolved id: `client_id` if non-zero, otherwise a fresh one.
    /// Matches livekit-ffi's pattern of letting clients choose their own async_id
    /// to correlate requests with callbacks.
    pub fn resolve_async_id(&self, client_id: u64) -> u64 {
        if client_id == 0 { self.next_id() } else { client_id }
    }

    pub fn store_handle<T: FfiHandle>(&self, id: FfiHandleId, value: T) {
        self.handles.insert(id, Box::new(value));
    }

    pub fn retrieve_handle<T: FfiHandle + Clone>(&self, id: FfiHandleId) -> FfiResult<T> {
        let entry = self.handles.get(&id).ok_or(FfiError::HandleNotFound(id))?;
        entry
            .value()
            .as_any()
            .downcast_ref::<T>()
            .cloned()
            .ok_or(FfiError::HandleTypeMismatch(id))
    }

    pub fn drop_handle(&self, id: FfiHandleId) -> bool {
        self.handles.remove(&id).is_some()
    }

    pub fn send_event(&self, message: proto::ffi_event::Message) {
        let guard = self.config.lock();
        let Some(cfg) = guard.as_ref() else {
            log::warn!("send_event called but FFI not initialized");
            return;
        };
        let cb = cfg.callback_fn;
        drop(guard);

        let event = proto::FfiEvent { message: Some(message) };
        let bytes = event.encode_to_vec();
        unsafe { cb(bytes.as_ptr(), bytes.len()) };
    }

    pub(crate) fn set_config(&self, cfg: FfiConfig) {
        *self.config.lock() = Some(cfg);
    }
}

impl Default for FfiServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Response buffer stored in the handle registry. `livekit_portal_ffi_request`
/// returns the id of one of these; the caller frees it via `_drop_handle`.
pub(crate) struct FfiDataBuffer {
    pub data: Vec<u8>,
}
impl FfiHandle for FfiDataBuffer {}

/// Shared helper used by request handlers that need the raw buffer pointer.
impl FfiDataBuffer {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
    pub fn as_ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
}

/// Check that `Any` downcast_ref works via DowncastSync so retrieve_handle can
/// statically dispatch without boxing twice.
#[allow(dead_code)]
fn _assert_trait_object() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<Box<dyn FfiHandle>>();
}
