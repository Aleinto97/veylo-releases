# Working on Veylo

Veylo is an all-Rust Dioxus app (`apps/veylo-dx`) for Android and Linux desktop on top of the `crates/core` data/network core. Start with [README.md](README.md).

## Where to work

- `apps/veylo-dx/src/`: screens (`home.rs`, `picks.rs`, `title.rs`, `library.rs`, ...), `backend/` for core access (`sources.rs` Play's choice, `metadata.rs` TMDB/BetterPosters/TVmaze, `store.rs` settings and saved state, `subtitles.rs`), `components.rs` for shared UI pieces, `picks_rules.rs` for Picks filtering, `styles/*.css`.
- `apps/veylo-dx/android/`: Kotlin player, Media3 FFmpeg glue, `jniLibs`, launcher `res`; `build.rs` copies them into the project dx generates.
- `crates/core/`: addon protocol, SQLite/history, recommendations, subtitles. `crates/media-proxy/`: loopback relay.
- `crates/video-surface/`: the desktop picture paths (Wayland, GL area, Windows, macOS). `crates/picture-check/`: CI's colour-bar picture test. Both belong to the `apps/veylo-dx` workspace, not the root one.
- `branding/`: logo sources. `tools/media3-ffmpeg/`: pinned audio-extension build.

## Change rules

1. Preserve pre-existing changes. Keep library/history migrations and provider URLs compatible. Use isolated temporary stores for tests, never the owner's database.
2. Never log or commit a configured addon URL, API key (TMDB key lives only in settings.json), token or full authenticated request.
3. UI copy is English; provider strings stay verbatim. Credentials are masked by default.
4. Delete files only after tracing their references. Preserve licences and reproducible build inputs.

## Verify

`cargo test` and `cargo clippy` in the root and in `apps/veylo-dx`. For device work, select a serial through `VEYLO_ADB_SERIAL`; do not hardcode a wireless-debugging address.

## Host filesystem caveat

On this host atomic new-file writers can leave broken `.l2s` links. Create files using direct Python `open(path, 'w')` or shell redirection.
