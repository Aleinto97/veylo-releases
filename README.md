# Veylo

<img src="branding/veylo-icon.png" width="96" alt="Veylo">

A quiet, all-Rust media client for Android tablets and the Linux and Windows desktop, built with [Dioxus](https://dioxuslabs.com).

- **Home**: Tonight's pick (why it was chosen, Play, Not tonight and the next picks), then today's charts from
  [BetterPosters](https://btttr.cc) and your add-ons' catalogues. The top bar shows what is on the player: one tap carries on.
- **Title pages**: the film's own logo, stills, tagline, chart rank, studios, cast and crew, season art, trailers, and an
  episode grid that splits long seasons into ranges.
- **Picks**: an endless two-card duel run by one local taste model over synopsis embeddings: match scores, reasons, moods, a shortlist of tonight's winners, Like and Not for me on every card, and nothing shown twice in a night.
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
python3 ../../tools/picks-ai/fetch.py linux-x64  # linux-aarch64 on ARM64 Linux
cargo run --release              # Linux desktop
./android/fetch-mpv.sh           # libmpv for Android (the player), once
python3 ../../tools/picks-ai/fetch.py android-arm64
dx build --platform android      # Android APK (needs the Android SDK/NDK)
```

Windows (x64, needs the Rust MSVC toolchain and the WebView2 runtime that ships with Windows 10/11). First prepare the assets with `python3 tools/picks-ai/fetch.py windows-x64` (requires bsdtar/libarchive; the Linux release builder does this automatically):

```powershell
cd apps\veylo-dx
powershell -ExecutionPolicy Bypass -File windows\fetch-mpv.ps1   # pinned libmpv-2.dll + import library
cargo build --release   # target\release\veylo-dx.exe, with libmpv-2.dll copied beside it
```

Ship `veylo-dx.exe` together with `libmpv-2.dll` and the complete `picks-ai/` directory from the build output. From Linux, `cargo xwin build --target x86_64-pc-windows-msvc --release`
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

Picks inference runs entirely on the CPU, in Rust through ONNX Runtime 1.22.0. Releases bundle
the pinned INT8 bge-small-en-v1.5 model (MIT), tokenizer, native runtime and licences; the APK needs
no model download or Python. Android extracts the model once into app-private storage. The
model is about 34 MB before APK compression, and the app needs extra private storage for the
extracted copy. Linux and Windows load `picks-ai/` beside the executable. Preserve that directory
when distributing a desktop build. The Windows bundle includes its app-local VC++ runtime DLLs.

Picks enriches available synopses with TMDB movie/TV keywords using the existing Settings key.
Enrichment runs in the background, with per-title request deduplication, rate limiting
and a bounded `picks-metadata-v1.json` cache (1,024 titles). Successful results refresh after seven
days; missing results after one day; failures retry after five minutes while keeping stale data.
No API key or authenticated request is written to these caches. The model still runs entirely locally.
Its 256-token input is the title, year, synopsis and keywords (genres are separate taste features);
the name and keywords keep a reserved share so a long synopsis never crowds them out.

Each title becomes a 384-dimension embedding, cached locally by model version and input hash
(`picks-embeddings-v1.json`, at most 1,024 entries). Changed enrichment embeds the title again. Titles
without a usable synopsis keep metadata ranking. An inference failure shows a status message and
preserves the duel with metadata ranking.
One taste model (`apps/veylo-dx/src/brain.rs`) is the brain of Picks and of Home's Tonight's pick.
Each title's features are its embedding projected onto the main directions the catalogue varies along
(PCA over the embedded titles, whitened, refitted as the catalogue grows), plus genre, type, style,
decade and rating. Taste is a Gaussian belief over those weights (online Bayesian logistic regression),
so the model knows how sure it is. Lasting taste learns from likes and dislikes, finishes, saves and
long watches in the library, plays from Picks and, lightly, duel votes; tonight starts from lasting
taste with room to move, is steered by duel votes, Neither and moods, and lets older votes fade.
The duel never stops: the card you keep meets the favourite of one plausible taste drawn from the
belief (Thompson sampling), which balances showing likely winners with testing uncertain directions.
A card that wins four in a row joins tonight's shortlist. Within a night a title is never shown twice
(under any source's key, IMDb id or name and year) until every title for the filters has been seen,
then Picks says it is reshuffling. Moods (chips or typed words) are embedded by the same model as
search queries and steer tonight; the same mood words describe what tonight leans to and how a card
feels. Every signal is kept in a local event log (`picks-brain-events.jsonl`, titles' features in
`picks-brain-items.json`, never add-on URLs) and replayed, so a new projection or model loses nothing.
Genre, type and decade weights of the earlier `picks-taste.json` profile seed lasting taste. Not for me
removes a title from Picks for good; Settings > Learned taste forgets everything. When TMDB knows a
title, its genres decide whether it is animated (catalogues sometimes tag live-action films Animation).
Continue preserves the cumulative choice count, comparisons, current mood and lasting taste;
a checkpoint requires four fresh confirmations before another proposal. It never silently
starts a new duel when challengers run out. A bounded private `picks-decisions.json` records the
last 128 actions, titles, pre-vote margins, counts and stopping reasons, without provider keys,
URLs, credentials or raw metadata. Older round-by-round choices cannot be reconstructed.
Every third challenger tests a different genre direction when available. Play is always available.
S changes the pair without a vote; N clears tonight’s direction and seeks a different pair without
changing lasting preferences; C continues from a suggestion. Left/right arrows keep a card.
Keep this retains its arrow keycaps. Other controls use icons, accessible names and tooltips.
A keyboard icon beside Search opens shortcuts for the current page, including Search and title
pages. The player has the same icon beside Details and its own H / ? sheet; reading it does not
execute playback shortcuts. ? also opens the app’s page-specific shortcut sheet.
"Start over" resets learned preferences; it does not remove the model or embedding cache.

Style verification combines full add-on/TMDB genres with TVmaze's show type; missing animation
tags from a sparse catalogue are repaired before a card can be selected. Deduplication preserves
richer genre/style evidence and already open cards are rechecked when it arrives. The action bar
keeps Keep this, Play/Resume, trailer and details on one line; longer playback labels can shrink.

Duel cards and final picks have Play/Resume controls. New series begin at the first released
regular episode; existing progress resumes the saved episode and position, or continues after
the last finished episode. Hide watched removes completed films and series whose released
regular episodes are all watched. Started titles are included in the candidate pool so unfinished series can appear with Resume. The filter and
buttons refresh after playback and manual episode changes; progress writes finish before the
playback-completion refresh. These actions use the existing player and do not cast a duel vote.

Tests: `cargo test` in the repo root (core crates) and in `apps/veylo-dx`.

Licence: MIT. Archivo is under the SIL Open Font License. libmpv/FFmpeg ship as built by mpv-android (Android) and
mpv-winbuild-cmake (Windows), under their licences; the Media3 FFmpeg licences are in `tools/media3-ffmpeg/licenses`.
