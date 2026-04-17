#!/usr/bin/env bash
# Regenerate Python protobuf bindings from livekit-portal-ffi/protocol/*.proto
# into python/livekit_portal/_proto/. Run any time the .proto files change.
#
# Requirements: `protoc` on PATH. On macOS: `brew install protobuf`. On Linux:
# install `protobuf-compiler` via your package manager or `uv pip install
# grpcio-tools` and invoke its bundled protoc.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
PROTO_SRC="$REPO_ROOT/livekit-portal-ffi/protocol"
OUT_DIR="$HERE/../livekit/portal/_proto"

if ! command -v protoc >/dev/null 2>&1; then
    echo "error: protoc not found on PATH" >&2
    echo "  macOS: brew install protobuf" >&2
    echo "  Linux: apt-get install protobuf-compiler (or equivalent)" >&2
    exit 1
fi

if [[ ! -d "$PROTO_SRC" ]]; then
    echo "error: proto source dir not found: $PROTO_SRC" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"

# Remove stale generated files (keep __init__.py; it's hand-written).
find "$OUT_DIR" -maxdepth 1 -name '*_pb2.py' -delete

protoc \
    -I "$PROTO_SRC" \
    --python_out="$OUT_DIR" \
    "$PROTO_SRC"/*.proto

# Generated files use flat `import foo_pb2` statements which fail when loaded
# inside a Python package. Our hand-written __init__.py puts $OUT_DIR on
# sys.path as a workaround. make sure it still exists (don't clobber it).
if [[ ! -f "$OUT_DIR/__init__.py" ]]; then
    echo "warning: $OUT_DIR/__init__.py missing; restoring default" >&2
    cat > "$OUT_DIR/__init__.py" <<'EOF'
"""Generated protobuf modules (regenerate via scripts/generate_protos.sh)."""
import os as _os
import sys as _sys

_HERE = _os.path.dirname(_os.path.abspath(__file__))
if _HERE not in _sys.path:
    _sys.path.insert(0, _HERE)

from . import ffi_pb2, handle_pb2, portal_pb2, types_pb2  # noqa: E402,F401
EOF
fi

echo "regenerated pb2 files in $OUT_DIR:"
ls -1 "$OUT_DIR"/*_pb2.py
