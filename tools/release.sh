#!/usr/bin/env bash
# Build Veylo for Windows (cargo-xwin) and Android (dx) on Linux and publish them, with the macOS app
# built on a Mac (tools/build-macos.sh, passed in through VEYLO_EXTRA_ASSETS), as a release of the public repo, where people install them and the app finds updates (apps/veylo-dx/src/updater.rs).
#
#   tools/release.sh ["notes"]            build, then publish v<version> (gh logged in, or GH_TOKEN)
#   VEYLO_PUBLISH=0 tools/release.sh      build only; the files stay in dist/ (or VEYLO_OUT)
#
# GitHub Actions runs this on every push to main that raises `version` in apps/veylo-dx/Cargo.toml
# (.github/workflows/release.yml); it runs the same way here.
# Needs: cargo-xwin + the x86_64-pc-windows-msvc target, dx 0.7.10 + the aarch64-linux-android target,
# the Android SDK (build-tools, platform 36) and NDK 28, JDK 17, bsdtar, zip, curl.
#
# Signing: every APK is signed with Veylo's release key, never kept in the repo. Android only
# updates an installed app from an APK signed with the same key, so it must never change:
#   VEYLO_KEYSTORE            the keystore (default ~/.config/veylo/release.jks, alias "veylo")
#   VEYLO_KEYSTORE_PASS       its password, or VEYLO_KEYSTORE_PASS_FILE (default ~/.config/veylo/release.pass)
# Uploaded: veylo-dx.exe + libmpv-2.dll (zip), the APK, and every file in VEYLO_EXTRA_ASSETS (a folder,
# e.g. veylo-macos-arm64.zip); all pass the privacy check below. Never settings, keys, local paths or source.
set -euo pipefail
REPO=${VEYLO_REPO:-Aleinto97/veylo-releases}
WIN_ASSET=veylo-windows-x64.zip
APK_ASSET=veylo-android-arm64.apk
publish=${VEYLO_PUBLISH:-1}
root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/apps/veylo-dx"
out_dir=${VEYLO_OUT:-$root/dist}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$app/Cargo.toml" | head -1)
tag="v$version"
notes=${1:-"Veylo $version for Windows, macOS and Android."}
extra=()
if [ -n "${VEYLO_EXTRA_ASSETS:-}" ]; then
    for f in "$VEYLO_EXTRA_ASSETS"/*; do [ -f "$f" ] && extra+=("$f"); done
fi
if [ "$publish" = 1 ] && gh release view "$tag" --repo "$REPO" >/dev/null 2>&1; then
    echo "$tag is already released: bump version in apps/veylo-dx/Cargo.toml first." >&2; exit 1
fi
IFS=. read -r major minor patch <<< "$version"
version_code=$((major * 10000 + minor * 100 + patch))

keystore=${VEYLO_KEYSTORE:-$HOME/.config/veylo/release.jks}
[ -f "$keystore" ] || { echo "No release key at $keystore (VEYLO_KEYSTORE): without it the APK can't update installed copies." >&2; exit 1; }
work=$(mktemp -d)
stage="$work/stage"
trap 'rm -rf "$work"' EXIT
mkdir -p "$stage"
pass_file="$work/keystore.pass"
( umask 077
  if [ -n "${VEYLO_KEYSTORE_PASS:-}" ]; then printf '%s' "$VEYLO_KEYSTORE_PASS" > "$pass_file"
  else cp "${VEYLO_KEYSTORE_PASS_FILE:-$HOME/.config/veylo/release.pass}" "$pass_file"; fi )
# No local folder names (your home path, so your username) inside the binaries.
remap="--remap-path-prefix=$HOME=~ --remap-path-prefix=$root=veylo"

# Model assets and native runtimes are bundled: classification never downloads at runtime.
python3 "$root/tools/picks-ai/fetch.py" windows-x64 android-arm64

# ---------------------------------------------------------------- Windows
mpv="$app/windows/mpv"
if [ ! -f "$mpv/libmpv-2.dll" ]; then
    pin() { tr -d '\r' < "$app/windows/fetch-mpv.ps1" | sed -n "s/^\$$1 = '\(.*\)'/\1/p"; }
    curl -fsSL -o "$work/mpv.7z" "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$(pin Tag)/$(pin Asset)"
    echo "$(pin Sha256)  $work/mpv.7z" | sha256sum -c -
    mkdir -p "$mpv" && bsdtar -xf "$work/mpv.7z" -C "$mpv" && cp "$mpv/libmpv.dll.a" "$mpv/mpv.lib" && pin Tag > "$mpv/TAG"
fi
cd "$app"
# Static C runtime (as .cargo/config.toml).
RUSTFLAGS="-C target-feature=+crt-static $remap" cargo xwin build --release --target x86_64-pc-windows-msvc
win_out="$app/target/x86_64-pc-windows-msvc/release"
mkdir -p "$work/win" && cp "$win_out/veylo-dx.exe" "$win_out/libmpv-2.dll" "$work/win/"
cp -r "$win_out/picks-ai" "$work/win/"
(cd "$work/win" && zip -q -r -9 "$stage/$WIN_ASSET" veylo-dx.exe libmpv-2.dll picks-ai)

# ---------------------------------------------------------------- Android
export ANDROID_HOME=${ANDROID_HOME:-$HOME/Android/Sdk}
if [ -z "${JAVA_HOME:-}" ] && [ -d "$HOME/Android/jdk17" ]; then export JAVA_HOME=$HOME/Android/jdk17; fi
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    # ARM64 Linux hosts use a shim over the NDK (README); elsewhere the newest NDK in the SDK.
    if [ -d "$HOME/Android/host-tools/ndk-shim" ]; then export ANDROID_NDK_HOME=$HOME/Android/host-tools/ndk-shim
    else export ANDROID_NDK_HOME=$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1); fi
fi
# ARM64 hosts run Gradle's x86 aapt2 under qemu (README).
if [ -d "$HOME/Android/host-tools/x86_64-root" ]; then export QEMU_LD_PREFIX=${QEMU_LD_PREFIX:-$HOME/Android/host-tools/x86_64-root}; fi
[ -f "$app/android/mpv/arm64-v8a/libmpv.so" ] || "$app/android/fetch-mpv.sh"
strip_tool=${LLVM_STRIP:-$(command -v llvm-strip || ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-strip | head -1)}
RUSTFLAGS="$remap" dx build --android --target aarch64-linux-android --release
project="$app/target/dx/veylo-dx/release/android/app"
# The C parts (ring) keep build paths in debug info only: ship the app's library without it (smaller too).
"$strip_tool" --strip-debug "$project/app/src/main/jniLibs/arm64-v8a/libmain.so"
# dx always writes versionCode 1; Android only updates to a higher one.
sed -i "s/versionCode = [0-9]*/versionCode = $version_code/; s/versionName = \"[^\"]*\"/versionName = \"$version\"/" "$project/app/build.gradle.kts"
(cd "$project" && ./gradlew assembleDebug -q)
build_tools=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)
# Gradle signs with this machine's debug key; the release carries Veylo's own key instead.
# (PKCS12: the key's password is the keystore's. Passing the same file twice would make apksigner read a second line.)
"$build_tools/apksigner" sign --ks "$keystore" --ks-key-alias veylo --ks-pass "file:$pass_file" \
    --out "$stage/$APK_ASSET" "$project/app/build/outputs/apk/debug/app-debug.apk"
