use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::{self, AssertUnwindSafe};
use std::slice;

use prost::Message;

use crate::server::{FfiCallbackFn, FfiConfig, FfiDataBuffer, FfiHandleId, INVALID_HANDLE};
use crate::{proto, FFI_SERVER};

/// Registers the event callback and stores SDK metadata. Must be called once
/// before any other FFI function. Safe to call from any thread, but only the
/// first call takes effect. subsequent calls overwrite.
///
/// # Safety
/// - `cb` must remain valid for the lifetime of the process.
/// - `sdk` and `sdk_version` must point to valid NUL-terminated UTF-8 strings
///   or be null.
#[no_mangle]
pub unsafe extern "C" fn livekit_portal_ffi_initialize(
    cb: FfiCallbackFn,
    sdk: *const c_char,
    sdk_version: *const c_char,
) {
    let sdk = unsafe { c_str_to_owned(sdk) }.unwrap_or_default();
    let sdk_version = unsafe { c_str_to_owned(sdk_version) }.unwrap_or_default();
    FFI_SERVER.set_config(FfiConfig { callback_fn: cb, sdk, sdk_version });
}

/// Send a request to the Rust side.
///
/// Decodes `(data, len)` as a protobuf `FfiRequest`, dispatches it, encodes
/// the `FfiResponse` bytes, and returns a handle id pointing at the response
/// buffer. `*res_ptr` and `*res_len` are set to the buffer's location and
/// length. The caller must copy the bytes and then call
/// `livekit_portal_ffi_drop_handle(<returned id>)` to release the buffer.
///
/// On invalid input (bad protobuf, dispatch error) returns `INVALID_HANDLE`
/// (`0`) and leaves `*res_ptr` / `*res_len` untouched. Callers should check
/// for this before dereferencing.
///
/// # Safety
/// - `(data, len)` must form a valid byte range.
/// - `res_ptr` and `res_len` must point to writable out-params.
#[no_mangle]
pub unsafe extern "C" fn livekit_portal_ffi_request(
    data: *const u8,
    len: usize,
    res_ptr: *mut *const u8,
    res_len: *mut usize,
) -> FfiHandleId {
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    let request = match proto::FfiRequest::decode(bytes) {
        Ok(r) => r,
        Err(e) => {
            log::error!("failed to decode FfiRequest: {e}");
            return INVALID_HANDLE;
        }
    };

    // Catch panics so the core's `assert!` calls (e.g. `set_fps(0)`) surface
    // as ffi errors on the Python side instead of aborting the process.
    let dispatch_result = panic::catch_unwind(AssertUnwindSafe(|| {
        crate::server::requests::handle_request(&FFI_SERVER, request)
    }));
    let response = match dispatch_result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            log::error!("request dispatch failed: {e}");
            return INVALID_HANDLE;
        }
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "panic with non-string payload".to_string()
            };
            log::error!("request dispatch panicked: {msg}");
            return INVALID_HANDLE;
        }
    };

    let encoded = response.encode_to_vec();
    let buffer = FfiDataBuffer::new(encoded);
    let ptr = buffer.as_ptr();
    let buf_len = buffer.len();

    let id = FFI_SERVER.next_id();
    FFI_SERVER.store_handle(id, buffer);

    unsafe {
        *res_ptr = ptr;
        *res_len = buf_len;
    }
    id
}

/// Release a handle (response buffer or domain object). Returns true if the
/// handle existed and was removed; false if the id was unknown.
#[no_mangle]
pub extern "C" fn livekit_portal_ffi_drop_handle(id: FfiHandleId) -> bool {
    FFI_SERVER.drop_handle(id)
}

unsafe fn c_str_to_owned(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    Some(s)
}
