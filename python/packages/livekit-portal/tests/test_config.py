"""Config builder smoke tests. No networking, no runtime."""
import pytest

from livekit.portal import DType, FieldSpec, Portal, PortalConfig, PortalError, Role


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


# --- Typed delivery wrappers ------------------------------------------------
#
# These tests exercise the Python wrappers (`Action`, `State`, `Observation`)
# rather than the FFI records. We build an FFI record by hand and pass it
# through the same `_wrap_*` helpers the dispatcher uses on live deliveries.


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


def test_action_wrapper_values_are_typed_by_default():
    from livekit.portal import _wrap_action
    from livekit.portal import livekit_portal_ffi as _ffi

    portal = _mixed_schema_portal()
    ffi_action = _ffi.Action(
        values={
            "shoulder": 0.5,
            "elbow": -1.25,
            "gripper": 1.0,
            "mode": 3.0,
            "counter": 42.0,
        },
        timestamp_us=100,
    )
    action = _wrap_action(ffi_action, portal._action_schema)
    assert action.timestamp_us == 100
    # Typed by default.
    assert action.values == {
        "shoulder": 0.5,
        "elbow": -1.25,
        "gripper": True,
        "mode": 3,
        "counter": 42,
    }
    assert isinstance(action.values["gripper"], bool)
    assert isinstance(action.values["mode"], int)
    assert isinstance(action.values["shoulder"], float)
    # Raw escape hatch preserves the f64 dict.
    assert action.raw_values == {
        "shoulder": 0.5,
        "elbow": -1.25,
        "gripper": 1.0,
        "mode": 3.0,
        "counter": 42.0,
    }


def test_state_wrapper_values_are_typed_by_default():
    from livekit.portal import _wrap_state
    from livekit.portal import livekit_portal_ffi as _ffi

    portal = _mixed_schema_portal()
    ffi_state = _ffi.State(
        values={"j1": 0.1, "j2": -0.2, "estop": 1.0},
        timestamp_us=99,
    )
    state = _wrap_state(ffi_state, portal._state_schema)
    assert state.values == {"j1": 0.1, "j2": -0.2, "estop": True}
    assert isinstance(state.values["estop"], bool)
    assert state.raw_values["estop"] == 1.0


def test_observation_wrapper_exposes_typed_state():
    from livekit.portal import _wrap_observation
    from livekit.portal import livekit_portal_ffi as _ffi

    portal = _mixed_schema_portal()
    ffi_obs = _ffi.Observation(
        state={"j1": 0.1, "j2": 0.2, "estop": 0.0},
        frames={},
        timestamp_us=50,
    )
    obs = _wrap_observation(ffi_obs, portal._state_schema)
    assert obs.state == {"j1": 0.1, "j2": 0.2, "estop": False}
    assert obs.raw_state == {"j1": 0.1, "j2": 0.2, "estop": 0.0}
    assert obs.frames == {}
    assert obs.timestamp_us == 50


def test_wrapper_drops_fields_missing_from_payload():
    from livekit.portal import _wrap_action
    from livekit.portal import livekit_portal_ffi as _ffi

    portal = _mixed_schema_portal()
    ffi_action = _ffi.Action(values={"shoulder": 0.25}, timestamp_us=0)
    action = _wrap_action(ffi_action, portal._action_schema)
    # Partial payload → wrapper returns only the fields that were sent.
    assert action.values == {"shoulder": 0.25}
    assert action.raw_values == {"shoulder": 0.25}
