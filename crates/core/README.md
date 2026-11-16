# RustMedia core

`rustmedia_core::Core::open(data_dir)` performs atomic SQLite migrations and installs built-in Cinemeta/Kitsu **descriptors**, without making network requests. `Core` is cloneable, `Send + Sync`; `execute(Value)` is async and must run on a Tokio runtime. It returns `anyhow::Result<Value>`; transport errors deliberately omit configured URLs. No SQLite guard crosses an await.

## JSON protocol

Requests are objects with `op` and the fields below. Unknown response/stream fields are retained unless explicitly noted. Types and IDs are raw Stremio strings, not inferred from names.

| Operation | Request fields | Response |
|---|---|---|
| `addons` | — | `{addons:[manifest + manifest_url]}` |
| `install_addon` | `url`, optional `allow_http` | Manifest descriptor directly |
| `remove_addon` | `url` | `{removed:bool}` |
| `refresh_addons` | — | `{addons:[...],results:[{addon,error?}]}`; independent failures retain old descriptors; removed/reinstalled generations ignore stale responses |
| `catalog` | `addon_url,type,id,extra?:object,force?:bool` | `{metas:[],cached:bool,...}` |
| `meta` | `addon_url,type,id,force?:bool` | `{meta:object,cached:bool,...}` |
| `streams` | `type,id,addon_url?` | `{groups:[{addon_url,addon_name,streams:[raw objects],error?}]}` |
| `subtitles` | `type,id,extra?:object,addon_url?` | `{subtitles:[raw objects],errors:[]}` |
| `search` | `query,extra?:object` | `{groups:[{addon_name,addon_url,catalog_id,type,metas,cached?,stale?,error?}]}` |
| `library` | — | `{items:[metadata + state]}` for saved/history/feedback rows |
| `set_library` | `type,id,addon_url,saved,meta?` | `{item:metadata + state}` |
| `playback` | `type,id,addon_url,video_id` | `{video_id,position_ms,duration_ms,watched_ms,watched}` for that exact video; unknown returns zeros/false |
| `progress` | `type,id,addon_url,position_ms,duration_ms,video_id?,watched_ms?,meta?` | `{item:metadata + state}` |
| `mark_watched` | `type,id,addon_url,watched,meta?` | `{item:metadata + state}` |
| `feedback` | `type,id,addon_url,value` (-1, 0 or 1) | `{item:metadata + state}` |
| `recommendations` | `limit?` (default 24, cap 100) | `{metas:[metadata + reason],needs_history,source}` |
| `similar` | `type,id,addon_url,limit?` | Same; `missing_metadata:true` and empty results if seed is not cached |
| `collections` | — | `{collections:[{id,name,kind,metas}]}` |
| `personal` | `limit?` (default 24, cap 100) | `{library:[metadata + state],recommendations:[metadata + state + reason],collections:[{id,name,kind,metas}],needs_history:bool}` from one media snapshot |
| `diagnostics` | — | Schema/count/concurrency/cache diagnostics; no request URLs |

`personal` is additive; `library`, `recommendations` and `collections` keep their existing responses. Its arrays exactly match those three operations on the same state, including ordering, metadata, reasons and collection membership. Only recommendations are limited; library and collections remain complete. `limit` must be a JSON nonnegative integer (zero returns no recommendations), defaults to 24, and is capped at 100. `needs_history` matches `recommendations.needs_history`; `personal` adds no `source` field or nesting.

Mutation `meta` must be an object when supplied. Omitted `meta` and `{}` both mean merge no fields, **not** clear metadata: an existing row's metadata and addon provenance are preserved without a metadata rewrite. A missing row gets minimal `id`/`type` metadata. Nonempty metadata keeps existing merge/null/source-switch rules and recursively strips streams before persistence. Mutations still return `{item:metadata + state}`. Their return lookup uses the existing primary-key index; standalone library filtering happens in SQL before metadata parsing, with the same saved/watched/position/feedback/measured-time predicate and `updated DESC,key` order.

