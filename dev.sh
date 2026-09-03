#!/usr/bin/env bash
#
# Build and run the debug build, teeing everything to dev.log — the Linux
# counterpart of dev.bat, and the same idea: one command that leaves a log
# worth reading behind.
#
#   ./dev.sh                 # opens with no file
#   ./dev.sh video.mkv       # opens that file, as a double-click would
#
# Two differences from the Windows script, both because the platform removes
# the problem rather than because the script is simpler: libmpv and ffmpeg come
# from the distribution instead of binaries/, so there is no PATH to arrange,
# and the log level is the only thing left to set.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
LOG=dev.log

# Verbose by default: the point of this script is diagnosing a running app, and
# at the default (warn) the log shows almost nothing. Values: trace, debug,
# info, error. Override with:  VAYOU_LOG=info ./dev.sh
export VAYOU_LOG="${VAYOU_LOG:-debug}"

# rustup installs into ~/.cargo/bin, which is only on PATH once the shell
# profile has been sourced — and this script is often the first thing run in a
# fresh terminal.
if ! command -v cargo >/dev/null && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null || { echo "cargo not found — install rustup first." >&2; exit 1; }

# libmpv is dlopen'd at runtime, so a missing one is not a link error: the app
# starts, fails to load mpv and shows an empty window. Say so up front.
#
# `grep -c` rather than `grep -q`: the latter exits at the first match, ldconfig
# takes a SIGPIPE for it, and `pipefail` then reports the pipeline as failed —
# so the check fired on a machine that had libmpv installed all along.
if [[ "$(ldconfig -p 2>/dev/null | grep -c 'libmpv\.so')" -eq 0 ]]; then
    echo
    echo "  libmpv is not installed — the app will start and fail to load mpv."
    echo "  Arch: pacman -S mpv · Debian/Ubuntu: apt install libmpv2 · Fedora: dnf install mpv-libs"
    echo
fi

echo "=== Vayou (Slint) dev ==="
echo "Log file:   $PWD/$LOG"
echo "VAYOU_LOG:  $VAYOU_LOG"
echo

cargo run -- "$@" 2>&1 | tee "$LOG"
ERR=${PIPESTATUS[0]}

echo
echo "==================================="
echo "Exit code: $ERR"
echo "Full log:  $PWD/$LOG"
echo "==================================="
exit "$ERR"
