"""Config builder + registry smoke tests. No networking, no runtime."""
import pytest

from livekit.portal import Portal, PortalConfig, PortalError, Role


def test_new_config_assigns_handle():
    cfg = PortalConfig("demo", Role.OPERATOR)
    try:
        assert cfg.handle != 0
        assert cfg.session == "demo"
        assert cfg.role == Role.OPERATOR
    finally:
        cfg.close()


def test_config_adders_are_captured():
    cfg = PortalConfig("demo", Role.ROBOT)
    try:
        cfg.add_video("cam1")
        cfg.add_video("cam2")
        cfg.add_state(["j1", "j2", "j3"])
        cfg.add_action(["j1", "j2", "j3"])
        assert cfg.video_tracks == ["cam1", "cam2"]
        assert cfg.state_fields == ["j1", "j2", "j3"]
        assert cfg.action_fields == ["j1", "j2", "j3"]
    finally:
        cfg.close()


def test_set_fps_zero_raises():
    cfg = PortalConfig("demo", Role.ROBOT)
    # The core `set_fps(0)` asserts; the FFI dispatch surfaces the assertion
    # as a failed request (INVALID_HANDLE returned). We expect any exception
    #. we don't depend on which exact variant propagates.
    with pytest.raises(Exception):
        cfg.set_fps(0)
    cfg.close()


def test_new_portal_echoes_declared_fields():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_video("cam1")
    cfg.add_state(["j1", "j2"])
    cfg.add_action(["j1", "j2"])

    portal = Portal(cfg)
    try:
        # The Portal wrapper snapshots these from the NewPortalResponse, not
        # from the config object. this verifies the round-trip.
        assert portal._state_fields == ["j1", "j2"]
        assert portal._action_fields == ["j1", "j2"]
        assert portal._video_tracks == ["cam1"]
    finally:
        portal.close()
        cfg.close()


def test_get_action_returns_none_when_empty():
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_action(["j1"])
    portal = Portal(cfg)
    try:
        assert portal.get_action() is None
        assert portal.get_state() is None
    finally:
        portal.close()
        cfg.close()


def test_send_action_before_connect_is_wrong_role_error():
    # Robot role should be rejected from send_action (operator-only).
    cfg = PortalConfig("demo", Role.ROBOT)
    cfg.add_action(["j1"])
    portal = Portal(cfg)
    try:
        with pytest.raises(PortalError) as exc:
            portal.send_action({"j1": 1.0})
        assert exc.value.variant == "WrongRole"
    finally:
        portal.close()
        cfg.close()
