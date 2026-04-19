#!/usr/bin/env bash
# Build the livekit-portal-ffi cdylib and copy it into the Python package.
# Run from anywhere; paths are resolved relative to this script.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# scripts/ lives at packages/livekit-portal/scripts/ under the python workspace,
# so repo root is four levels up from here.
REPO_ROOT="$(cd "$HERE/../../../.." && pwd)"
PKG_DIR="$HERE/../livekit/portal"

MODE="${1:-release}"
if [[ "$MODE" != "release" && "$MODE" != "debug" ]]; then
    echo "usage: $0 [release|debug]" >&2
    exit 1
fi

cargo_flags=()
target_subdir="debug"
if [[ "$MODE" == "release" ]]; then
    cargo_flags+=("--release")
    target_subdir="release"
fi

(cd "$REPO_ROOT" && cargo build -p livekit-portal-ffi "${cargo_flags[@]}")

# Copy the freshly built cdylib into the package. Locate by platform extension.
case "$(uname -s)" in
    Darwin)  ext=".dylib"; base="liblivekit_portal_ffi" ;;
    Linux)   ext=".so";    base="liblivekit_portal_ffi" ;;
    MINGW*|MSYS*|CYGWIN*) ext=".dll"; base="livekit_portal_ffi" ;;
    *) echo "unsupported platform: $(uname -s)" >&2; exit 1 ;;
esac

src="$REPO_ROOT/target/$target_subdir/${base}${ext}"
dst="$PKG_DIR/${base}${ext}"
if [[ ! -f "$src" ]]; then
    echo "cargo did not produce $src" >&2
    exit 1
fi
cp "$src" "$dst"
echo "copied $src -> $dst"
