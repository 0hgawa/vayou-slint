#!/usr/bin/env bash
#
# Install Vayou for the current user — the Linux counterpart of the per-user
# Windows installer, and per-user for the same reason: no root, nothing outside
# $HOME, and an uninstall that is three `rm`s.
#
# It runs from either place it ships: unpacked from the release tarball, where
# it is named install.sh and sits beside the binary, or in the source tree
# against a build.
#
#   ./install.sh                            # from the unpacked tarball
#   installer/install-linux.sh              # from a checkout (build it first)
#   install.sh --uninstall                  # remove exactly what was added
#
# Everything lands under the XDG base directories, so the desktop picks the app
# up without a session restart.
#
# libmpv and ffmpeg are NOT installed here. They are distribution packages the
# app resolves at runtime; this script only checks for them and says what to
# install if either is missing.
set -euo pipefail

PREFIX="${XDG_DATA_HOME:-$HOME/.local/share}"
BINDIR="$HOME/.local/bin"
APPDIR="$PREFIX/applications"
ICONDIR="$PREFIX/icons/hicolor/scalable/apps"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

say() { printf '  %s\n' "$1"; }

# The desktop and icon caches are advisory: a missing tool means the entry shows
# up a little later rather than not at all, so neither is worth failing over.
refresh_caches() {
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPDIR" 2>/dev/null || true
    command -v gtk-update-icon-cache   >/dev/null && gtk-update-icon-cache -qtf "$PREFIX/icons/hicolor" 2>/dev/null || true
}

uninstall() {
    rm -f "$BINDIR/vayou" "$APPDIR/vayou.desktop" "$ICONDIR/vayou.svg"
    refresh_caches
    say "removed. Settings in ${XDG_CONFIG_HOME:-$HOME/.config}/Vayou were left alone."
    exit 0
}

[[ "${1:-}" == "--uninstall" ]] && uninstall

# Two layouts, one script: the release tarball, where the binary and the icon
# are unpacked beside it, and the source tree, where both are build inputs a
# directory up. The tarball's copies come first — extracted into a checkout that
# also has a stale `target/`, what the user downloaded is what they meant to
# install.
BIN="$HERE/vayou"
[[ -x "$BIN" ]] || BIN="$ROOT/target/release/vayou"
[[ -x "$BIN" ]] || BIN="$ROOT/target/debug/vayou"
[[ -x "$BIN" ]] || { echo "error: no vayou binary beside this script or under $ROOT/target — run 'cargo build --release' first." >&2; exit 1; }

ICON="$HERE/vayou.svg"
[[ -f "$ICON" ]] || ICON="$ROOT/assets/icon.svg"
[[ -f "$ICON" ]] || { echo "error: no icon beside this script or at $ROOT/assets/icon.svg." >&2; exit 1; }

install -Dm755 "$BIN"  "$BINDIR/vayou"
install -Dm644 "$ICON" "$ICONDIR/vayou.svg"

# The entry ships with a placeholder rather than a hardcoded path, so the same
# file serves this installer and the Flatpak build, which substitutes its own.
mkdir -p "$APPDIR"
sed "s|@BINDIR@|$BINDIR|" "$HERE/vayou.desktop" > "$APPDIR/vayou.desktop"
chmod 644 "$APPDIR/vayou.desktop"

refresh_caches

say "installed:"
say "  $BINDIR/vayou"
say "  $APPDIR/vayou.desktop"
say "  $ICONDIR/vayou.svg"

# Both are dlopen'd or shelled out to at runtime, so a missing one is not a
# startup error the user would recognise — it is an empty window, or subtitle
# extraction that never finishes. Say it here, where it can still be acted on.
missing=()
# `grep -c`, not `grep -q`: the latter exits at the first match, ldconfig takes a
# SIGPIPE for it, and `pipefail` then reports the whole pipeline as failed — so
# the check fires on a machine that has libmpv installed all along.
if [[ "$(ldconfig -p 2>/dev/null | grep -c 'libmpv\.so')" -eq 0 ]]; then missing+=("libmpv"); fi
command -v ffmpeg >/dev/null || missing+=("ffmpeg")
if (( ${#missing[@]} )); then
    echo
    say "missing at runtime: ${missing[*]}"
    say "  Arch: pacman -S mpv ffmpeg"
    say "  Debian/Ubuntu: apt install libmpv2 ffmpeg"
    say "  Fedora: dnf install mpv-libs ffmpeg"
fi

case ":$PATH:" in
    *":$BINDIR:"*) ;;
    *) echo; say "note: $BINDIR is not on your PATH — the desktop entry works regardless." ;;
esac
