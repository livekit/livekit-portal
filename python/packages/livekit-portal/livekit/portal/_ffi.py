"""ctypes bridge to the livekit-portal-ffi cdylib.

Responsibilities:
  - locate and load the shared library (build_native.sh copies it beside this package)
  - declare the three `extern "C"` signatures
  - register the single event callback once at import
  - expose `request(FfiRequest) -> FfiResponse` for the wrapper classes

The event callback fires on a tokio worker thread. We keep the trampoline
tiny: decode the bytes, hand off to `_events.dispatch`, return. All user-side
work is scheduled onto the asyncio loop from there.
"""
from __future__ import annotations

import ctypes
import ctypes.util
import os
import pathlib
import sys
from typing import TYPE_CHECKING

from . import _proto  # ensures sys.path is set up for generated pb2 modules
from ._proto import ffi_pb2

if TYPE_CHECKING:
    from ._proto.ffi_pb2 import FfiRequest, FfiResponse

_LIB_BASENAMES = {
    "linux": "liblivekit_portal_ffi.so",
    "darwin": "liblivekit_portal_ffi.dylib",
    "win32": "livekit_portal_ffi.dll",
}

# build_native.sh copies the cargo-produced cdylib (liblivekit_portal_ffi.{so,dylib,dll})
# into this directory. We also check for a `_native.<ext>` name in case a
# different build setup (e.g. maturin with `module-name = "livekit_portal._native"`)
# is used, so the package remains compatible.
def _find_cdylib() -> str:
    here = pathlib.Path(__file__).parent
    platform = "linux" if sys.platform.startswith("linux") else sys.platform
    candidates = []
    # Plain Rust-produced cdylib (from build_native.sh / cargo build)
    if platform in _LIB_BASENAMES:
        candidates.append(here / _LIB_BASENAMES[platform])
    # Fallback: `_native.<ext>` name used by maturin-style builds
    for suffix in (".abi3.so", ".so", ".dylib", ".pyd"):
        candidates.append(here / f"_native{suffix}")
    # Allow override for dev
    env_override = os.environ.get("LIVEKIT_PORTAL_FFI_LIB")
    if env_override:
        candidates.insert(0, pathlib.Path(env_override))

    for candidate in candidates:
        if candidate.exists():
            return str(candidate)
    raise FileNotFoundError(
        "livekit-portal-ffi cdylib not found. Tried: "
        + ", ".join(str(c) for c in candidates)
        + ". Build it with `bash python/packages/livekit-portal/scripts/build_native.sh`"
        " (run from the repo root), or set LIVEKIT_PORTAL_FFI_LIB to the"
        " absolute path of the shared library."
    )


_lib = ctypes.CDLL(_find_cdylib())

# -- C ABI signatures ---------------------------------------------------------

_CallbackType = ctypes.CFUNCTYPE(None, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t)

_lib.livekit_portal_ffi_initialize.restype = None
_lib.livekit_portal_ffi_initialize.argtypes = [
    _CallbackType,
    ctypes.c_char_p,
    ctypes.c_char_p,
]

_lib.livekit_portal_ffi_request.restype = ctypes.c_uint64
_lib.livekit_portal_ffi_request.argtypes = [
    ctypes.POINTER(ctypes.c_uint8),
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)),
    ctypes.POINTER(ctypes.c_size_t),
]

_lib.livekit_portal_ffi_drop_handle.restype = ctypes.c_bool
_lib.livekit_portal_ffi_drop_handle.argtypes = [ctypes.c_uint64]


# -- Event callback registration ---------------------------------------------

def _on_event(ptr, length):
    # Defensive: don't let anything raise back into Rust.
    try:
        if length == 0 or not ptr:
            return
        buf = ctypes.string_at(ctypes.cast(ptr, ctypes.c_void_p), length)
        event = ffi_pb2.FfiEvent.FromString(buf)
        from . import _events  # late import: avoids circular at module load
        _events.dispatch(event)
    except BaseException:  # noqa: BLE001
        import traceback
        traceback.print_exc()


_cb_ref = _CallbackType(_on_event)  # must outlive the cdylib

_SDK_NAME = b"python"
_SDK_VERSION = b"0.1.0"
_lib.livekit_portal_ffi_initialize(_cb_ref, _SDK_NAME, _SDK_VERSION)


# -- request helper -----------------------------------------------------------

def request(req: "FfiRequest") -> "FfiResponse":
    """Send a protobuf FfiRequest, return the decoded FfiResponse.

    Handles memory lifecycle: the Rust-side response buffer is freed via
    `livekit_portal_ffi_drop_handle` before we return.
    """
    payload = req.SerializeToString()
    arr = (ctypes.c_uint8 * len(payload)).from_buffer_copy(payload)
    res_ptr = ctypes.POINTER(ctypes.c_uint8)()
    res_len = ctypes.c_size_t()
    handle = _lib.livekit_portal_ffi_request(
        arr, len(payload), ctypes.byref(res_ptr), ctypes.byref(res_len)
    )
    if handle == 0:
        raise RuntimeError("livekit_portal_ffi_request failed (returned INVALID_HANDLE)")
    try:
        resp_bytes = ctypes.string_at(ctypes.cast(res_ptr, ctypes.c_void_p), res_len.value)
        return ffi_pb2.FfiResponse.FromString(resp_bytes)
    finally:
        _lib.livekit_portal_ffi_drop_handle(handle)
