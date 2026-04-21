#!/usr/bin/env bash
# Build the livekit-portal-ffi cdylib and regenerate the UniFFI Python
# bindings. The cdylib and generated module both land next to the package
# source (livekit/portal/) so the wheel ships them together.
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

# Build the cdylib.
(cd "$REPO_ROOT" && cargo build -p livekit-portal-ffi ${cargo_flags[@]+"${cargo_flags[@]}"})

# Locate the freshly built cdylib by platform extension.
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

# Generate the Python module from the cdylib (library mode — no UDL file).
# The generated `livekit_portal_ffi.py` lives next to the shared library so
# its relative-path `CDLL` lookup resolves.
(cd "$REPO_ROOT" && cargo run --bin uniffi-bindgen ${cargo_flags[@]+"${cargo_flags[@]}"} -- \
    generate \
    --library "$src" \
    --language python \
    --out-dir "$PKG_DIR" \
    --no-format)

echo "generated UniFFI Python module at $PKG_DIR/livekit_portal_ffi.py"
