#!/usr/bin/env bash
# Compile Media3 1.11.1 ffmpeg_jni.cc + static FFmpeg into libmedia3ffmpeg.so
# (arm64-v8a, Android API 32) using host clang + NDK sysroot + lld.
set -euo pipefail
NDK="${ANDROID_NDK_HOME:-${ANDROID_HOME:-$HOME/Android/Sdk}/ndk/27.2.12479018}"
BASE="$(cd "$(dirname "$0")" && pwd)"
PREBUILT=$NDK/toolchains/llvm/prebuilt/linux-x86_64
SYSROOT=$PREBUILT/sysroot
API=${RUSTMEDIA_ANDROID_API:-31}
ABI=${VEYLO_ANDROID_ABI:-arm64-v8a}
case "$ABI" in
  arm64-v8a) TRIPLE=aarch64-linux-android; NDK_TRIPLE=aarch64-linux-android; BUILTINS=aarch64 ;;
  armeabi-v7a) TRIPLE=armv7a-linux-androideabi; NDK_TRIPLE=arm-linux-androideabi; BUILTINS=arm ;;
  *) echo "Unsupported ABI: $ABI" >&2; exit 2 ;;
esac
LIBDIR=$SYSROOT/usr/lib/$NDK_TRIPLE/$API
FFSRC=$BASE/ffmpeg-src
GLUE=$BASE/androidx-media/libraries/decoder_ffmpeg/src/main/jni/ffmpeg_jni.cc
OUT=$BASE/build-jni/$ABI
TFLAGS="--target=${TRIPLE}${API} --sysroot=${SYSROOT}"
mkdir -p "$OUT"

# 1. Compile the JNI glue (C++11, PIC).
clang++ $TFLAGS -std=c++11 -fPIC -O2 -fvisibility=hidden \
  -I"$FFSRC" \
  -c "$GLUE" -o "$OUT/ffmpeg_jni.o"

# 2. Link shared library. Same recipe as scripts/link-android.sh (verified working),
#    plus the -Wl,-Bsymbolic that media3's CMakeLists adds for arm64-v8a.
clang $TFLAGS -fuse-ld=lld -nostartfiles \
  -L"$LIBDIR" \
  -L${BASE}/toolchain-shims/$ABI \
  "$LIBDIR/crtbegin_so.o" \
  "$OUT/ffmpeg_jni.o" \
  "$FFSRC/libavcodec/libavcodec.a" "$FFSRC/libswresample/libswresample.a" "$FFSRC/libavutil/libavutil.a" \
  -llog -landroid -lc -lm -ldl \
  "$LIBDIR/crtend_so.o" \
  "$PREBUILT/lib/clang/18/lib/linux/libclang_rt.builtins-${BUILTINS}-android.a" \
  -shared -o "$OUT/libmedia3ffmpeg.so" \
  -Wl,-soname,libmedia3ffmpeg.so \
  -Wl,-Bsymbolic \
  -Wl,-z,max-page-size=16384

# 3. Media3 1.11.1 FfmpegLibrary loads "ffmpegJNI" by default -> also ship that name.
llvm-strip --strip-debug "$OUT/libmedia3ffmpeg.so"
cp "$OUT/libmedia3ffmpeg.so" "$OUT/libffmpegJNI.so"

echo GLUE_OK
