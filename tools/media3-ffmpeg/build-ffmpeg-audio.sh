#!/usr/bin/env bash
# Build FFmpeg 6.0 static libs (audio decoders only) for Android arm64-v8a using
# host clang + NDK sysroot (host is aarch64; NDK prebuilt x86_64 binaries cannot run).
set -euo pipefail
NDK="${ANDROID_NDK_HOME:-${ANDROID_HOME:-$HOME/Android/Sdk}/ndk/27.2.12479018}"
BASE="$(cd "$(dirname "$0")" && pwd)"
PREBUILT=$NDK/toolchains/llvm/prebuilt/linux-x86_64
SYSROOT=$PREBUILT/sysroot
SRC=$BASE/ffmpeg-src
API=${RUSTMEDIA_ANDROID_API:-31}
ABI=${VEYLO_ANDROID_ABI:-arm64-v8a}
case "$ABI" in
  arm64-v8a) ARCH=aarch64; CPU=armv8-a; TRIPLE=aarch64-linux-android; BUILTINS=aarch64 ;;
  armeabi-v7a) ARCH=arm; CPU=armv7-a; TRIPLE=armv7a-linux-androideabi; BUILTINS=arm ;;
  *) echo "Unsupported ABI: $ABI" >&2; exit 2 ;;
esac
TARGET_FLAGS="--target=${TRIPLE}${API} --sysroot=${SYSROOT}"

mkdir -p "$BASE/toolchain-shims/$ABI"
if [ ! -e "$BASE/toolchain-shims/$ABI/libgcc.a" ]; then
  ln -s "$PREBUILT/lib/clang/18/lib/linux/libclang_rt.builtins-${BUILTINS}-android.a" "$BASE/toolchain-shims/$ABI/libgcc.a"
fi
cd "$SRC"

# Audio decoders requested: aac ac3/eac3 dts(dca) truehd mlp flac opus vorbis mp3 pcm_* adpcm_* alac
DECODERS=$(./configure --list-decoders 2>/dev/null | tr ' ' '\n' \
  | grep -E '^(aac|aac_latm|ac3|eac3|dca|truehd|mlp|flac|opus|vorbis|mp3|alac|pcm_|adpcm_)' \
  | grep -v '_at$' | sort)
DECODER_FLAGS=""
for d in $DECODERS; do DECODER_FLAGS="$DECODER_FLAGS --enable-decoder=$d"; done
echo "Enabled decoders:"; echo "$DECODERS" | tr '\n' ' '; echo

./configure \
  --target-os=android \
  --arch="$ARCH" \
  --cpu="$CPU" \
  --enable-cross-compile \
  --cc="clang ${TARGET_FLAGS}" \
  --cxx="clang++ ${TARGET_FLAGS}" \
  --ar=llvm-ar --nm=llvm-nm --ranlib=llvm-ranlib --strip=llvm-strip \
  --sysroot="${SYSROOT}" \
  --extra-cflags="-fPIC -O2" \
  --extra-ldflags="-L${BASE}/toolchain-shims/$ABI" \
  --enable-static --disable-shared \
  --disable-doc --disable-programs --disable-everything \
  --disable-avdevice --disable-avformat --disable-swscale --disable-postproc --disable-avfilter \
  --disable-symver --enable-swresample --disable-autodetect --disable-iconv \
  --disable-v4l2-m2m --disable-vulkan \
  ${DECODER_FLAGS}

make -j"${VEYLO_BUILD_JOBS:-2}"
echo BUILD_OK
ls -la libavcodec/libavcodec.a libavutil/libavutil.a libswresample/libswresample.a
