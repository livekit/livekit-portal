"""ActionChunk type smoke tests. Builds the type, inspects fields, and
confirms `get_action_chunk()` returns None before any traffic. Full
round-trip requires a LiveKit room and is covered in examples/python/rtc.
"""
import numpy as np

from livekit.portal import ActionChunk, ChunkDtype, Portal, PortalConfig, Role


def test_chunk_type_constructs_and_carries_payload():
    horizon, action_dim = 16, 7
    payload = np.zeros((horizon, action_dim), dtype=np.float32).tobytes()
    chunk = ActionChunk(
        horizon=horizon,
        action_dim=action_dim,
        dtype=ChunkDtype.F32,
        captured_at_us=123_456,
        payload=payload,
    )
    assert chunk.horizon == horizon
    assert chunk.action_dim == action_dim
    assert chunk.dtype == ChunkDtype.F32
    assert chunk.captured_at_us == 123_456
    # Python `bytes` is what UniFFI surfaces for Vec<u8>.
    assert len(chunk.payload) == horizon * action_dim * 4


def test_chunk_payload_round_trips_through_numpy():
    horizon, action_dim = 8, 3
    tensor = np.arange(horizon * action_dim, dtype=np.float32).reshape(horizon, action_dim)
    chunk = ActionChunk(
        horizon=horizon,
        action_dim=action_dim,
        dtype=ChunkDtype.F32,
        captured_at_us=0,
        payload=tensor.tobytes(),
    )
    back = np.frombuffer(chunk.payload, dtype=np.float32).reshape(
        chunk.horizon, chunk.action_dim
    )
    assert np.array_equal(back, tensor)


def test_get_action_chunk_none_before_any_traffic():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_state(["j1"])
    portal = Portal(cfg)
    assert portal.get_action_chunk() is None


def test_register_byte_stream_handler_noop_when_unregistered():
    cfg = PortalConfig("demo", Role.ROBOT)
    portal = Portal(cfg)

    calls = []

    def handler(sender, data):
        calls.append((sender, data))

    portal.register_byte_stream_handler("my-topic", handler)
    portal.unregister_byte_stream_handler("my-topic")
    # No traffic has been generated, so calls stays empty. What matters is
    # that register/unregister don't throw on a fresh Portal.
    assert calls == []
