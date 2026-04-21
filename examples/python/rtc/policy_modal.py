"""Optional: run the policy side on Modal.

Wraps `policy.py` so the same code runs on a Modal container instead of a
local process. Matches the request-flow demo in `policy.py` but mounts the
local source into the container and runs it as the entrypoint.

Why: in production you want the policy behind a GPU inference server,
accessed over LiveKit. Portal's request/response flow is the same — the
only thing that moves is where Python runs.

Usage:
    modal setup                   # one-time, stores your Modal token
    modal run policy_modal.py     # runs the policy side in a Modal container

The robot keeps running locally (`uv run robot.py`). LiveKit routes the
room so both sides meet.
"""
from __future__ import annotations

import os
import pathlib

import modal

HERE = pathlib.Path(__file__).parent
REPO_ROOT = HERE.parents[3]

# Uses the worktree's editable livekit-portal install so changes to the
# Rust core are picked up without re-publishing a wheel. For a real
# deployment, replace with `pip_install("livekit-portal==<version>")` once
# the package is released.
image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install(
        "numpy",
        "python-dotenv",
        "livekit-api>=0.7",
    )
    # Mount the local livekit-portal editable package plus the prebuilt
    # cdylib. Assumes `build_native.sh` has been run for the host arch.
    .add_local_dir(
        str(REPO_ROOT / "python/packages/livekit-portal"),
        remote_path="/pkgs/livekit-portal",
    )
    .add_local_file(
        str(HERE / "policy.py"), remote_path="/app/policy.py"
    )
    .add_local_file(
        str(HERE / "_common.py"), remote_path="/app/_common.py"
    )
    .run_commands("pip install -e /pkgs/livekit-portal")
)

app = modal.App("portal-rtc-policy", image=image)

secrets = [modal.Secret.from_local_environ(["LIVEKIT_URL", "LIVEKIT_API_KEY",
                                            "LIVEKIT_API_SECRET", "LIVEKIT_ROOM"])]


@app.function(timeout=60 * 60, secrets=secrets)
def run_policy() -> None:
    import asyncio
    import sys

    sys.path.insert(0, "/app")
    from policy import main  # type: ignore

    asyncio.run(main())


@app.local_entrypoint()
def main() -> None:
    run_policy.remote()
