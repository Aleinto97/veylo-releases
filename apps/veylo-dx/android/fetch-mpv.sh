#!/usr/bin/env bash
# Fetch libmpv for the Android mpv engine from a pinned mpv-android release:
# the arm64 and 32-bit ARM (TVs) libraries (libmpv, FFmpeg) the Rust player links, and its CA bundle for TLS.
# Nothing is committed:
# android/mpv/ is ignored; build.rs packages it when present.
# Licences: mpv-android MIT (LICENSE kept below); libmpv/FFmpeg as shipped by that release.
set -euo pipefail
TAG=2026-09-17
declare -A SHA256=(
    [arm64-v8a]=0b0e0a534dd6b8d2b117f06952e0600064ad97702e48bff02543e3e32107803d
    [armeabi-v7a]=30c63f5528530f56cdece5d6466c749976f01bdcd8c8e67ae76e70a54b7243aa
)
here=$(cd "$(dirname "$0")" && pwd)
out="$here/mpv"
if [ -f "$out/TAG" ] && [ "$(cat "$out/TAG")" = "$TAG" ] \
    && [ -f "$out/arm64-v8a/libmpv.so" ] && [ -f "$out/armeabi-v7a/libmpv.so" ] \
    && [ -f "$out/cacert.pem" ] && [ -f "$out/LICENSE-mpv-android" ]; then
    echo "Android libmpv $TAG already present."
    exit 0
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl_retry=(curl -fsSL --retry 5 --retry-delay 3 --retry-all-errors --connect-timeout 30 --max-time 300)
for abi in arm64-v8a armeabi-v7a; do
    "${curl_retry[@]}" -o "$work/$abi.apk" "https://github.com/mpv-android/mpv-android/releases/download/$TAG/app-default-$abi-release.apk"
    echo "${SHA256[$abi]}  $work/$abi.apk" | sha256sum -c -
    mkdir -p "$out/$abi"
    unzip -o -j -q "$work/$abi.apk" "lib/$abi/*.so" -d "$out/$abi"
done
unzip -o -j -q "$work/arm64-v8a.apk" 'assets/cacert.pem' -d "$out"
raw="https://raw.githubusercontent.com/mpv-android/mpv-android/$TAG"
"${curl_retry[@]}" -o "$out/LICENSE-mpv-android" "$raw/LICENSE"
echo "$TAG" > "$out/TAG"
ls -1 "$out" "$out/arm64-v8a" "$out/armeabi-v7a"
