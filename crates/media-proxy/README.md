# Rust-owned native media relay

`rustmedia_media_proxy::MediaProxy` is an HTTP(S) transport boundary for libVLC
and native media engines. It owns upstream TLS, headers, redirects and supported manifests.

```rust,ignore
let proxy = MediaProxy::new(); // retain for the playback lifetime; also Default
let media = proxy.prepare(original_url, &headers_json, allow_http).await?;
// Exactly {url, lease_id, transport: "direct", via: "rust-proxy"}
proxy.stop(media["lease_id"].as_str()).await?;
// stop(None) stops the current lease; stale IDs do nothing.
```

Preparation binds loopback without fetching upstream, replacing one active
lease only after validation succeeds. A failed preparation preserves the previous
lease. Replacement, stop and Drop cancel the listener and every worker's request,
response and body IO. Cancellation is asynchronous, observed when tasks next run;
bounded manifest rewrite loops also check cancellation. Idle upstream connections
are not pooled across requests.

## Integration contract / security assertion

**Give native engines only the returned `http://127.0.0.1:port/...` capability URL.**
The bridge must supply `headers: {}` and `proxy_enforced: true`; those fields are
bridge assertions, not additional fields returned by this crate. All original
headers stay in Rust. Do not give native players an original HTTPS URL, headers,
engine options, or a fallback original URL on relay failure. Native engines must
not perform upstream TLS or receive untrusted remote subtitle/metadata/DRM URLs.
Torrent loopback HTTP URLs must also be wrapped in a lease.

**This assertion means relay-owned HTTP transport, not complete OS/network
isolation.** Binary containers and codec payloads can themselves embed external
references; this crate does not parse/rewrite all of them. Native demuxers,
plugins, metadata/cover-art/subtitle autoloaders, or independently configured
features may open other URLs. Disable those features or provide a separately
verified boundary. Do not claim arbitrary malicious native input cannot access
the network without a native protocol restriction or OS sandbox enforcing it.
Prefix detection is a conservative compatibility/security filter, not proof that
all native format autodetection or binary polyglots have been enumerated.

## HTTP and resource policy

* IPv4 `127.0.0.1` listener, ephemeral port, independent random 256-bit capability
  token and lease ID. Treat the URL as a bearer secret; neither relay nor callers
  should log URL/headers/tokens. Possession authorizes other HTTP(S) resources
  under that token, subject to consent and header isolation, not a path allowlist.
* Exact local Host, origin-form request target, GET/HEAD only. Reject duplicate
  headers, bodies, pipelining, controls and oversized requests. Request line plus
  headers: 8 KiB and an absolute 10-second deadline. Native auth/cookie/referer
  headers are never forwarded. Only Range, If-Range, If-None-Match and
  If-Modified-Since are forwarded from the engine.
* Eight active sockets, including slow header readers. Excess connections close
  immediately. Upstream connection/response headers have 10-second deadlines;
  body reads and client writes have 10-second idle deadlines reset on progress.
  Healthy long movies have no total-duration timeout. Disconnect cancels upstream
  IO and releases the slot. Requests use HTTP/1 with no idle connection pooling.
* HTTPS uses Rust reqwest/rustls verification. HTTP requires explicit consent,
  except literal loopback IPs and `localhost`. A loopback initial URL does **not**
  grant consent for remote HTTP descendants. Check at preparation, every redirect,
  every mapped manifest reference and each actual child request. DNS-resolved
  loopback addresses do not exempt other hostnames; no DNS pinning is provided.
* ALL configured headers, including unknown secret names and cookies, are sent
  only to the **initial original origin**, matched by scheme, normalized host,
  effective port. Cross-origin/port redirects and children never receive them;
  returning to the original origin can. A cookie jar is not enabled. Configuration
  is an object of strings or null, capped at 8 KiB. Controls, duplicate
  case-insensitive names, Host, hop-by-hop/framing/proxy-auth/encoding overrides
  are rejected. Engine range/cache validators override configured equivalents.
* Reject raw userinfo (including empty userinfo), backslashes, whitespace,
  controls, fragments, ambiguous authority syntax and non-HTTP schemes. Route
  origins must decode to their exact canonical origin representation. Upstream
  percent-escaped path/query bytes remain escaped; they are not URL-decoded into
  route separators or an alternate authority.
* System proxies, automatic redirects and every automatic decompressor are
  explicitly disabled, including under workspace feature unification.
  `Accept-Encoding: identity` is sent; nonidentity Content-Encoding fails closed
  even for progressive media, avoiding hidden playlists/length mismatches.
  At most eight redirects. Parsed upstream headers (including redirect headers)
  are capped at 8 KiB; critical duplicate headers and controls are rejected.
  The HTTP parser has its own bounded buffer before this application-level cap.
