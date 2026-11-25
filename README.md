<p align="center">
  <img src="branding/veylo-icon.png" width="96" alt="Veylo">
</p>

<h1 align="center">Veylo</h1>

<p align="center">
  <strong>A fast, quiet, all-Rust media client for Android, Android TV, macOS, Windows, and Linux.</strong><br>
  Built with <a href="https://dioxuslabs.com">Dioxus</a>, native <code>libmpv</code> HDR playback, on-device ONNX AI recommendations, offline downloads, and encrypted cross-OS sync.
</p>

<p align="center">
  <a href="https://github.com/Aleinto97/veylo-releases/releases/latest"><img src="https://img.shields.io/github/v/release/Aleinto97/veylo-releases?style=flat-square&color=111111" alt="Latest release"></a>
  <a href="https://github.com/Aleinto97/veylo-releases/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Aleinto97/veylo-releases/ci.yml?branch=main&style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/Aleinto97/veylo-releases/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/Aleinto97/veylo-releases/release.yml?branch=main&style=flat-square&label=Release" alt="Release"></a>
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License">
</p>

---

## Highlights

| Area | What Veylo does |
| --- | --- |
| **Native `libmpv` HDR Player** | One player on every platform with glass Dioxus controls over a native video surface (`SurfaceView` + MediaCodec direct HDR10/HLG/HDR10+ on Android/TV, D3D11 + HDR swapchain on Windows, Core Animation EDR on macOS, Wayland subsurface / GL area on Linux). Includes Dolby Vision Profile 5 reshaping, FFmpeg HTTP reconnect resilience, subtitle & audio sync (`±` delay), subtitle sizing/positioning/box, chapter ticks & picker, intro/recap/credits skipping (chapters + IntroDB + AniSkip), floating **Up Next** countdown, and per-series audio/subtitle memory. |
| **Smart Autoplay** | Queries add-ons in parallel, verifies direct files with chunked header inspection (real resolution, HDR/Dolby Vision tier, audio tracks, and duration), rejects broken/sample/non-video links, respects your screen's HDR/Dolby Vision capabilities, and prefetches the next episode near the end of playback. |
| **Android, Tablet & TV Native** | Full Android TV D-pad/remote navigation and Leanback banner (`arm64` + 32-bit `armv7` APKs), `AudioFocus`, headphone/Bluetooth disconnect auto-pause, system `MediaSession` controls, `Surface.setFrameRate` matching, tablet touch gestures (double-tap ±10s, brightness/volume swipe, long-press 2×), rotation lock, and Picture-in-Picture. |
| **Local AI Picks** | Endless two-card taste duel powered by an on-device INT8 `bge-small-en-v1.5` embedding model (ONNX Runtime, zero cloud inference) and online Bayesian preference learning with moods, match reasons, and a live shortlist. |
| **Library & Offline Downloads** | Chronological history, filter tabs (*All, Movies, Series, In progress, Watched, Saved, Downloads*), in-library search, direct offline downloads with automatic offline playback, and Trakt / IMDb / Veylo JSON watch-history import & export. |
| **Zero-Knowledge E2EE Sync** | Optional Firebase account (Email, Google, or QR-code desktop linking) that end-to-end encrypts watch progress, library, likes, preferences, TMDB key, and add-ons on-device with AES-256-GCM using a user-held 256-bit Sync Key (transferred automatically over encrypted QR linking). |
| **Neutral Add-ons & Privacy** | Ships with zero pre-installed streaming add-ons. Supports one-tap `stremio://` & `veylo://` OS deep links, `configurable` manifests, portable add-on bundles, one-time on-device Stremio collection import, and user-supplied directory URLs. Credentials are masked and never logged; browsing prefetch and diagnostics are strictly opt-in; updates fail closed without verified SHA-256 checksums. See [PRIVACY.md](PRIVACY.md). |

---

## Install

### macOS (Apple Silicon — M1 or later)

```sh
curl -fsSL https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.sh | sh
```

Installs `Veylo.app` into `/Applications` (or `~/Applications`) and launches it. Re-run to update. To uninstall, move Veylo to the Bin; your data stays in `~/Library/Application Support/veylo-dx`.

### Windows 10 / 11 (64-bit)

```powershell
irm https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\Programs\Veylo` (no admin rights required) with Start menu and desktop shortcuts. Re-run to update. Your data lives in `%LOCALAPPDATA%\veylo-dx`.

### Android & Android TV (Android 8+)

