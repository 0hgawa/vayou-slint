#!/usr/bin/env bash
# Debian package — the counterpart of the PKGBUILD, and it exists for the same
# reason that file gives: a tarball cannot declare what it needs, so whoever
# unpacks one has to know to install libmpv by hand. `Depends:` is that
# knowledge, written where apt reads it.
#
#   installer/build-deb.sh                  # after cargo build --release
#   installer/build-deb.sh path/to/vayou    # or point it at a binary
#
# Writes vayou_<version>_amd64.deb beside the repository root.
#
# The dependency list is NOT hand-written. `dpkg-shlibdeps` reads the binary's
# DT_NEEDED entries and asks the local apt which package owns each library, so
# the list cannot drift when a crate adds or drops a native dependency — which
# is exactly how the PKGBUILD's list would rot if nobody re-ran `readelf`. Two
# entries have to be added by hand because no linker records them:
#
#   libmpv2  — resolved with dlopen at runtime (see mpv::ffi), never linked.
#              Version 2 specifically: the loader asks for libmpv.so.2, which
#              means Debian 13 / Ubuntu 24.04 and newer. Debian 12 carries
#              soname 1 and cannot satisfy this.
#   ffmpeg   — a separate process the subtitle extraction runs, not a library.
#
# Build host matters: shlibdeps emits versioned dependencies taken from the
# machine it runs on, so building on the oldest distribution you intend to
# support is what keeps the package installable there. CI uses ubuntu-latest,
# whose glibc is older than Debian stable's — the resulting floor is satisfied
# by both.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"
cd "$root"

bin="${1:-target/release/vayou}"
[[ -f "$bin" ]] || { echo "error: no binary at $bin — run cargo build --release first." >&2; exit 1; }
# Absolute from here on: shlibdeps runs from a scratch directory of its own, so
# a path relative to the repository would not resolve once we are there.
bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"

command -v dpkg-deb >/dev/null || { echo "error: dpkg-deb not found (apt install dpkg-dev)." >&2; exit 1; }
command -v dpkg-shlibdeps >/dev/null || { echo "error: dpkg-shlibdeps not found (apt install dpkg-dev)." >&2; exit 1; }

version="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

# The same layout the PKGBUILD's package() produces, so a file lands in the same
# place whichever package manager put it there.
install -Dm755 "$bin"             "$stage/usr/bin/vayou"
install -Dm644 assets/icon.svg    "$stage/usr/share/icons/hicolor/scalable/apps/vayou.svg"
install -Dm644 LICENSE            "$stage/usr/share/doc/vayou/copyright"
# The entry carries @BINDIR@ rather than a path, so one file serves this package,
# install-linux.sh, the PKGBUILD and the Flatpak build — each fills in its own.
sed 's|@BINDIR@|/usr/bin|' installer/vayou.desktop |
  install -Dm644 /dev/stdin "$stage/usr/share/applications/vayou.desktop"

# dpkg-shlibdeps insists on a debian/control existing before it will run, even
# with -O, which prints the answer to stdout instead of writing substvars.
shlibs_dir="$(mktemp -d)"
trap 'rm -rf "$stage" "$shlibs_dir"' EXIT
mkdir -p "$shlibs_dir/debian"
printf 'Source: vayou\n' > "$shlibs_dir/debian/control"
# `$bin` is already absolute; stderr is left alone on purpose, because the one
# thing worse than this step failing is it failing without saying why.
linked="$(cd "$shlibs_dir" && dpkg-shlibdeps -O --ignore-missing-info "$bin" | sed 's/^shlibs:Depends=//')" ||
  { echo "error: dpkg-shlibdeps failed against $bin (see its output above)." >&2; exit 1; }
[[ -n "$linked" ]] || { echo "error: dpkg-shlibdeps produced no dependencies — refusing to ship a package that declares none." >&2; exit 1; }

# Kibibytes of installed content, which is what the field means. Package
# managers show it before downloading; omitting it makes apt report nothing.
installed_size="$(du -ks "$stage" | cut -f1)"

install -Dm644 /dev/stdin "$stage/DEBIAN/control" <<CONTROL
Package: vayou
Version: $version
Architecture: amd64
Maintainer: Ohgawa <https://github.com/0hgawa>
Installed-Size: $installed_size
Depends: $linked, libmpv2, ffmpeg
Section: video
Priority: optional
Homepage: https://github.com/0hgawa/vayou-slint
Description: Native video player with libmpv and a Slint interface
 A single frameless window: mpv renders the video through its render API as an
 OpenGL underlay and the interface is drawn on top of it, so there is no second
 window and no embedded browser.
 .
 Subtitles are the reason it exists — embedded and external tracks, an
 OpenSubtitles search, and machine translation into twelve languages that keeps
 the styling of the track it came from.
CONTROL

out="vayou_${version}_amd64.deb"
# xz at its highest level: the binary is the whole package, it compresses well,
# and a release artefact is written once and downloaded many times.
dpkg-deb --build --root-owner-group -Zxz -z9 "$stage" "$out" >/dev/null

echo "$out  ($(du -h "$out" | cut -f1))"
echo "Depends: $linked, libmpv2, ffmpeg"
