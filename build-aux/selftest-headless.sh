#!/bin/sh
# Run the headless widget checks on a display nobody is looking at.
#
# The checks open real windows — some of them several, holding live rsync
# processes — and on the session's own display those windows appear in front
# of whoever is at the machine, and take focus. A Wayland client cannot choose
# which monitor or workspace it lands on, so rather than aim them somewhere
# else they are given a compositor of their own: mutter, headless, with one
# virtual monitor, on a private session bus. Nothing is drawn anywhere.
#
# CI does the same thing with xvfb-run; this is for a desktop that has mutter
# and no Xvfb, which is any GNOME one.
#
# Usage:  build-aux/selftest-headless.sh [--no-build]
#         FORESIGHT_GRESOURCE=/path/to/foresight.gresource build-aux/selftest-headless.sh
#
# Exit status is the selftest's: 0 when every check passed.
set -eu

HERE="$(cd "$(dirname "$0")/.." && pwd)"
GRESOURCE="${FORESIGHT_GRESOURCE:-$HERE/builddir/foresight.gresource}"

if [ "${1:-}" != "--no-build" ]; then
    (cd "$HERE" && cargo build --quiet --bin foresight --features selftest)
fi
[ -x "$HERE/target/debug/foresight" ] || {
    echo "no selftest binary — run without --no-build" >&2
    exit 1
}
[ -f "$GRESOURCE" ] || {
    echo "no compiled UI at $GRESOURCE" >&2
    echo "build it:  meson setup builddir && ninja -C builddir foresight.gresource" >&2
    exit 1
}
command -v mutter >/dev/null || {
    echo "mutter not found — on CI use xvfb-run instead (see .github/workflows/ci.yml)" >&2
    exit 1
}

# Under target/, so it is git-ignored and goes with `cargo clean`.
WORK="$(mktemp -d "$HERE/target/selftest.XXXXXX")"
mkdir -p "$WORK/st" "$WORK/cfg" "$WORK/run"
chmod 700 "$WORK/run"
# The private session bus starts a document portal and gvfs, which mount FUSE
# filesystems under the runtime dir and are still letting go of them when the
# session ends. So: unmount what is left, and give the removal a moment. None
# of this may decide the exit status — that belongs to the checks.
cleanup() {
    for mount in "$WORK/run/doc" "$WORK/run/gvfs"; do
        fusermount3 -uz "$mount" 2>/dev/null || true
    done
    n=0
    until rm -rf "$WORK" 2>/dev/null || [ "$n" -ge 30 ]; do
        n=$((n + 1))
        sleep 0.1
    done
}
trap cleanup EXIT

# A runtime dir of its own as well: the compositor's socket lives there, so it
# cannot collide with — or be mistaken for — the session's wayland-0.
export XDG_RUNTIME_DIR="$WORK/run"
export FORESIGHT_TARGET="$HERE/target/debug/foresight"
export FORESIGHT_GRESOURCE="$GRESOURCE"
export FORESIGHT_SELFTEST_DIR="$WORK/st"
export XDG_CONFIG_HOME="$WORK/cfg"
unset DISPLAY WAYLAND_DISPLAY

# No a11y bus on the private session bus; without this GTK warns about it on
# every run.
export GTK_A11Y=none

# Not `exec`: the trap above has to outlive the run to clean up after it.
status=0
# shellcheck disable=SC2016  # expanded by the inner shell, on purpose
dbus-run-session -- sh -c '
    mutter --headless --no-x11 --wayland-display=foresight-selftest \
        --virtual-monitor 1280x800 >"$XDG_RUNTIME_DIR/mutter.log" 2>&1 &
    compositor=$!
    # Wait for the socket rather than sleep: the checks must not start before
    # there is a display, and must not wait longer than it takes to make one.
    n=0
    while [ ! -S "$XDG_RUNTIME_DIR/foresight-selftest" ]; do
        n=$((n + 1))
        if [ "$n" -gt 100 ] || ! kill -0 "$compositor" 2>/dev/null; then
            echo "the headless compositor did not start:" >&2
            cat "$XDG_RUNTIME_DIR/mutter.log" >&2
            exit 1
        fi
        sleep 0.1
    done
    status=0
    GDK_BACKEND=wayland WAYLAND_DISPLAY=foresight-selftest "$FORESIGHT_TARGET" || status=$?
    kill "$compositor" 2>/dev/null
    wait "$compositor" 2>/dev/null
    exit "$status"
' || status=$?
exit "$status"
