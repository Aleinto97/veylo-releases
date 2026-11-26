#!/bin/sh
# Veylo for macOS (Apple Silicon) and Linux (x86_64, ARM64): install, or update to, the latest release.
#   curl -fsSL https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.sh | sh
# - macOS: installs Veylo.app to /Applications (or ~/Applications when that isn't writable) and opens it.
# - Linux: installs to ~/.local/lib/veylo, links ~/.local/bin/veylo, registers the desktop launcher and
#   stremio:// / veylo:// deep links, and launches Veylo. No root rights needed.
# Later versions arrive through the Update button inside Veylo.
# Testing: VEYLO_TARBALL / VEYLO_ZIP=<local archive> installs that file; VEYLO_NO_LAUNCH=1 doesn't open the app.
set -eu
repo=Aleinto97/veylo-releases

fail() { echo "$*" >&2; exit 1; }
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print tolower($1)}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print tolower($1)}'
    else
        fail "Refusing to install: neither sha256sum nor shasum is available to verify the download."
    fi
}

os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Darwin)
        [ "$arch" = arm64 ] || fail "Veylo for macOS needs a Mac with Apple silicon (M1 or later)."
        asset=veylo-macos-arm64.zip
        os_label=macOS
        ;;
    Linux)
        case "$arch" in
            x86_64|amd64) asset=veylo-linux-x64.tar.gz ;;
            aarch64|arm64) asset=veylo-linux-arm64.tar.gz ;;
            *) fail "Veylo for Linux supports x86_64 and ARM64 (found $arch)." ;;
        esac
        os_label=Linux
        ;;
    *)
        fail "This installer is for macOS and Linux. On Windows, run install.ps1 in PowerShell."
        ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

local_archive=${VEYLO_TARBALL:-${VEYLO_ZIP:-}}
if [ -n "$local_archive" ]; then
    cp "$local_archive" "$work/$asset"
else
    echo "Downloading Veylo..."
    base="https://github.com/$repo/releases/latest/download"
    curl -fL --progress-bar -o "$work/$asset" "$base/$asset" \
        || fail "The latest Veylo release has no $os_label download yet."
    curl -fsSL -o "$work/SHA256SUMS.txt" "$base/SHA256SUMS.txt" \
        || fail "Refusing to install: couldn't download SHA256SUMS.txt to verify the release."
    expected=$(awk -v f="$asset" '{gsub(/^\*/, "", $2); if ($2 == f) print tolower($1)}' "$work/SHA256SUMS.txt" | head -1)
    [ -n "$expected" ] || fail "Refusing to install: $asset is missing from SHA256SUMS.txt."
    actual=$(sha256_of "$work/$asset")
    [ "$actual" = "$expected" ] || fail "Refusing to install: SHA-256 verification failed for $asset."
fi

if [ "$os" = Darwin ]; then
    dest=${VEYLO_INSTALL_DIR:-/Applications}
    if [ ! -w "$dest" ]; then dest="$HOME/Applications"; mkdir -p "$dest"; fi
    app="$dest/Veylo.app"
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
else
    command -v tar >/dev/null 2>&1 || fail "tar is required to install Veylo on Linux."
    mkdir -p "$work/unpacked"
    tar -xzf "$work/$asset" -C "$work/unpacked"
    [ -x "$work/unpacked/veylo/veylo" ] && [ -f "$work/unpacked/veylo/.veylo-bundle" ] \
        || fail "The download isn't a Veylo Linux bundle."

    dest=${VEYLO_INSTALL_DIR:-$HOME/.local/lib/veylo}
    bin_dir=${VEYLO_BIN_DIR:-$HOME/.local/bin}
    data_home=${XDG_DATA_HOME:-$HOME/.local/share}

    # A running Veylo from this folder holds its files: close it first.
    pkill -f "$dest/veylo" 2>/dev/null && sleep 1 || true

    mkdir -p "$(dirname "$dest")" "$bin_dir" "$data_home/applications" \
        "$data_home/icons/hicolor/512x512/apps" "$data_home/icons/hicolor/scalable/apps"

    backup="$dest.old.$$"
    if [ -d "$dest.old" ] && [ -f "$dest.old/.veylo-bundle" ]; then
        rm -rf "$dest.old"
    fi
    if [ -e "$dest" ]; then
        mv "$dest" "$backup"
    fi
    if ! mv "$work/unpacked/veylo" "$dest"; then
        if [ -e "$backup" ] && [ ! -e "$dest" ]; then
            mv "$backup" "$dest" || true
        fi
        fail "Failed to install Veylo into $dest."
    fi
    rm -rf "$backup"

    if command -v ldd >/dev/null 2>&1; then
        missing=$(ldd "$dest/veylo" 2>/dev/null | awk '/=> not found/{print $1}' | sort -u)
        if [ -n "$missing" ]; then
            echo "Missing runtime system libraries for Veylo:" >&2
            printf '  %s\n' "$missing" >&2
            echo "Install the required system libraries first:" >&2
            echo "  Arch / Omarchy: sudo pacman -S mpv webkit2gtk-4.1" >&2
            echo "  Debian / Ubuntu: sudo apt install libmpv2 libwebkit2gtk-4.1-0" >&2
            echo "  Fedora:          sudo dnf install mpv-libs webkit2gtk4.1" >&2
            exit 1
        fi
    fi

    ln -sfn "$dest/veylo" "$bin_dir/veylo"
    [ -f "$dest/icons/veylo.png" ] && cp "$dest/icons/veylo.png" "$data_home/icons/hicolor/512x512/apps/veylo.png"
    [ -f "$dest/icons/veylo.svg" ] && cp "$dest/icons/veylo.svg" "$data_home/icons/hicolor/scalable/apps/veylo.svg"

    cat > "$data_home/applications/veylo.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Veylo
Comment=Films and series
Exec=$bin_dir/veylo %u
Icon=veylo
Terminal=false
Categories=AudioVideo;Video;Player;
MimeType=x-scheme-handler/stremio;x-scheme-handler/veylo;
StartupWMClass=veylo
EOF

    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f "$data_home/icons/hicolor" >/dev/null 2>&1 || true
    command -v xdg-mime >/dev/null 2>&1 && xdg-mime default veylo.desktop x-scheme-handler/stremio x-scheme-handler/veylo >/dev/null 2>&1 || true

    version=$(cat "$dest/.veylo-bundle" 2>/dev/null || echo "")
    echo "Veylo $version is installed in $dest (linked at $bin_dir/veylo)."
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) echo "Note: add $bin_dir to your PATH to launch 'veylo' from any shell." ;;
    esac
    if [ -z "${VEYLO_NO_LAUNCH:-}" ]; then
        echo "Opening it now; next time launch 'veylo' or pick Veylo from your application launcher."
        nohup "$bin_dir/veylo" >/dev/null 2>&1 </dev/null &
    fi
fi