* Progressive binary bodies stream incrementally, with at most a prefix plus
  bounded reqwest chunks rather than a whole-file buffer. A text-like prefix is
  inspected up to 512 bytes; binary streaming needs only a minimum 32-byte
  signature prefix (or EOF), not a full file. ID3-prefixed bodies additionally
  inspect through at most 4 MiB/64 metadata tags and a 512-byte payload window,
  bounded to 4 MiB + 64 KiB accumulated bytes. This follows FFmpeg's tag stripping:
  ID3-wrapped text/playlists fail closed even at opaque binary-MIME redirect targets,
  while normal tagged binary audio still streams. Ranges splitting ID3 magic or
  ending inside metadata fail closed; a partial response must include the complete
  post-tag sniff window unless it reaches representation EOF. Native reconnects
  must not assemble a document from individually unclassified ID3 fragments.
  Preserve single-range 206 status, Content-Range, length, type, encoding and
  cache headers. Validate single-range span/total/length consistency. Multipart
  ranges and malformed 206 framing fail closed; Content-Range outside 206/416
  is rejected. The 16-byte AES exemption requires an entire 16-byte representation,
  never a 16-byte slice of a larger response. Small printable single-range
  binary-media payloads are allowed unless playlist-like. Text-like ranges can
  otherwise be conservatively rejected. A nonmanifest upstream error such as
  416 preserves status/Content-Range but has its unsafe error body discarded.
  Unknown-length successful bodies use connection-close framing.
* HEAD is forwarded as HEAD and never reads/forwards an upstream body. Known
  manifests/subtitles omit upstream length because validated/rewritten GET lengths
  can differ. Location,
  Set-Cookie, and other unapproved response headers never reach the engine.

## Text subtitles

Subtitle MIME types and extensions select validation, never an exemption from
manifest rewriting. HLS/DASH disguised as subtitles are still rewritten; a
binary/ID3 prefix followed by a `<tt/>` comment is not a subtitle signature.
WebVTT, SubRip and text-only TTML must pass **complete-document** validation before
any successful response headers or body bytes reach the native player. Recognized
WebVTT works even at an opaque URL with `application/octet-stream`; SRT and TTML
can likewise be recognized without a subtitle hint. Ordinary binary media remains
incrementally streamed, not buffered as a subtitle.

Subtitle input must be UTF-8, at most **4 MiB**, with at most one initial UTF-8 BOM.
Partial (206), truncated, malformed and oversized subtitle responses fail closed
with 502 and no unvalidated body. WebVTT requires a complete signature/header and
valid cue blocks/timestamps/settings throughout; numeric HLS `X-TIMESTAMP-MAP`
headers, inert NOTE blocks and basic cue formatting are supported, CSS STYLE and
REGION definition blocks are not. SubRip requires numbered, timed cues throughout.
Both formats cap their line indexes at 65,536 to bound newline-based allocation. Arbitrary trailing blocks, invalid
UTF-8/controls and non-whitelisted HTML cue markup are rejected. Valid WebVTT/SRT
bytes are preserved, with a format-correct response Content-Type.

TTML is parsed to EOF with quick-xml and canonically re-emitted with escaped text
and attributes, never accepted by a prefix/substring search. Only literal XML
whitespace, an optional initial XML 1.0 declaration and the `tt` document are
allowed; arbitrary prefixes, multiple roots and trailing payload are rejected.
Support is deliberately limited to unprefixed `tt/head/styling/layout/style/region/
body/div/p/span/br` elements, finite non-fetching timing/layout/style attributes,
and the standard default TTML namespace (or absent namespace). Only literal `tts`
and `ttp` prefixes bound at the root to the standard styling/parameter namespaces
are supported; aliases, extension namespaces and descendant namespace rebinding
are rejected. DTDs, processing instructions, external/unknown entities, profile
URLs, images, scripts, source/href/base attributes and URI-valued styling are
unsupported **anywhere in the document**, including beyond the 512-byte sniff
window. XML declarations/comments are dropped and CDATA is emitted as escaped
text. Limits: 64 nesting levels, 65,536 reader events, 64 attributes per element,
and **4 MiB canonical output**. This is a text-only compatibility subset, not full
TTML schema validation. See [subtitle validation](src/subtitle.rs).

## HLS and DASH

