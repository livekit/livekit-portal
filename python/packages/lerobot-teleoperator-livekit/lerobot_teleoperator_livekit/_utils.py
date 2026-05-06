from __future__ import annotations


def split_observation_features(
    features: dict,
) -> tuple[list[str], dict[str, tuple[int, ...]]]:
    """Split a lerobot observation_features dict into motor keys and cameras.

    Scalar-valued entries are motor keys; tuple-valued entries are camera
    names mapped to their shape. Returns ``(sorted_motor_keys, cameras)``.
    """
    motor_keys: list[str] = []
    cameras: dict[str, tuple[int, ...]] = {}
    for key, val in features.items():
        if isinstance(val, tuple):
            cameras[key] = val
        else:
            motor_keys.append(key)
    return sorted(motor_keys), cameras
