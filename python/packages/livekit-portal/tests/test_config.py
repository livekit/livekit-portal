"""Config builder smoke tests. No networking, no runtime."""
import pytest

from livekit.portal import DType, Portal, PortalConfig, PortalError, Role


def test_new_config_constructs():
    cfg = PortalConfig("demo", Role.OPERATOR)
    assert cfg.session == "demo"
    assert cfg.role == Role.OPERATOR


def test_config_adders_are_captured():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_video("cam1")
    cfg.add_video("cam2")
    cfg.add_state_typed([("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)])
    cfg.add_action_typed([("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)])
    assert cfg.video_tracks == ["cam1", "cam2"]
    assert cfg.state_fields == ["j1", "j2", "j3"]
    assert cfg.action_fields == ["j1", "j2", "j3"]
    assert cfg.state_schema == [("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)]
    assert cfg.action_schema == [("j1", DType.F32), ("j2", DType.F32), ("j3", DType.F32)]


def test_mixed_dtype_schema_is_accepted():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_action_typed(
        [
            ("shoulder", DType.F32),
            ("gripper", DType.BOOL),
            ("mode", DType.I8),
            ("counter", DType.U16),
        ]
    )
    assert cfg.action_fields == ["shoulder", "gripper", "mode", "counter"]
    assert cfg.action_schema == [
        ("shoulder", DType.F32),
        ("gripper", DType.BOOL),
        ("mode", DType.I8),
        ("counter", DType.U16),
    ]


def test_set_fps_zero_raises():
    cfg = PortalConfig("demo", Role.ROBOT)
    # The core `set_fps(0)` asserts; UniFFI surfaces the panic as an
    # `InternalError` from the generated module. We accept any Exception.
    with pytest.raises(Exception):
        cfg.set_fps(0)


def test_new_portal_echoes_declared_fields():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_video("cam1")
    cfg.add_state_typed([("j1", DType.F64), ("j2", DType.F64)])
    cfg.add_action_typed([("j1", DType.F64), ("j2", DType.F64)])

    portal = Portal(cfg)
    # The Portal snapshots these from the core after construction.
    assert portal._state_fields == ["j1", "j2"]
    assert portal._action_fields == ["j1", "j2"]
    assert portal._video_tracks == ["cam1"]


def test_get_action_returns_none_when_empty():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_action_typed([("j1", DType.F64)])
    portal = Portal(cfg)
    assert portal.get_action() is None
    assert portal.get_state() is None


def test_send_action_before_connect_is_wrong_role_error():
    # Robot role should be rejected from send_action (operator-only).
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_action_typed([("j1", DType.F64)])
    portal = Portal(cfg)
    with pytest.raises(PortalError.WrongRole):
        portal.send_action({"j1": 1.0})
