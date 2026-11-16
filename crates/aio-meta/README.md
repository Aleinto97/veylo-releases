# AIO Metadata (Rust) — in-app port

Personal-use Rust port of AIOMetadata (https://github.com/cedya77/aiometadata), embedded in Veylo as a loopback Stremio addon. It is installed beside Cinemeta and Kitsu; the existing addons are not replaced or removed.

## What is ported

- Stremio addon protocol: manifest.json, catalog, meta (metadata only; no streams or subtitles).
- TMDB catalogs (Popular, Trending, Top Rated, Anime, Search, Airing Today) and TVMaze Airing Today / series search; the TVMaze daily row drops news and talk shows.
- Canonical IMDb ids for metadata and episodes; TMDB/TVmaze ids are accepted as input.
- TVmaze episode lists (all seasons) with TMDB season fallback.
- TMDB localized titles and descriptions, artwork, credits, trailers and IMDb-compatible fields.
- TV-only operation without a TMDB key; movies require the key.
- In-memory TTL cache; the app core still owns its own SQLite cache.

## Not ported

Redis/Postgres, the React configuration UI, OAuth/Trakt/MDBList, TVDB/MAL/AniList/IMDb/Fanart providers, Jellyfin/SSO, the full upstream poster-cache pipeline (resizing, signing, shaped posters), personal lists, watch tracking, advanced id mapping and the full AIOMetadata feature set. This is a focused metadata-core port, not a drop-in replacement for the complete AIOMetadata product.

## Licence and attribution

GPL-3.0-only. This crate is a derivative work of AIOMetadata (cedya77/aiometadata, GPL-3.0); see LICENSE. The rest of Veylo remains MIT. Personal use only; review GPL obligations before distributing a build that includes this crate.

Metadata and artwork: TMDB. This product uses the TMDB API but is not endorsed or certified by TMDB.
TV data: TVMaze, CC BY-SA 4.0.

## Configuration

The Android settings screen stores a free TMDB API key in app-private preferences; a tmdb.key file in the app files directory and the VEYLO_META_TMDB_KEY or TMDB_API_KEY environment variables are also accepted. VEYLO_META_LANGUAGE (default en-US) and VEYLO_META_COUNTRY (default US) control localization and the TVMaze schedule country.

## Build

The crate is a workspace member and is compiled into librustmedia.so through crates/android-bridge. Run cargo test -p aio-meta for the host build. The Android addon starts only in the app process; it binds 127.0.0.1:7099 on the device (ephemeral fallback).

## Artwork proxy

Posters, backgrounds, logos and episode thumbnails are served by the addon itself, not hotlinked from TMDB/TVmaze. Artwork URLs use the legacy `/poster-cache/proxy/{class}/{type}/{id}` route that AIOMetadata exposes for proxied art. Its primary built-in form, `/poster-cache/<class>/<absolute-image-url>` plus `/w<width>/` resizing, is not implemented here.

The handler validates the upstream host (image.tmdb.org, static.tvmaze.com, images.metahub.space), streams at most 16 MiB per image (Content-Length is checked before the body), serves from a 128 MiB in-memory cache with a seven-day TTL and immutable Cache-Control, and redirects to the fallback only when the upstream is unavailable. This removes the extra direct CDN request from the client and keeps repeat artwork local.

TMDB art follows the official AIOMetadata selection: backdrops prefer textless (xx), then language-neutral, then the configured language, then the first image; posters and logos rank by configured language and vote. Sizes are w600_and_h900_bestv2 for catalog posters, w780 for detail posters, original for backgrounds and logos, and w500 for episode stills. TVmaze art uses the medium rendition for catalog rows and episode thumbnails (original for detail posters), and series hints leave defaultVideoId unset so clients pick the first/next episode.
