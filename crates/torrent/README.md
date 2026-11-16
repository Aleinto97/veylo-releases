# Isolated torrent adapter

`stremio_torrent::TorrentEngine` is `Send + Sync`. `new(PathBuf)` creates no runtime or network tasks. Call its async methods from the bridge-owned **multithread Tokio runtime** (two workers are sufficient). Never call through a per-byte JNI interface.

## Contract

- `prepare(Value) -> anyhow::Result<Value>` accepts `{ "infoHash": "40 hexadecimal characters", "fileIdx": 0, "sources": ["tracker:https://...", "tracker:udp://..."] }`.
- `fileIdx` is the original metadata index. Omit it to choose the largest file, first original index on ties. Indices are never sorted/remapped.
- `tracker:` is stripped and its URL encoded into magnet `tr=` parameters. `dht:` sources are explicitly returned in `ignored_sources`; a bogus DHT hash is not treated as a tracker or peer. Ordinary hash-based DHT remains enabled.
- Returns `{url, lease_id, mime_type, file_name, file_length, file_index, info_hash, transport, cache_reserved_bytes, ignored_sources}`. The URL is an ephemeral `http://127.0.0.1:PORT/stream/RANDOM_192_BIT_TOKEN` lease.
- `stop() -> Result<()>` cancels transfers, closes listener/readers, stops the **whole session including DHT**, and deletes that inactive lease. Idempotent. A replacement prepare stops the previous lease.
- `stats() -> Result<Value>` returns `{active, cache_budget_bytes, lease_id?, torrent?}` with librqbit counters. Stats/lifecycle calls serialize behind a prepare; stop first cancels an epoch shared by every already-started or queued prepare, including replacements waiting on old shutdown. Prepares begun while any overlapping stop is in progress are also cancelled. After all stops finish, a new prepare may start.
- URL-only HTTP streams return an explicit error here; direct HTTP(S) playback belongs to core/Media3. This crate supplies the real P2P-to-HTTP transport, not a fake P2P result.

Only GET/HEAD, exact token path, exact Host including port, and loopback peers are admitted. Content-Length, MIME, Accept-Ranges, 200/206/416, open-ended and suffix ranges are supported. Multiple ranges are rejected with 416. No control endpoints, CORS, UPnP forwarding, incoming peer TCP listener, persistent DHT, auto-resume, or post-stop seeding. At most four concurrent HTTP readers. A 60-second body-idle deadline bounds missing-piece reads, seeks, and blocked socket writes; progress resets it, so there is no total stream-duration limit. A disconnected client waiting for pieces releases its reader/slot within that deadline, not necessarily immediately. TCP write-half-close remains supported. A still-connected client stalled beyond the deadline must retry (for example with Range). Reader timeout does not stop the entire playback lease; the player must call stop on release/backgrounding. Drop cancels networking and attempts asynchronous cleanup while the runtime remains alive.

## Storage admission

Resolve metadata using `list_only` before admitting payload. Reject unsafe path components, symlink metadata, more than 512 files, more than 32 MiB resolved metadata, and pieces larger than 16 MiB. Admit only the selected original index; librqbit downloads its intersecting boundary pieces. Default budget is **8 GiB selected file plus two pieces**, with **1 GiB free-space reserve**. `TorrentConfig` and `with_config` configure these limits and preparation timeout (default 90 seconds).

No rolling cache. Filesystem storage opens empty unselected files; boundary pieces can sparsely extend neighbor files. Budget concerns physical selected/boundary payload, not sparse apparent lengths. Private 0700 root, random per-lease directories, exclusive lockfiles, and cleanup restricted to recognized inactive unlocked leases prevent deleting an active lease. Supply an app-private cache directory; do not share it with untrusted filesystem writers. Free space is checked at admission, not continuously reserved against unrelated applications or simultaneous independent engines. One engine/active player per app is the intended integration.

## Pinned library and TLS proof

Exact published `librqbit = 8.1.1`, Apache-2.0, defaults disabled, `rust-tls` enabled. The generated lock resolves reqwest **0.12.28**, `rustls-tls-webpki-roots` and ring: embedded Mozilla/webpki roots, **no Android Kotlin/JNI platform-verifier initialization** and **no OpenSSL**. `cargo tree -e features -i webpki-roots` verifies the path. The transitive `librqbit-sha1-wrapper 4.1.0` feature named `sha1-ring` actually uses **aws-lc-rs 1.18.2** for SHA1; ARM builds must support its native C/ASM build in addition to ring. This is unrelated to Android trust/JNI.

Upstream has hardcoded **128 concurrent metadata resolver connections AND 128 live peer permits**. Neither cap has a public lower-limit option in 8.1.1. This is a known mobile resource gap, not a claimed configured peer limit. Admission guards occur after upstream parses resolved metadata; the upstream metadata resolver may allocate before our 32 MiB guard. librqbit stop cancels its tasks and uses an upstream one-second grace period rather than providing a full task-join proof. Device/instrumented Media3 playback, background lifecycle, metered consent, sustained throughput, and ARM linkage are integration validation, not established by host tests.

## Tests

`cargo test --manifest-path crates/torrent/Cargo.toml --offline` (once added to the parent workspace) runs helper/lifecycle/cache tests and an entirely local hash-verified file test of HTTP status, ranges, Host, token/method gating, HEAD, and listener cancellation. Deterministic regressions hold replacement shutdown behind a channel, overlap/abort stops with queued prepares, and use mocked stalled readers plus loopback clients to verify all four HTTP slots recover. Virtual-clock body tests cover read/write idle expiry and streams longer than the idle deadline. It does not require a public swarm. The public-swarm smoke is ignored and separately consent-gated:

```sh
STREMIO_TORRENT_SMOKE_CONSENT=I_HAVE_PERMISSION \
STREMIO_TORRENT_SMOKE_JSON='{ "infoHash": "YOUR_LAWFUL_40_HEX_HASH", "sources": ["tracker:https://YOUR_TRACKER/announce"] }' \
cargo test --manifest-path crates/torrent/Cargo.toml --test lawful_swarm -- --ignored
```

The smoke refuses selected payload+boundaries over 64 MiB and requests only 16 response bytes (the torrent downloader may concurrently fetch up to the admitted small file). No public swarm is downloaded by default.

**Workspace integration:** the temporary standalone `[workspace]` table has been removed after scoped tests. The lead can add this crate as a root member and owns the root manifest/lock. Preserve exact librqbit version and verify the unified root dependency tree still excludes platform-verifier/OpenSSL. The nested lockfile records standalone tested versions and is not used by Cargo after workspace integration.
