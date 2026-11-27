#!/usr/bin/env bash
# Generates the player test matrix: synthetic test patterns (testsrc2 + tones, no real media) in the
# codecs, containers, resolutions, bit depths and audio formats Veylo meets in the wild.
# Writes <out>/<name>.<ext> and <out>/manifest.json. Clips whose encoder this ffmpeg lacks are
# skipped and listed in the manifest as "skipped" (CI generates them once, on Linux, for every OS).
#   tool/make-test-clips.sh [out-dir] [seconds]
set -euo pipefail
out="${1:-build/test-clips}"; secs="${2:-5}"
mkdir -p "$out"
enc="$(ffmpeg -hide_banner -encoders 2>/dev/null)"
has() { grep -qE "^ [A-Z.]{6} $1 " <<<"$enc"; }
entries=(); skipped=()

# name ext vcodec-args audio-args width height fps label-json-extras
clip() {
  local name=$1 ext=$2 venc=$3 vargs=$4 aenc=$5 aargs=$6 w=$7 h=$8 fps=$9 extra=${10:-}
  if ! has "$venc" || ! has "$aenc"; then skipped+=("\"$name\""); echo "skip $name (no $venc/$aenc)"; return; fi
  # A 10-bit clip from an 8-bit-only encoder build would silently be 8-bit: skip it instead.
  if [[ "$vargs" == *yuv420p10le* ]] && ! ffmpeg -hide_banner -h "encoder=$venc" 2>/dev/null | grep -q yuv420p10le; then
    skipped+=("\"$name\""); echo "skip $name ($venc has no 10-bit)"; return
  fi
  local f="$out/$name.$ext"
  if [[ ! -s "$f" ]]; then
    echo "make $name"
    # shellcheck disable=SC2086
    ffmpeg -hide_banner -loglevel error -y \
      -f lavfi -i "testsrc2=size=${w}x${h}:rate=${fps}:duration=${secs}" \
      -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=${secs}" \
      -c:v "$venc" $vargs -c:a "$aenc" $aargs -shortest "$f"
  fi
  entries+=("{\"name\":\"$name\",\"file\":\"$name.$ext\",\"vcodec\":\"$venc\",\"acodec\":\"$aenc\",\"width\":$w,\"height\":$h,\"fps\":$fps${extra:+,$extra}}")
}

x264="-preset veryfast -pix_fmt yuv420p -b:v 4M"
stereo_aac="-b:a 160k -ac 2"
# H.264 across sizes and shapes (the most common stream format).
clip h264-240p      mp4 libx264 "$x264" aac "$stereo_aac" 426 240 30
clip h264-720p      mp4 libx264 "$x264" aac "$stereo_aac" 1280 720 30
clip h264-1080p     mp4 libx264 "$x264" aac "$stereo_aac" 1920 1080 30
clip h264-1080p60   mkv libx264 "$x264" aac "$stereo_aac" 1920 1080 60
clip h264-2160p     mp4 libx264 "-preset ultrafast -pix_fmt yuv420p -b:v 16M" aac "$stereo_aac" 3840 2160 30
clip h264-portrait  mp4 libx264 "$x264" aac "$stereo_aac" 1080 1920 30
clip h264-ultrawide mp4 libx264 "$x264" aac "$stereo_aac" 2560 1080 30
clip h264-odd-size  mkv libx264 "$x264" aac "$stereo_aac" 1278 534 24 '"note":"odd anamorphic-like size"'
# Surround / lossless / DTS audio on H.264 video.
clip h264-ac3-51    mkv libx264 "$x264" ac3  "-b:a 448k -ac 6" 1920 1080 24
clip h264-eac3-51   mkv libx264 "$x264" eac3 "-b:a 640k -ac 6" 1920 1080 24
clip h264-dts       mkv libx264 "$x264" dca  "-strict -2 -b:a 768k -ac 2" 1920 1080 24
clip h264-flac      mkv libx264 "$x264" flac "-ac 2" 1920 1080 24
# HEVC: 8-bit, 10-bit, HDR10 4K (what high-quality releases use).
x265="-preset ultrafast -x265-params log-level=error"
clip hevc-1080p      mkv libx265 "$x265 -pix_fmt yuv420p -b:v 3M" aac "$stereo_aac" 1920 1080 24
clip hevc-10bit-1080p mkv libx265 "$x265 -pix_fmt yuv420p10le -b:v 3M" eac3 "-b:a 640k -ac 6" 1920 1080 24
clip hevc-hdr10-2160p mkv libx265 "-preset ultrafast -pix_fmt yuv420p10le -b:v 12M -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -x265-params log-level=error:hdr10=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400" eac3 "-b:a 640k -ac 6" 3840 2160 24 '"hdr":"hdr10"'
clip hevc-mp4-1080p  mp4 libx265 "$x265 -pix_fmt yuv420p -tag:v hvc1 -b:v 3M" aac "$stereo_aac" 1920 1080 30
# VP9 / AV1 (web/YouTube-style and modern releases).
clip vp9-1080p  webm libvpx-vp9 "-deadline realtime -cpu-used 8 -row-mt 1 -b:v 3M" libopus "-b:a 128k" 1920 1080 30
clip vp9-2160p  webm libvpx-vp9 "-deadline realtime -cpu-used 8 -row-mt 1 -b:v 12M" libopus "-b:a 128k" 3840 2160 30
if has libsvtav1; then av1=libsvtav1; av1args="-preset 12 -b:v 3M"; else av1=libaom-av1; av1args="-cpu-used 8 -row-mt 1 -b:v 3M"; fi
clip av1-1080p       mkv  "$av1" "$av1args -pix_fmt yuv420p" libopus "-b:a 128k" 1920 1080 30
clip av1-10bit-1080p mkv  "$av1" "$av1args -pix_fmt yuv420p10le" libopus "-b:a 128k" 1920 1080 24
# Legacy broadcast/DVD.
clip mpeg2-576i ts mpeg2video "-b:v 6M -flags +ilme+ildct -field_order tt" ac3 "-b:a 192k -ac 2" 720 576 25 '"interlaced":true'

{
  printf '{"seconds":%s,"clips":[\n' "$secs"
  ( IFS=,; printf '%s' "${entries[*]}" ) | sed 's/},{/},\n{/g'
  printf '\n],"skipped":[%s]}\n' "$( IFS=,; printf '%s' "${skipped[*]:-}" )"
} > "$out/manifest.json"
echo "wrote ${#entries[@]} clips to $out (${#skipped[@]} skipped)"