* **Phones, tablets & 64-bit TVs (`arm64-v8a`)**:
  [Download `veylo-android-arm64.apk`](https://github.com/Aleinto97/veylo-releases/releases/latest/download/veylo-android-arm64.apk)
* **32-bit Android TVs (`armeabi-v7a`)**:
  [Download `veylo-android-armv7.apk`](https://github.com/Aleinto97/veylo-releases/releases/latest/download/veylo-android-armv7.apk)

Open the APK on the device and allow app installation when prompted.

### Updates

Veylo checks the latest GitHub release at launch, verifies downloads against `SHA256SUMS.txt` (or the release SHA-256 digest), and offers a one-click **Update** button. On Android, the first in-app update asks for "Install unknown apps" permission once.

---

## Repository Layout

| Path | What |
| --- | --- |
| `apps/veylo-dx/` | All-Rust Dioxus application (`src/` UI + backend, `android/` Kotlin JNI bridges & TV resources, `firebase/` rules & sign-in page) |
| `crates/core/` | Stremio add-on protocol, SQLite library/history/sync store, recommendations, subtitle parser & cache |
| `crates/media-proxy/` | Token-scoped loopback media relay and header-safe stream prober |
| `crates/video-surface/` | Desktop video surface bridges (Wayland subsurface / GTK GL area, Windows D3D11, macOS Core Animation EDR) |
| `crates/picture-check/` | Colour-bar picture verification test used by CI |
| `tools/` | `release.sh`, `build-macos.sh`, `picks-ai/fetch.py`, and CI launch checks (`tools/ci/`) |
| `branding/` | App icon and Android TV banner sources |

---

## Building from Source

### Linux Desktop

```sh
cd apps/veylo-dx
python3 ../../tools/picks-ai/fetch.py linux-x64   # use linux-aarch64 on ARM64 Linux
cargo run --release
```

### Android (`arm64-v8a` / `armeabi-v7a`)

Requires the Android SDK (platform 36, build-tools 36), NDK 28, JDK 17, and `dioxus-cli` (`dx` 0.7.10):

```sh
cd apps/veylo-dx
./android/fetch-mpv.sh
python3 ../../tools/picks-ai/fetch.py android-arm64 android-armv7
dx build --android --target aarch64-linux-android --release
```

### Windows (x64)

Requires the Rust MSVC toolchain, WebView2 (ships with Windows 10/11), and `bsdtar`/`libarchive`:

```powershell
python3 tools\picks-ai\fetch.py windows-x64
cd apps\veylo-dx
powershell -ExecutionPolicy Bypass -File windows\fetch-mpv.ps1
cargo build --release
```

Keep `veylo-dx.exe`, `libmpv-2.dll`, and the `picks-ai/` directory together when distributing. From Linux, cross-build with `cargo xwin build --target x86_64-pc-windows-msvc --release`.

### macOS (Apple Silicon)

```sh
brew install mpv dylibbundler
python3 tools/picks-ai/fetch.py macos-arm64
cd apps/veylo-dx && cargo run --release   # or run tools/build-macos.sh for a self-contained Veylo.app
```

---

## Testing & Verification

Run the test suites in both workspaces:

```sh
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
cd apps/veylo-dx && cargo test && cargo clippy --all-targets -- -D warnings
```

Run the standalone headless self-test anywhere (uses an isolated temporary store; your library is never touched):

```sh
veylo-dx --self-test [--online] [--report <file>]
```

It validates bundled model/runtime/libmpv assets, store persistence, on-device Picks embeddings, H.264/AAC & HDR playback through `libmpv`, and (with `--online`) TLS and the release channel. `.github/workflows/ci.yml` runs tests, clippy, self-tests, and packaged install/launch checks natively on Linux, Windows, and macOS on every push.

---

## Releases

Every push to `main` that raises `version` in `apps/veylo-dx/Cargo.toml` triggers `.github/workflows/release.yml`, building signed Android (`arm64` + `armv7`), Windows (`x64`), and macOS (`arm64`) binaries plus `SHA256SUMS.txt` and publishing them automatically. `tools/release.sh` runs the same build locally (`VEYLO_PUBLISH=0` builds into `dist/` without publishing) and runs a privacy check ensuring no local paths or API keys appear in release artifacts.

<details>
<summary><strong>How on-device Picks AI works</strong></summary>

* **Local inference**: Runs entirely on the CPU in Rust via ONNX Runtime 1.22.0 with the pinned INT8 `bge-small-en-v1.5` model (MIT, ~34 MB bundled in releases — no Python or runtime downloads).
* **Enrichment & caching**: Synopses are enriched in the background with TMDB keywords (when configured) or Wikidata/TVmaze metadata, cached in bounded local files (`picks-metadata-v1.json`, `picks-embeddings-v1.json`, max 1,024 entries; zero API keys or URLs written).
* **Bayesian taste model (`apps/veylo-dx/src/brain.rs`)**: Projects 384-dim embeddings onto catalog PCA directions combined with genre, type, style, decade, and rating. Online Bayesian logistic regression tracks both lasting taste (likes, finishes, saves, plays) and tonight's session mood, using Thompson sampling so duels balance likely favorites with unexplored directions.
* **Controls**: `←` / `→` keep a card, `S` skips a pair, `N` resets tonight's direction, `?` opens page-specific shortcuts, and **Settings → Learned taste → Start over** resets learned preferences while keeping the model cache.

</details>

---

## Licence

Veylo's source code is licensed under the [MIT License](LICENSE). Distributed binary packages dynamically link GPL-licensed `libmpv` and `FFmpeg` builds and are therefore governed as combined works under the GPL (with corresponding source links and license texts bundled in every release; see [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)). The bundled **Outfit** and **Inter** fonts are licensed under the [SIL Open Font License 1.1](apps/veylo-dx/assets/fonts/OFL.txt), and UI icons are from **Lucide** (ISC License). ONNX Runtime and `bge-small-en-v1.5` notices are bundled in `picks-ai/`. Metadata comes from keyless public sources (BetterPosters, ElfHosted, JustWatch, TVmaze, Wikidata, Wikipedia, AniList). See [PRIVACY.md](PRIVACY.md) for privacy details.
