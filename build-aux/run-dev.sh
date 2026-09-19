#!/bin/sh
# Build the dev manifest and run it WITHOUT installing it.
#
# The installed Foresight is the published one, from the hosted repo. A dev
# build installed beside it (`flatpak-builder --install` lands on the `master`
# branch) takes over the desktop icon, and two branches of one app id make
# xdg-document-portal and a bare `flatpak run <id>` fail with "Multiple
# branches available". So dev builds run from build-dir instead.
#
# This is `flatpak-builder --run` plus one thing it leaves out: the document
# portal mount. Without it the app has its id and can reach the portals, but a
# folder picked through one comes back as a /run/user/$UID/doc/… path that does
# not exist inside the sandbox — and that path is the only way Foresight ever
# sees a file. The sandbox permissions are read from the manifest, so this runs
# with what ships and nothing wider.
#
# Usage:  build-aux/run-dev.sh [--no-build] [command [args…]]
#         build-aux/run-dev.sh                       # build, then run the app
#         build-aux/run-dev.sh --no-build            # run the last build as is
#         build-aux/run-dev.sh rsync --version       # the bundled engine
set -eu

HERE="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST="$HERE/io.github.superuser_miguel.Foresight.yml"
BUILD_DIR="$HERE/build-dir"

build=1
if [ "${1:-}" = "--no-build" ]; then
    build=0
    shift
fi

if [ "$build" = 1 ]; then
    # The build empties build-dir first and can take minutes from cold, so it
    # must not look hung: a silent wait is how it gets interrupted, and an
    # interrupted build leaves nothing for --no-build to run.
    echo "Building (a few minutes from cold, seconds when cached). Don't interrupt:" >&2
    echo "build-dir is emptied first. Full log: builddir-flatpak.log" >&2
    # --disable-rofiles-fuse: works whether or not FUSE is usable here.
    # Only the stage lines reach the terminal; everything goes to the log.
    # --state-dir: flatpak-builder keeps its cache in the *current* directory
    # by default, so running this from anywhere but the repo root would start a
    # second, cold cache there (a full rsync rebuild) and leave it behind.
    if ! flatpak-builder --user --force-clean --disable-rofiles-fuse \
        --state-dir="$HERE/.flatpak-builder" \
        "$BUILD_DIR" "$MANIFEST" 2>&1 | tee "$HERE/builddir-flatpak.log" \
        | grep --line-buffered -E '^(Building module|Cache hit|Starting build|Committing stage|Finishing|Pruning)' >&2
    then
        :   # grep's status says nothing about the build; the check below does
    fi
    if [ ! -x "$BUILD_DIR/files/bin/foresight" ]; then
        tail -n 40 "$HERE/builddir-flatpak.log" >&2
        echo "build failed — full log: builddir-flatpak.log" >&2
        exit 1
    fi
fi

if [ ! -x "$BUILD_DIR/files/bin/foresight" ]; then
    echo "No finished build in build-dir (never built, or a build was interrupted)." >&2
    echo "Run this again without --no-build." >&2
    exit 1
fi

# Every `  - --flag` under finish-args:, trailing comments dropped.
finish_args="$(awk '/^finish-args:/{p=1;next} p&&/^[^ #]/{p=0} p&&/^  - --/{print $2}' "$MANIFEST")"

[ "$#" -gt 0 ] || set -- foresight

# shellcheck disable=SC2086  # finish_args is a flag list, split on purpose
exec flatpak build --with-appdir --allow=devel \
    --talk-name='org.freedesktop.portal.*' --talk-name=org.a11y.Bus \
    --filesystem="/run/user/$(id -u)/doc" \
    $finish_args "$BUILD_DIR" "$@"