State fields are `library`, `watched`, `video_watched`, `position_ms`, `duration_ms`, `watched_ms`, `liked` (-1/0/1), `addon_url`, and optional latest `video_id`. History stays editable after an addon is removed. Metadata updates do not erase user state. Arbitrary IDs include addon scope and media type; established `tt<digits>` and `kitsu:<digits>` namespaces deduplicate across addons **within the same media type**, with the complete raw ID retained. Kitsu is never silently converted to IMDb.

`watched_ms` is **cumulative measured playback time for that video**, not a delta and not a seek position. Repeated reports use the maximum measured time per video. Omission records zero additional evidence. Auto-watch requires at least 80% measured viewing AND a position at least 90% through; explicit `mark_watched` is also available. Completion is sticky **per video** and uses the retained maximum measured time, including when later reports omit or lower `watched_ms`. Automatic completion sets title-level `watched` only for `type:"movie"`; completing a series/anime episode does not mark the whole title watched. `mark_watched` remains a title-level toggle and does not change any video's evidence/completion. `video_watched` describes only the latest `video_id` and is false when no playback exists. The item's `position_ms`/`duration_ms` and `video_id` describe the latest reported video; its `watched_ms` aggregates measured evidence across all videos. Use `playback` (not the library aggregate) to resume a selected episode: it returns that video's own evidence and completion, without writing anything or requiring the addon to remain installed. Continue watching excludes a title-level watched item or completed current video; reporting progress on an unfinished next episode makes an unmarked series eligible again.

Schema v2 adds `playback.watched` and an opaque `addons.generation` token. The v1 migration preserves all media, playback, cache, addon and consent data and backfills per-video completion using the same thresholds. **Legacy ambiguity:** v1 did not distinguish manual title marking from automatic episode completion. Existing title-level `watched` flags are therefore preserved rather than erasing possibly intentional manual marks; clear an unwanted legacy series mark with `mark_watched:false`. Fresh episodic completions do not create such flags. A database from a newer schema is rejected without downgrading.

## Addon transport and storage

- HTTPS by default; `stremio://` becomes HTTPS. HTTP requires loopback or explicit `allow_http:true` during install. Consent also applies to redirects; credentials in URL authority are rejected.
- Configured manifest path and query are preserved. Resource path segments and extras are UTF-8 percent-encoded (including slash, ampersand and equals inside values).
- Stream/subtitle requests match manifest resources, media types and ID prefixes. Catalog required extras are validated before network work. Search skips catalogs whose other required extras are not supplied. Optional search `extra` can supply those values.
- Explicit install is an upsert with a fresh persisted generation token. Refresh only conditionally updates the captured generation; it never inserts and cannot resurrect removed addons or overwrite an intervening remove/reinstall, explicit reinstall, or newer successful refresh. Ignored responses are reported as per-addon errors.
- At most eight HTTP requests per Core run concurrently; independent addon failures do not discard successful groups. Connect timeout is 5s; request timeout is 12s; redirects are capped at three; responses at 8 MiB.
- Catalog default TTL is 15 minutes; metadata default is 6 hours. All `Cache-Control` field lines are combined before applying `no-store`, `private`, `no-cache` and TTL policy. `max-age` is honored with a 24-hour cap (the minimum if repeated), along with `Age`; field-qualified `private`/`no-cache` are treated conservatively. Offline/error fallback is explicitly labeled `{cached:true,stale:true,error}`. `force:true` revalidates but can still return labeled stale data on failure.
- Resource cache is bounded to 500 entries / 64 MiB of raw bodies. The metadata recommendation index retains at most 10,000 inactive candidates; saved/history/feedback rows are retained. Expired metadata can remain available for explicit stale fallback.
- Streams are cached only in volatile memory, for 60s by default and at most 180s. They are never inserted into SQLite or logged. Subtitles are fetched without persistent caching. Stream `name`, `title`, `description`, line breaks, Unicode and unknown fields are untouched. Volatile stream cache is bounded to 64 entries / 16 MiB. Responses containing inline `streams` keys are returned untouched and are never resource-cached: serving a stripped response would violate exclusive inline-stream semantics. Only a recursively sanitized metadata index/history is persisted for ranking. The UI should use fresh inline streams when provided instead of querying stream addons.
- Use an app-private `data_dir`. Configured addon URLs/descriptors and ordinary metadata are persisted in SQLite, not encrypted by this crate. The Android layer must protect that directory and avoid credential-bearing addon URLs in logs/backups. No embedded third-party provider tokens exist.

