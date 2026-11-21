#!/usr/bin/env bash
# Build Veylo.app for macOS on Apple Silicon and zip it for release and install.sh:
#
#   tools/build-macos.sh            -> dist/veylo-macos-arm64.zip (or VEYLO_OUT)
#
# Runs on a Mac (GitHub Actions' macos runner, or yours). Needs Rust, Python 3 and Homebrew with
#   brew install mpv dylibbundler
# The app carries everything it loads: libmpv with FFmpeg and their libraries (Contents/Frameworks),
# MoltenVK for mpv's Vulkan renderer, and the Picks model and ONNX Runtime (Contents/Resources).
# It is signed ad hoc (no Apple Developer ID): install.sh downloads it with curl, which macOS doesn't
# quarantine, so Gatekeeper doesn't block it. The build fails if any library still points into Homebrew.
set -euo pipefail
ASSET=veylo-macos-arm64.zip
root=$(cd "$(dirname "$0")/.." && pwd)
app_src="$root/apps/veylo-dx"
out_dir=${VEYLO_OUT:-$root/dist}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$app_src/Cargo.toml" | head -1)
[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || { echo "Build the macOS app on an Apple Silicon Mac." >&2; exit 1; }
command -v brew >/dev/null || { echo "Homebrew is missing: https://brew.sh" >&2; exit 1; }
command -v dylibbundler >/dev/null || { echo "Run: brew install mpv dylibbundler" >&2; exit 1; }
brew_prefix=$(brew --prefix)
# Homebrew's libraries are built for this macOS and later.
min_macos="$(sw_vers -productVersion | cut -d. -f1).0"

python3 "$root/tools/picks-ai/fetch.py" macos-arm64
cd "$app_src"
# No local folder names (your home path, so your username) inside the binary.
RUSTFLAGS="--remap-path-prefix=$HOME=~ --remap-path-prefix=$root=veylo" MACOSX_DEPLOYMENT_TARGET="$min_macos" \
    cargo build --release --locked
release="$app_src/target/release"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/Veylo.app"
contents="$app/Contents"
mkdir -p "$contents/MacOS" "$contents/Resources" "$contents/Frameworks"
cp "$release/veylo-dx" "$contents/MacOS/veylo-dx"
cp -R "$release/picks-ai" "$contents/Resources/picks-ai"

# The icon, from the same artwork as Android and Windows.
iconset="$work/veylo.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z $size $size "$root/branding/veylo-icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$root/branding/veylo-icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$contents/Resources/veylo.icns"

cat > "$contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Veylo</string>
    <key>CFBundleDisplayName</key><string>Veylo</string>
    <key>CFBundleIdentifier</key><string>dev.veylo.dx</string>
    <key>CFBundleExecutable</key><string>veylo-dx</string>
    <key>CFBundleIconFile</key><string>veylo</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>$min_macos</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.entertainment</string>
    <key>NSHighResolutionCapable</key><true/>
    <!-- Add-on catalogues and artwork aren't all served over HTTPS. -->
    <key>NSAppTransportSecurity</key>
    <dict>
        <key>NSAllowsArbitraryLoads</key><true/>
        <key>NSAllowsArbitraryLoadsInWebContent</key><true/>
    </dict>
</dict>
</plist>
PLIST

# libmpv and everything it links, copied in and pointed at @executable_path/../Frameworks.
# Every dependency's lib folder is a search path, so dylibbundler never stops to ask for one.
search=(-s "$brew_prefix/lib")
for formula in mpv $(brew deps mpv); do search+=(-s "$(brew --prefix "$formula")/lib"); done
dylibbundler -of -b -x "$contents/MacOS/veylo-dx" -d "$contents/Frameworks" -p @executable_path/../Frameworks/ \
    "${search[@]}" < /dev/null >/dev/null
# mpv's renderer uses Vulkan: bring MoltenVK (the driver) and its manifest; main.rs points the loader at it.
if moltenvk=$(brew --prefix molten-vk 2>/dev/null) && [ -f "$moltenvk/lib/libMoltenVK.dylib" ]; then
    cp "$moltenvk/lib/libMoltenVK.dylib" "$contents/Frameworks/"
    chmod u+w "$contents/Frameworks/libMoltenVK.dylib"
    install_name_tool -id @executable_path/../Frameworks/libMoltenVK.dylib "$contents/Frameworks/libMoltenVK.dylib"
    dylibbundler -of -b -x "$contents/Frameworks/libMoltenVK.dylib" -d "$contents/Frameworks" -p @executable_path/../Frameworks/ \
        "${search[@]}" < /dev/null >/dev/null
    mkdir -p "$contents/Resources/vulkan/icd.d"
    printf '{\n  "file_format_version": "1.0.0",\n  "ICD": { "library_path": "../../../Frameworks/libMoltenVK.dylib", "api_version": "1.2.0" }\n}\n' \
        > "$contents/Resources/vulkan/icd.d/MoltenVK_icd.json"
fi

# Licences of what the bundle carries (mpv, FFmpeg; ONNX Runtime and the model ship theirs in picks-ai).
mkdir -p "$contents/Resources/licenses"
for formula in mpv ffmpeg molten-vk; do
    prefix=$(brew --prefix "$formula" 2>/dev/null) || continue
    for f in "$prefix"/LICENSE* "$prefix"/COPYING* "$prefix"/Copyright*; do
        [ -f "$f" ] && cp "$f" "$contents/Resources/licenses/$formula-$(basename "$f")"
    done
done

# Nothing may still load from Homebrew: it isn't on other Macs.
leaks=0
while IFS= read -r -d '' file; do
    file -b "$file" | grep -q 'Mach-O' || continue
    if otool -L "$file" | tail -n +2 | grep -E "$brew_prefix|/usr/local/|@rpath" >/dev/null; then
        echo "Still linked outside the bundle: ${file#"$work"/}" >&2
        otool -L "$file" | tail -n +2 | grep -E "$brew_prefix|/usr/local/|@rpath" >&2
        leaks=1
    fi
done < <(find "$app" -type f -print0)
[ "$leaks" = 0 ] || exit 1

# Ad-hoc signatures: every library, then the app (Apple Silicon runs only signed code).
find "$contents/Frameworks" "$contents/Resources" -type f \( -name '*.dylib' -o -name '*.so' \) -exec codesign --force --sign - {} \;
codesign --force --sign - "$app"
codesign --verify --strict "$app"

mkdir -p "$out_dir"
rm -f "$out_dir/$ASSET"
ditto -c -k --sequesterRsrc --keepParent "$app" "$out_dir/$ASSET"
echo "Built Veylo $version for macOS $min_macos+ ($(du -sh "$app" | cut -f1) unpacked): $out_dir/$ASSET"
