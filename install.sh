#!/bin/sh
# Veylo for macOS (Apple Silicon): install, or update to, the latest release. In Terminal:
#   curl -fsSL https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.sh | sh
# Installs Veylo.app to /Applications (or ~/Applications when that isn't writable) and opens it.
# Later versions arrive through the Update button inside Veylo.
# Testing: VEYLO_ZIP=<local zip> installs that file; VEYLO_NO_LAUNCH=1 doesn't open the app.
set -eu
repo=Aleinto97/veylo-releases
asset=veylo-macos-arm64.zip

fail() { echo "$*" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || fail "This installer is for macOS. On Windows use install.ps1; on Linux build Veylo from source (see the README)."
[ "$(uname -m)" = arm64 ] || fail "Veylo for macOS needs a Mac with Apple silicon (M1 or later)."

dest=${VEYLO_INSTALL_DIR:-/Applications}
if [ ! -w "$dest" ]; then dest="$HOME/Applications"; mkdir -p "$dest"; fi
app="$dest/Veylo.app"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -n "${VEYLO_ZIP:-}" ]; then
    cp "$VEYLO_ZIP" "$work/$asset"
else
    echo "Downloading Veylo..."
    base="https://github.com/$repo/releases/latest/download"
    curl -fL --progress-bar -o "$work/$asset" "$base/$asset" \
        || fail "The latest Veylo release has no macOS download yet."
    curl -fsSL -o "$work/SHA256SUMS.txt" "$base/SHA256SUMS.txt" \
        || fail "Refusing to install: couldn't download SHA256SUMS.txt to verify the release."
    expected=$(awk -v f="$asset" '{gsub(/^\*/, "", $2); if ($2 == f) print tolower($1)}' "$work/SHA256SUMS.txt" | head -1)
    [ -n "$expected" ] || fail "Refusing to install: $asset is missing from SHA256SUMS.txt."
    actual=$(shasum -a 256 "$work/$asset" | awk '{print tolower($1)}')
    [ "$actual" = "$expected" ] || fail "Refusing to install: SHA-256 verification failed for $asset."
fi
ditto -x -k "$work/$asset" "$work/unpacked"
[ -d "$work/unpacked/Veylo.app" ] || fail "The download isn't a Veylo app."

# A running Veylo from this folder holds its files: close it first.
pkill -f "$app/Contents/MacOS/veylo-dx" 2>/dev/null && sleep 1 || true
rm -rf "$app"
mv "$work/unpacked/Veylo.app" "$app"
xattr -dr com.apple.quarantine "$app" 2>/dev/null || true

version=$(defaults read "$app/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || echo "")
echo "Veylo $version is installed in $dest."
if [ -z "${VEYLO_NO_LAUNCH:-}" ]; then
    echo "Opening it now; next time open it from Launchpad, Spotlight or $dest."
    open "$app"
fi
