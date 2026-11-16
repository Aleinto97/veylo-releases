#!/usr/bin/env bash
# Fetch libmpv for the Android mpv engine from a pinned mpv-android release:
# the arm64 libraries (libmpv, FFmpeg) the Rust player links, and its CA bundle for TLS.
# Nothing is committed:
# android/mpv/ is ignored; build.rs packages it when present.
# Licences: mpv-android MIT (LICENSE kept below); libmpv/FFmpeg as shipped by that release.
set -euo pipefail
TAG=2026-09-17
SHA256=0b0e0a534dd6b8d2b117f06952e0600064ad97702e48bff02543e3e32107803d
here=$(cd "$(dirname "$0")" && pwd)
out="$here/mpv"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/mpv.apk" "https://github.com/mpv-android/mpv-android/releases/download/$TAG/app-default-arm64-v8a-release.apk"
echo "$SHA256  $work/mpv.apk" | sha256sum -c -
mkdir -p "$out/arm64-v8a"
unzip -o -j -q "$work/mpv.apk" 'lib/arm64-v8a/*.so' -d "$out/arm64-v8a"
unzip -o -j -q "$work/mpv.apk" 'assets/cacert.pem' -d "$out"
raw="https://raw.githubusercontent.com/mpv-android/mpv-android/$TAG"
curl -fsSL -o "$out/LICENSE-mpv-android" "$raw/LICENSE"
echo "$TAG" > "$out/TAG"
ls -1 "$out" "$out/arm64-v8a"
