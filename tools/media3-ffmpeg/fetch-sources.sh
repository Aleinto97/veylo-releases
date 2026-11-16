#!/usr/bin/env bash
# Recreate pinned, verified build inputs; never silently use a different cached JNI API.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
readarray -t PIN < <(python3 - "$HERE/sources.json" <<'PY'
import json,sys
p=json.load(open(sys.argv[1]))
for k in ['ffmpeg','androidx_media_tag','ffmpeg_archive_sha256']: print(p[k])
PY
)
FFMPEG_VERSION=${PIN[0]}
MEDIA3_TAG=${PIN[1]}
if [ ! -d "$HERE/ffmpeg-src" ]; then
  archive="$HERE/ffmpeg-${FFMPEG_VERSION}.tar.xz"
  curl -4 --connect-timeout 15 --max-time 180 -fL "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" -o "$archive"
  printf '%s  %s\n' "${PIN[2]}" "$archive" | sha256sum --check -
  tar -xf "$archive" -C "$HERE"
  mv "$HERE/ffmpeg-${FFMPEG_VERSION}" "$HERE/ffmpeg-src"
fi
if [ "$(cat "$HERE/ffmpeg-src/RELEASE")" != "$FFMPEG_VERSION" ]; then
  echo 'Cached FFmpeg sources do not match sources.json; move that cache before fetching.' >&2
  exit 1
fi
if [ ! -d "$HERE/androidx-media" ]; then
  git clone --depth 1 --branch "$MEDIA3_TAG" https://github.com/androidx/media "$HERE/androidx-media"
fi
if [ "$(git -C "$HERE/androidx-media" describe --tags --exact-match 2>/dev/null || true)" != "$MEDIA3_TAG" ]; then
  if [ -n "$(git -C "$HERE/androidx-media" status --porcelain)" ]; then
    echo 'Cached AndroidX sources contain changes; preserve them before fetching another version.' >&2
    exit 1
  fi
  git -C "$HERE/androidx-media" fetch --depth 1 origin tag "$MEDIA3_TAG"
  git -C "$HERE/androidx-media" checkout --detach "$MEDIA3_TAG"
fi
printf 'sources ready: ffmpeg %s, androidx/media %s\n' "$FFMPEG_VERSION" "$MEDIA3_TAG"
