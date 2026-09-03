#!/usr/bin/env bash
#
# Build, sign and package the Linux release — the counterpart of build.ps1, and
# deliberately the same shape: build, sign the bare binary, emit latest.json.
#
#   installer/build-linux.sh [-k SECRET_KEY] [-r OWNER/REPO]
#
# The self-updater downloads and swaps ONLY the `vayou` binary, verified against
# the minisign key embedded in the app (src/update.rs UPDATE_PUBKEY). That is
# why the binary is signed on its own and uploaded under a name that does not
# change between releases: the feed points at
# releases/latest/download/vayou. The tarball is for first-time installs; it
# carries install.sh, the desktop entry and the icon alongside the binary.
#
# libmpv and ffmpeg are NOT bundled. They are distribution packages the app
# resolves at runtime, which is why a libmpv bump needs no new release here —
# unlike Windows, where it ships a fresh installer.
#
# Signing uses rsign2 (`cargo install rsign2`) with the SAME key as the Windows
# release, so both platforms verify against the one public key in the app.
set -euo pipefail

REPO_SLUG="0hgawa/vayou-slint"
KEY="${VAYOU_SIGN_KEY:-$HOME/.keys/vayou.key}"

while getopts ":k:r:h" opt; do
    case "$opt" in
        k) KEY="$OPTARG" ;;
        r) REPO_SLUG="$OPTARG" ;;
        h) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "usage: $0 [-k SECRET_KEY] [-r OWNER/REPO]" >&2; exit 2 ;;
    esac
done

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
BIN="$ROOT/target/release/vayou"
SIG="$BIN.minisig"
MANIFEST="$HERE/latest.json"

die() { echo "error: $1" >&2; exit 1; }

# Fail before the long build rather than after it: a missing signer or key means
# the release cannot be finished either way.
command -v cargo   >/dev/null || die "cargo not found on PATH."
command -v rsign   >/dev/null || die "rsign not found on PATH. Install it: 'cargo install rsign2'."
command -v python3 >/dev/null || die "python3 not found on PATH (needed to write latest.json)."
[[ -f "$KEY" ]]               || die "secret key not found at $KEY (override with -k, or \$VAYOU_SIGN_KEY)."

# Single source of truth for the version, read from [package] specifically so a
# dependency's `version =` can never be picked up instead.
VERSION="$(sed -n '/^\[package\]/,/^\[[^p]/{s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p}' "$ROOT/Cargo.toml" | head -1)"
[[ -n "$VERSION" ]] || die "couldn't read [package] version from Cargo.toml"

echo "Building release v$VERSION..."
cargo build --release --manifest-path "$ROOT/Cargo.toml"
[[ -x "$BIN" ]] || die "$BIN not found after build."

# 1. Sign the bare binary — this is what the self-updater verifies.
#    -W: the key was generated passwordless, so signing is non-interactive.
rm -f "$SIG"
rsign sign -W -s "$KEY" -x "$SIG" "$BIN"
echo "Signed:   $SIG"

# 2. Tarball for first-time installs. install.sh is the same script the source
#    tree uses, renamed to the name the README tells people to run.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
PKGDIR="$STAGE/vayou-$VERSION"
mkdir -p "$PKGDIR"
install -Dm755 "$BIN"                    "$PKGDIR/vayou"
install -Dm755 "$HERE/install-linux.sh"  "$PKGDIR/install.sh"
install -Dm644 "$HERE/vayou.desktop"     "$PKGDIR/vayou.desktop"
install -Dm644 "$ROOT/assets/icon.svg"   "$PKGDIR/vayou.svg"
TARBALL="$ROOT/target/release/vayou-$VERSION-linux-x86_64.tar.gz"
tar -czf "$TARBALL" -C "$STAGE" "vayou-$VERSION"
echo "Packaged: $TARBALL"

# 3. Write this platform's entry into the shared feed, preserving whatever the
#    Windows release left there. The two builds run on different machines, so
#    the manifest is carried between them and each script completes the other's
#    half rather than overwriting it.
python3 - "$MANIFEST" "$VERSION" "$REPO_SLUG" "$SIG" <<'PY'
import json, pathlib, sys, datetime

manifest, version, repo, sig_path = sys.argv[1:5]
p = pathlib.Path(manifest)
data = json.loads(p.read_text()) if p.exists() else {}
platforms = data.get("platforms", {})

platforms["linux-x86_64"] = {
    "url": f"https://github.com/{repo}/releases/latest/download/vayou",
    "signature": pathlib.Path(sig_path).read_text(),
}
data["version"] = version
data["pub_date"] = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
data["platforms"] = platforms
p.write_text(json.dumps(data, indent=4) + "\n")
print("Manifest: " + manifest + "  (platforms: " + ", ".join(sorted(platforms)) + ")")
PY

cat <<EOF

Upload to the v$VERSION release, with these exact names:
  vayou                                   (the updater downloads this one)
  $(basename "$TARBALL")
  latest.json

A missing 'windows-x86_64' above means this manifest has not met the Windows
build yet — carry it over and run build.ps1, or the Windows updater will read
this release as having nothing for it.
EOF