rm -f "$stage/$APK_ASSET.idsig"
"$build_tools/apksigner" verify --print-certs "$stage/$APK_ASSET" | grep 'SHA-256' | sed 's/^/APK /'

# ---------------------------------------------------------------- privacy check
# Unpack everything that will be public and refuse if a key from your settings.json or your home path is inside.
mkdir -p "$work/check" && cp -r "$work/win/"* "$work/check/" && bsdtar -xf "$stage/$APK_ASSET" -C "$work/check"
for f in ${extra[@]+"${extra[@]}"}; do mkdir -p "$work/check/extra/$(basename "$f")" && bsdtar -xf "$f" -C "$work/check/extra/$(basename "$f")"; done
settings="${XDG_DATA_HOME:-$HOME/.local/share}/veylo-dx/settings.json"
python3 - "$settings" "$HOME" "$work/check" <<'PY'
import json, os, sys
settings, home, folder = sys.argv[1:4]
values = []
if os.path.isfile(settings):
    values = [v for k, v in json.load(open(settings)).items() if k in ("tmdb_key", "rd_key") and isinstance(v, str) and len(v) >= 8]
needles = [(v, "a key from settings.json") for v in values] + [(home, "your home folder path")]
for dirpath, _, files in os.walk(folder):
    for name in files:
        data = open(os.path.join(dirpath, name), "rb").read()
        for text, what in needles:
            if text.encode() in data or text.encode("utf-16-le") in data:
                sys.exit(f"Refusing to publish: {what} is inside {os.path.relpath(os.path.join(dirpath, name), folder)}")
print("Privacy check: no keys or local paths in the release files.")
PY

mkdir -p "$out_dir" && cp "$stage/$WIN_ASSET" "$stage/$APK_ASSET" ${extra[@]+"${extra[@]}"} "$out_dir/"
if [ "$publish" != 1 ]; then echo "Built $tag (not published): $out_dir"; exit 0; fi
# On Actions the tag goes on the commit that was built.
target=(); [ -n "${GITHUB_SHA:-}" ] && target=(--target "$GITHUB_SHA")
gh release create "$tag" "$stage/$WIN_ASSET" "$stage/$APK_ASSET" ${extra[@]+"${extra[@]}"} --repo "$REPO" "${target[@]}" --title "Veylo $version" --notes "$notes"
echo "Published $tag (Android versionCode $version_code): installs and in-app updates now get it."
