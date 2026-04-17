"""Frame input/output helpers.

Send side (`normalize_rgb`): the Rust FFI takes packed RGB24 (W*H*3 bytes).
We accept either raw `bytes` / `bytearray` / memoryview, or a numpy array of
shape `(H, W, 3)` dtype uint8 and infer W/H from the array.

Receive side: `VideoFrameData.data` carries planar I420 bytes (Y plane, then
U plane at quarter size, then V plane). `i420_bytes_to_numpy_rgb` provides a
best-effort conversion for display. not performance-critical; users in the
hot path should parse the planes directly.
"""
from __future__ import annotations

from typing import Optional, Tuple, Union

import numpy as np

FrameLike = Union[bytes, bytearray, memoryview, np.ndarray]


def normalize_rgb(
    frame: FrameLike,
    width: Optional[int],
    height: Optional[int],
) -> Tuple[bytes, int, int]:
    if isinstance(frame, np.ndarray):
        if frame.dtype != np.uint8 or frame.ndim != 3 or frame.shape[2] != 3:
            raise ValueError(
                f"numpy RGB frame must be uint8 with shape (H, W, 3); got dtype={frame.dtype}, "
                f"shape={frame.shape}"
            )
        h, w, _ = frame.shape
        if width is not None and width != w:
            raise ValueError(f"width mismatch: array is {w}, argument is {width}")
        if height is not None and height != h:
            raise ValueError(f"height mismatch: array is {h}, argument is {height}")
        contiguous = np.ascontiguousarray(frame)
        return contiguous.tobytes(), w, h

    if isinstance(frame, (bytes, bytearray, memoryview)):
        if width is None or height is None:
            raise ValueError("width and height are required when frame is raw bytes")
        data = bytes(frame)
        expected = width * height * 3
        if len(data) != expected:
            raise ValueError(
                f"raw RGB frame size mismatch: expected {expected} bytes "
                f"(W*H*3 = {width}*{height}*3), got {len(data)}"
            )
        return data, width, height

    raise TypeError(
        f"unsupported frame type: {type(frame).__name__}. Pass bytes or np.ndarray (H,W,3) uint8."
    )


def i420_bytes_to_numpy_rgb(data: bytes, width: int, height: int) -> np.ndarray:
    """Decode I420 planar bytes → (H, W, 3) uint8 RGB.

    Best-effort, pure-numpy conversion suitable for preview/display. If you
    need speed or colorimetric accuracy, decode the planes yourself or feed
    them to a tuned converter (opencv, av, libyuv).
    """
    w, h = width, height
    if w % 2 or h % 2:
        raise ValueError(f"I420 requires even dimensions; got {w}x{h}")

    y_size = w * h
    uv_size = (w // 2) * (h // 2)
    expected = y_size + 2 * uv_size
    if len(data) != expected:
        raise ValueError(
            f"I420 size mismatch: expected {expected} bytes (Y={y_size}, U=V={uv_size}), "
            f"got {len(data)}"
        )

    buf = np.frombuffer(data, dtype=np.uint8)
    y = buf[:y_size].reshape(h, w).astype(np.float32)
    u = buf[y_size : y_size + uv_size].reshape(h // 2, w // 2).astype(np.float32)
    v = buf[y_size + uv_size :].reshape(h // 2, w // 2).astype(np.float32)

    # Upsample chroma via nearest-neighbor repeat (cheapest correct option).
    u_full = u.repeat(2, axis=0).repeat(2, axis=1)
    v_full = v.repeat(2, axis=0).repeat(2, axis=1)

    # BT.601 full-range → RGB
    c = y - 16.0
    d = u_full - 128.0
    e = v_full - 128.0
    r = 1.164 * c + 1.596 * e
    g = 1.164 * c - 0.392 * d - 0.813 * e
    b = 1.164 * c + 2.017 * d
    rgb = np.stack([r, g, b], axis=-1)
    return np.clip(rgb, 0, 255).astype(np.uint8)
