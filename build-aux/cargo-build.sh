#!/bin/sh
# Meson -> cargo bridge. Meson passes the build parameters positionally; we
# export the FORESIGHT_* env the app's build.rs reads, build, then copy the
# binary to the Meson-declared output path.
set -eu

CARGO="$1"        # absolute path to cargo
MANIFEST="$2"     # workspace Cargo.toml
TARGET_DIR="$3"   # cargo --target-dir (inside the Meson build tree)
PROFILE="$4"      # "release" | "debug"
BIN="$5"          # package + binary name
OUTPUT="$6"       # Meson @OUTPUT@
APP_ID="$7"
PKGDATADIR="$8"
VERSION="$9"

export FORESIGHT_APP_ID="$APP_ID"
export FORESIGHT_PKGDATADIR="$PKGDATADIR"
export FORESIGHT_VERSION="$VERSION"
export FORESIGHT_PROFILE="$PROFILE"

if [ "$PROFILE" = "release" ]; then
    "$CARGO" build --manifest-path "$MANIFEST" --target-dir "$TARGET_DIR" -p "$BIN" --release
    built="$TARGET_DIR/release/$BIN"
else
    "$CARGO" build --manifest-path "$MANIFEST" --target-dir "$TARGET_DIR" -p "$BIN"
    built="$TARGET_DIR/debug/$BIN"
fi

cp "$built" "$OUTPUT"
