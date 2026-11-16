# Veylo

<img src="branding/veylo-icon.png" width="96" alt="Veylo">

A quiet, all-Rust media client for Android tablets and the Linux and Windows desktop, built with [Dioxus](https://dioxuslabs.com).

- **Home**: an artwork-lit marquee, continue watching and catalogues.
- **Picks**: a two-card duel that learns your taste (type, style, genre and years; hides what you have watched).
- **Library**: saved titles and what you watched, with per-episode progress.
- **Player**: one player everywhere, libmpv driven from Rust with Dioxus controls (`engine.rs`, `player_ui.rs`), its picture
  under the page through a bridge per OS (`video.rs`): a SurfaceView on Android, a GL area or Wayland subsurface on Linux,
  mpv's own child window on Windows.

## Install

**Windows** 10 or 11, 64-bit: in PowerShell or Windows Terminal run

```powershell
irm https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.ps1 | iex
```

Veylo installs to `%LOCALAPPDATA%\Programs\Veylo` (no admin rights) with Start menu and desktop shortcuts. Running
the command again reinstalls the latest version. To uninstall, delete that folder and the two shortcuts; your
library is in `%LOCALAPPDATA%\veylo-dx`.

**Android** 8 or later, 64-bit: on the device open
https://github.com/Aleinto97/veylo-releases/releases/latest/download/veylo-android-arm64.apk, open the downloaded
file and allow your browser to install apps when Android asks.

**Updates:** when a new version is out, Veylo shows **Veylo x.y is available** with an **Update** button. On
Android the first Update asks you to allow Veylo to install apps: allow it, come back and press Update again.

## Layout

| Path | What |
| --- | --- |
| `apps/veylo-dx/` | the app (Dioxus UI, Android glue in `android/`, fonts in `assets/`) |
| `crates/core/` | addon protocol, SQLite library/history, recommendations, subtitles |
| `crates/media-proxy/` | token-scoped loopback media relay |
| `crates/torrent/`, `crates/aio-meta/` | P2P and embedded metadata crates (not used by the app yet) |
| `tools/media3-ffmpeg/` | reproducible build of the Media3 FFmpeg audio decoder (the app no longer ships it) |
| `branding/` | logo sources |

## Build

```sh
cd apps/veylo-dx
cargo run --release              # Linux desktop
./android/fetch-mpv.sh           # libmpv for Android (the player), once
dx build --platform android      # Android APK (needs the Android SDK/NDK)
```

Windows (x64, needs the Rust MSVC toolchain and the WebView2 runtime that ships with Windows 10/11):

```powershell
cd apps\veylo-dx
powershell -ExecutionPolicy Bypass -File windows\fetch-mpv.ps1   # pinned libmpv-2.dll + import library
cargo build --release   # target\release\veylo-dx.exe, with libmpv-2.dll copied beside it
```

Ship `veylo-dx.exe` together with `libmpv-2.dll`. From Linux, `cargo xwin build --target x86_64-pc-windows-msvc --release`
cross-builds the same with `VEYLO_MPV_DIR` pointing at the extracted mpv files.

## Releases

Every push to `main` that raises `version` in `apps/veylo-dx/Cargo.toml` builds Windows and Android on GitHub
Actions (`.github/workflows/release.yml`, which runs `tools/release.sh`) and publishes them as a release here.
Installed copies check this repo's latest release at launch and show **Veylo x.y is available — Update**
(`apps/veylo-dx/src/updater.rs`): Windows replaces its files and restarts; Android downloads the APK and opens the
system's "Update this app?" screen (`android/kotlin/VeyloUpdate.kt`).

The APK is signed with Veylo's release key, kept out of the repo: in the `ANDROID_KEYSTORE_B64` and
`ANDROID_KEYSTORE_PASSWORD` Actions secrets, and locally in `~/.config/veylo/` (`release.jks`, alias `veylo`, and
`release.pass`). Android only updates an app from an APK signed with the same key, so the key never changes. To
put a build of your own on a device that has the release, sign it with that key too. `tools/release.sh` also runs
locally (`VEYLO_PUBLISH=0` builds into `dist/` without publishing). It refuses to publish if a key from your
settings or a local path is inside the files.

Tests: `cargo test` in the repo root (core crates) and in `apps/veylo-dx`.

Licence: MIT. Archivo is under the SIL Open Font License. libmpv/FFmpeg ship as built by mpv-android (Android) and
mpv-winbuild-cmake (Windows), under their licences; the Media3 FFmpeg licences are in `tools/media3-ffmpeg/licenses`.