## Recommendations and collections

Ranking uses only cached content and local saved/watched/liked/measured-viewing signals: shared genres/tags, director and cast, with type preference and negative-feedback penalties. Reasons explain actual shared fields. Cold start sets `needs_history:true`; there is no hidden network discovery, cloud recommender or account sync.

Collections include watchlist, continue-watching, watched and liked rules; genre/tag and director buckets require at least two cached titles and activity in the bucket. Franchise collections require explicit addon metadata objects (`collection`, `belongsToCollection` or `franchise`) with an ID and name, scoped to addon and media type. Title similarity alone never fabricates a franchise.

This crate implements addon protocol/state, not torrent resolution, video decoding, DRM, an Android UI, or full Stremio parity. Bootstrap descriptors should be refreshed to obtain current public manifests. No external provider availability is required by tests.

## Verification

```sh
cargo test -p rustmedia-core
cargo fmt -p rustmedia-core -- --check
cargo clippy -p rustmedia-core --all-targets -- -D warnings
```

Integration tests run an in-process minimal loopback HTTP server and temporary scoped SQLite stores, never real internet addons.


### Optional local SQLite benchmark

```sh
# Baseline command (before production optimization; skips the not-yet-added personal op):
cargo test -p rustmedia-core --locked --test performance -- --ignored --nocapture
# Same workload after optimization, plus the additive single-call refresh:
CORE_BENCH_PERSONAL=1 cargo test -p rustmedia-core --locked --test performance -- --ignored --nocapture
```

The ignored benchmark creates a fresh temporary store under this crate's ignored `target/bench-data`, seeds 6,000 ordinary metadata rows directly in a SQLite transaction, and cleans up only its checked per-process directory. It uses no network, new dependency or migration. Setup and one warm-up progress call are excluded. The measured workload is 100 progress calls alternating omitted `meta`/`{}`, then eight refreshes using `library` + `recommendations(limit:20)` + `collections`. The post-change run also measures eight `personal(limit:20)` calls and asserts full value equivalence; no timing assertions exist.

Measured on this development host (`aarch64`, rustc 1.98.1, unoptimized Cargo test profile):

| Workload | Before, total / per operation | After, total / per operation |
|---|---|---|
| 100 progress writes | 10,612.568 ms / 106.126 ms | 58.806 ms / 0.588 ms |
| Eight legacy three-call refreshes | 3,421.550 ms / 427.694 ms | 2,635.146 ms / 329.393 ms |
| Eight single-call personal refreshes | Not available | 1,811.257 ms / 226.407 ms |

A second post-change run produced 88.403 ms / 0.884 ms for progress, 2,656.192 ms / 332.024 ms for the legacy refresh, and 1,815.588 ms / 226.948 ms for personal. These local samples show roughly 120–180× faster progress and a 47% lower personal refresh time than the pre-change three-call path (31–32% below the optimized three-call path). There was one baseline run and two post-change runs, not a statistically controlled comparison. Timings are illustrative, not Android/release/p95 predictions: file caches are warm, hardware/storage and concurrent host load matter, and only one sparse-history dataset is tested. New-personal timing conservatively includes deep equality checks; both refresh paths construct the same flat result. Ranking and collections still scan the full snapshot; recommendation affinity still scales with candidates × history signals. Returning an item necessarily parses its one metadata row; only the redundant metadata-update parse/merge/serialize and unrelated-row scans have been removed. No schema/index migration or new dependency was needed.

Lead verification with the same command produced 101.151 ms total / 1.012 ms per progress call, 2,937.471 ms total / 367.184 ms per legacy refresh, and 1,879.763 ms total / 234.970 ms per personal refresh. A concurrent native release build was running on this host; this is an additional non-isolated sample, not a replacement baseline. The broad reduction remains approximately 100× for progress and 45–47% for the new personal refresh versus the original three-call baseline.
