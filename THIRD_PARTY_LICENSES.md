# Third-Party Licenses & Notices

Veylo's own source code is licensed under the MIT License (see `LICENSE`).

## Distributed Binary Packages (GPL Notice & Corresponding Source)

Pre-built Veylo release packages for Windows, macOS, and Android dynamically link
and redistribute GPL-licensed builds of `libmpv`, `FFmpeg`, and their codec
dependencies. Under the GNU General Public License (GPLv2+ / GPLv3), the
combined distributed binary packages are governed by the terms of the GPL.

Corresponding source code and exact build configurations for every redistributed
GPL/LGPL component are publicly available from their pinned upstream build
repositories (and written source offers are honored upon request via the Veylo
repository):

- **libmpv & FFmpeg (Windows)**: built by `shinchiro/mpv-winbuild-cmake`
  - Pinned tag & SHA-256: `apps/veylo-dx/windows/fetch-mpv.ps1`
  - Upstream source & build scripts: https://github.com/shinchiro/mpv-winbuild-cmake
  - mpv source (GPLv2+ / LGPLv2.1+): https://github.com/mpv-player/mpv
  - FFmpeg source (GPLv2+ / GPLv3 / LGPLv2.1+): https://ffmpeg.org/download.html
- **libmpv & FFmpeg (Android)**: built by `mpv-android/mpv-android`
  - Pinned tag & SHA-256: `apps/veylo-dx/android/fetch-mpv.sh`
  - Upstream source & build scripts: https://github.com/mpv-android/mpv-android
  - License bundled in APK assets: `licenses/LICENSE-mpv-android`
- **libmpv, FFmpeg & transitive Homebrew codecs (macOS)**:
  - Bundled by `tools/build-macos.sh` from Homebrew formulae (`mpv`, `ffmpeg`, and
    all transitive dependencies including `x264`, `x265`, `libass`, `dav1d`, etc.).
  - Upstream formula recipes & source tarballs: https://formulae.brew.sh/formula/mpv
  - Per-formula license texts are copied into `Veylo.app/Contents/Resources/licenses/`.

## Fonts

- **Archivo** (`apps/veylo-dx/assets/fonts/archivo.ttf`)
  - Copyright 2020 The Archivo Project Authors (Omnibus-Type)
  - Licensed under the SIL Open Font License, Version 1.1 (`apps/veylo-dx/assets/fonts/OFL.txt`).

## On-Device AI Assets

- **ONNX Runtime** (MIT License) — `LICENSE` and `ThirdPartyNotices.txt` are
  bundled inside `picks-ai/runtime/`.
- **bge-small-en-v1.5** (MIT License) — notice bundled in `tools/picks-ai/NOTICE`
  and `picks-ai/`.

## Media3 FFmpeg Audio Extension Tools

- Upstream license notices are preserved under `tools/media3-ffmpeg/licenses/`.

## Metadata & Artwork Attributions

- **TMDB**: This product uses the TMDB API but is not endorsed or certified by TMDB.
  https://www.themoviedb.org/
- **TVmaze**: Series metadata and air schedules are provided by TVmaze under CC BY-SA.
  https://www.tvmaze.com/
- **Wikidata & Wikimedia Commons**: Open metadata under CC0; images under their
  respective authors' licenses. https://www.wikidata.org/
