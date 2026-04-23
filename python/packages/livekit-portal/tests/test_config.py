"""Config builder smoke tests. No networking, no runtime."""
import pytest

from livekit.portal import Action, DType, FieldSpec, Portal, PortalConfig, PortalError, Role, State


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


def test_fieldspec_accepted_as_schema_entry():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_action_typed(
        [FieldSpec(name="j1", dtype=DType.F32), ("j2", DType.F64)]
    )
    assert cfg.action_schema == [("j1", DType.F32), ("j2", DType.F64)]


def _mixed_schema_portal():
    cfg = PortalConfig("typed", Role.OPERATOR)
    cfg.add_action_typed(
        [
            ("shoulder", DType.F32),
            ("elbow", DType.F32),
            ("gripper", DType.BOOL),
            ("mode", DType.I8),
            ("counter", DType.U16),
        ]
    )
    cfg.add_state_typed(
        [
            ("j1", DType.F32),
            ("j2", DType.F32),
            ("estop", DType.BOOL),
        ]
    )
    return Portal(cfg)


def test_typed_action_reconstructs_native_types():
    portal = _mixed_schema_portal()
    # What would arrive on the callback: all floats since that's what the
    # FFI delivers. typed_action maps them back to the declared Python type.
    action = Action(
        values={
            "shoulder": 0.5,
            "elbow": -1.25,
            "gripper": 1.0,
            "mode": 3.0,
            "counter": 42.0,
        },
        timestamp_us=0,
    )
    typed = portal.typed_action(action)
    assert typed == {
        "shoulder": 0.5,
        "elbow": -1.25,
        "gripper": True,
        "mode": 3,
        "counter": 42,
    }
    assert isinstance(typed["gripper"], bool)
    assert isinstance(typed["mode"], int)
    assert isinstance(typed["counter"], int)
    assert isinstance(typed["shoulder"], float)


def test_typed_state_accepts_raw_dict():
    portal = _mixed_schema_portal()
    # Observation.state arrives as a plain dict in lerobot/observer code; the
    # helper should handle both a dict and a State record.
    typed = portal.typed_state({"j1": 0.1, "j2": -0.2, "estop": 0.0})
    assert typed == {"j1": 0.1, "j2": -0.2, "estop": False}
    assert isinstance(typed["estop"], bool)


def test_typed_helpers_drop_fields_missing_from_payload():
    portal = _mixed_schema_portal()
    # Partial update — gripper and mode absent. typed_action returns only
    # the fields actually present, preserving their dtype cast.
    typed = portal.typed_action(
        Action(values={"shoulder": 0.25}, timestamp_us=0)
    )
    assert typed == {"shoulder": 0.25}


def test_typed_state_via_state_record():
    portal = _mixed_schema_portal()
    typed = portal.typed_state(
        State(values={"j1": 0.1, "estop": 1.0}, timestamp_us=0)
    )
    assert typed == {"j1": 0.1, "estop": True}