Content-Type and filename hints are supplemented by byte-prefix detection:
extensionless/mislabelled `#EXTM3U` and MPD XML are rewritten, not blindly relayed.
Invalid UTF-8 later in a binary chunk cannot hide an earlier playlist header.
Apart from fully validated text subtitles, unknown UTF-8/plain-ASCII text, HTML,
JSON, arbitrary XML, detected legacy
M3U/PLS/SMIL, detected
UTF-16/32 markup and gzip payloads are denied rather than delegated to native
playlist parsers. Compressed responses are unsupported. Very text-like binary
prefixes or unusual encodings can consequently be rejected. Unrecognized
non-UTF-8 data can stream as binary (including opaque AES keys); it shares the
native format-autodetection/embedded-reference limitation above.

HLS maps segment/variant lines and quoted URI attributes into absolute loopback
URLs. Supported tags cover keys/maps, alternate renditions, I-frame/image
playlists, LL-HLS parts/preload hints/rendition reports and ordinary timing/range
metadata. Relative, root-relative, scheme-relative and absolute references use
the **final** upstream manifest URL as base. Native options/comments such as
EXTVLCOPT/KODIPROP are stripped. A strict tag/attribute allowlist rejects unknown
extensions, malformed/duplicate/unquoted URI attributes, variables, steering,
session data, interstitial assets (including X-ASSET-LIST), and vendor metadata.
Only identity AES-128 encryption or NONE is supported; SAMPLE-AES/DRM is rejected.

DASH parses and re-emits XML, decoding predefined/numeric entities before policy
checks and escaping output. Support includes one inherited BaseURL per MPD,
Period, AdaptationSet or Representation; SegmentList, SegmentBase,
SegmentTimeline and segment URL attributes. All URL attributes and BaseURLs are
local; URI-relative path segments and query escapes retain URL join semantics.
`$Number$`, `$Time$`, `$Bandwidth$`, `$RepresentationID$` and zero-padded numeric
`%0Nd` templates (width 1–20) are supported in segment path/query attributes only.
Representation IDs must be non-dot ASCII alphanumeric/dot/underscore/hyphen.
Authority/BaseURL templates, unknown/unbalanced tokens, literal-dollar escaping
and ambiguous formatting fail closed.

DASH permits the standard default MPD namespace (or absent namespace), not
prefixed elements/extension namespaces, XLink, xml:base, DTD, external entities,
processing instructions, alternative BaseURLs, unknown attributes/elements,
Location/PatchLocation, UTCTiming, ContentProtection/license URLs or event streams.
Only explicitly known non-fetching role/audio/channel/caption/trick-mode/switching
and related descriptor schemes/values are accepted; chaining/external metadata
schemes are rejected. This is deliberately a subset of DASH, not full XML schema
or standards conformance. See the allowlists in [manifest implementation](src/manifest.rs).

Manifest input is UTF-8 and at most **4 MiB**, rewritten output at most **16 MiB**.
DASH adds 64 nesting levels and 65,536 XML reader events. Malformed, partial (206),
non-success and unsupported manifests fail closed with 502; an error response is
never a signal to give the original URL to native code. Full playlists/MPDs rather
than byte-range manifests are required. No persistent cache, DRM acquisition,
websocket/P2P upstream transport or automatic retries are implemented. IPv6
upstreams are supported; the local listener is IPv4 only. Local URLs still must
fit the 8 KiB request budget.

## Validation

```sh
cargo test -p rustmedia-media-proxy --locked --offline
cargo clippy -p rustmedia-media-proxy --all-targets --locked --offline -- -D warnings
```

Tests use loopback-only fixtures, no external network or P2P. They cover
request/token/Host/method/header validation, canonical decoded origins and percent
escapes, consent and redirects, cookie/unknown-secret isolation across host/port,
Range/HEAD, unknown MIME and malformed/wide/compressed text, HLS/DASH references,
unsupported metadata/templates/namespaces, manifest expansion/input/XML limits,
incremental streaming, stop/replacement/stale IDs, response-header cancellation,
slot saturation and 10-second header/body idle expiry. Real relay regressions cover
ID3-prefixed HLS with an immediate `<tt/>` comment and `.m3u8` → `.vtt` redirect,
full-document subtitle grammar, TTML DTD/reference/namespace alias attacks before
and after the sniff window, malformed XML/trailing payload, split signatures and
UTF-8/chunks, subtitle input/output/nesting/event/line limits, valid HLS WebVTT
clock mappings, mislabeled/redirected ID3-wrapped playlists, and preserved streaming
of ID3-tagged audio and fully inspected binary ranges. Tiny/reconnecting ID3
fragments (including 16-byte fragments that might resemble AES keys) are rejected
before any original bytes are exposed; this is relay-level evidence, not a live
native-decoder/network-exploit execution.
