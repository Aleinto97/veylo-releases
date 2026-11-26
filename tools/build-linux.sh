#!/usr/bin/env bash
# Build a self-contained Veylo bundle for Linux (x86_64 or ARM64) for release and install.sh:
#
#   tools/build-linux.sh            -> dist/veylo-linux-<arch>.tar.gz (or VEYLO_OUT)
#
# Needs Rust, Python 3, readelf, tar, and system development libraries (libmpv, webkit2gtk-4.1, gtk3, libxdo).
# Bundles the binary (linked with $ORIGIN/lib rpath), Picks ONNX Runtime + model, libxdo (whose SONAME
# varies across distros), desktop icons, and licences. Later updates arrive through the in-app Update button.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
app_src="$root/apps/veylo-dx"
out_dir=${VEYLO_OUT:-$root/dist}
version=$(awk -F'"' '/^version = /{if (!found) {print $2; found=1}}' "$app_src/Cargo.toml")
[ -n "$version" ] || { echo "Couldn't read version from $app_src/Cargo.toml" >&2; exit 1; }
[ "$(uname -s)" = Linux ] || { echo "Build the Linux bundle on Linux." >&2; exit 1; }
case "$(uname -m)" in
    x86_64|amd64) ASSET=veylo-linux-x64.tar.gz; PICKS_TARGET=linux-x64 ;;
    aarch64|arm64) ASSET=veylo-linux-arm64.tar.gz; PICKS_TARGET=linux-aarch64 ;;
    *) echo "Unsupported Linux architecture: $(uname -m)" >&2; exit 1 ;;
esac

python3 "$root/tools/picks-ai/fetch.py" "$PICKS_TARGET"
cd "$app_src"
# No local folder names (your home path, so your username) inside the binary; check $ORIGIN/lib first
# for bundled distro-varying libraries like libxdo.
RUSTFLAGS="--remap-path-prefix=$HOME=~ --remap-path-prefix=$root=veylo -C link-arg=-Wl,-rpath,\$ORIGIN/lib" \
    cargo build --release --locked
release="$app_src/target/release"
readelf_out=$(readelf -d "$release/veylo-dx")
printf '%s\n' "$readelf_out" | grep -E 'R(UN)?PATH.*\$ORIGIN/lib' >/dev/null \
    || { echo "Missing \$ORIGIN/lib RPATH/RUNPATH on $release/veylo-dx" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bundle="$work/veylo"
mkdir -p "$bundle/lib" "$bundle/icons" "$bundle/licenses"

cp "$release/veylo-dx" "$bundle/veylo"
chmod 755 "$bundle/veylo"
if command -v strip >/dev/null 2>&1; then
    strip --strip-debug "$bundle/veylo"
fi
printf '%s\n' "$version" > "$bundle/.veylo-bundle"
cp -R "$release/picks-ai" "$bundle/picks-ai"

# Bundle libxdo matching the build host's SONAME (Ubuntu ships .so.3 while Arch ships .so.4).
xdo_line=$(ldd "$release/veylo-dx" | awk '/libxdo\.so/{if (!found) {print $1, $3; found=1}}')
if [ -n "$xdo_line" ]; then
    xdo_soname=${xdo_line%% *}
    xdo_path=${xdo_line#* }
    [ -f "$xdo_path" ] || { echo "Couldn't resolve $xdo_soname from ldd ($xdo_path)" >&2; exit 1; }
    cp -L "$xdo_path" "$bundle/lib/$xdo_soname"
fi

cp "$root/branding/veylo-icon.png" "$bundle/icons/veylo.png"
cp "$root/branding/veylo-icon.svg" "$bundle/icons/veylo.svg"
cp "$root/LICENSE" "$root/PRIVACY.md" "$root/THIRD_PARTY_LICENSES.md" "$app_src/assets/fonts/OFL.txt" "$bundle/licenses/"
for lic in /usr/share/doc/libxdo*/copyright /usr/share/licenses/xdotool/* /usr/share/licenses/libxdo*/*; do
    if [ -f "$lic" ]; then
        cp "$lic" "$bundle/licenses/libxdo-$(basename "$lic")"
        break
    fi
done
chmod 644 "$bundle/licenses/"*

mkdir -p "$out_dir"
rm -f "$out_dir/$ASSET"
tar -C "$work" -czf "$out_dir/$ASSET" veylo
echo "Built Veylo $version for Linux $(uname -m) ($(du -sh "$bundle" | cut -f1) unpacked): $out_dir/$ASSET"
