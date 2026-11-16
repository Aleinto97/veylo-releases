//! A cancellation-scoped HTTP loopback relay. Native players must receive ONLY
//! the returned URL, never original headers. See README for manifest limitations.
use anyhow::{anyhow, bail, ensure, Context, Result};
use rand::{rngs::OsRng, RngCore};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client, Method, Response,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use url::Url;

mod manifest;
mod subtitle;
const HEADER_LIMIT: usize = 8192;
const MANIFEST_LIMIT: usize = 4 * 1024 * 1024;
/// Connection setup, headers and error responses must be prompt.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Streaming body reads/writes must tolerate CDN cache misses on large files.
/// 10s was too short for 4K remuxes and killed live playback mid-stream.
const STREAM_TIMEOUT: Duration = Duration::from_secs(60);
const SLOTS: usize = 8;
/// Smallest size a probed source may have to count as a film (status clips are smaller).
const PROBE_MIN_BYTES: u64 = 50 * 1024 * 1024;
/// Pause after a transient accept() failure (EMFILE, ECONNABORTED) instead of
/// abandoning the lease's listener for good.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// Own one active media lease. Replacement, stop and Drop cancel all lease IO.
pub struct MediaProxy {
    active: Mutex<Option<Active>>,
    client: Client,
    /// Per-read/write idle deadline for streaming bodies. Injectable so tests can
    /// verify expiry without waiting out the production CDN-miss allowance.
    stream_timeout: Duration,
}
struct Active {
    id: String,
    cancel: CancellationToken,
    source_bytes: Arc<AtomicU64>,
    debrid_status: Arc<Mutex<Option<&'static str>>>,
}
struct Lease {
    token: String,
    authority: String,
    original: Url,
    headers: HeaderMap,
    allow_http: bool,
    cancel: CancellationToken,
    client: Client,
    stream_timeout: Duration,
    /// Total size of the source file as the server reports it; 0 = unknown.
    source_bytes: Arc<AtomicU64>,
    /// Set when the source redirected to a debrid status clip instead of the file.
    debrid_status: Arc<Mutex<Option<&'static str>>>,
}
impl Default for MediaProxy {
    fn default() -> Self {
        Self::new()
    }
}
/// Validate every configured header (types, reserved names, total size) without
/// creating a lease. The Android bridge uses this to reject a malformed
/// replacement before it tears down the currently playing relay.
pub fn validate_headers(value: &Value) -> Result<()> {
    scoped_headers(value).map(|_| ())
}
/// Apply exactly the source-URL rules `MediaProxy::prepare` enforces (canonical
/// authority, no credentials/fragment/backslash, scoped HTTP consent) so the
/// bridge can reject a replacement before stopping the current relay.
pub fn validate_source(url: &str, allow_http: bool) -> Result<String> {
    validate_reference(url)?;
    let url = Url::parse(url).context("invalid media URL")?;
    validate_url(&url, allow_http)?;
    Ok(url.to_string())
}

impl MediaProxy {
    pub fn new() -> Self {
        Self::with_stream_timeout(STREAM_TIMEOUT)
    }
    /// Production uses the generous CDN-miss allowance; tests may shorten it to
    /// observe idle expiry in real time without a slow suite.
    pub fn with_stream_timeout(stream_timeout: Duration) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            // Explicit even if another workspace dependency enables these features.
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .http1_only()
            .pool_max_idle_per_host(0)
            .connect_timeout(IO_TIMEOUT)
            .build()
            .expect("rustls client initialization");
        Self {
            active: Mutex::new(None),
            client,
            stream_timeout,
        }
    }
    /// Validate before replacing the previous lease. No upstream request occurs here.
    pub async fn prepare(&self, url: &str, headers: &Value, allow_http: bool) -> Result<Value> {
        validate_reference(url)?;
        let url = Url::parse(url).context("invalid media URL")?;
        validate_url(&url, allow_http)?;
        let headers = scoped_headers(headers)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let authority = listener.local_addr()?.to_string();
        let token = random_id();
        let id = random_id();
        let cancel = CancellationToken::new();
        let source_bytes = Arc::new(AtomicU64::new(0));
        let debrid_status = Arc::new(Mutex::new(None));
        let lease = Arc::new(Lease {
            token,
            authority,
            original: url.clone(),
            headers,
            allow_http,
            cancel: cancel.clone(),
            client: self.client.clone(),
            stream_timeout: self.stream_timeout,
            source_bytes: Arc::clone(&source_bytes),
            debrid_status: Arc::clone(&debrid_status),
        });
        let local = lease.local_url(&url)?;
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow!("proxy state unavailable"))?;
        if let Some(old) = active.replace(Active {
            id: id.clone(),
            cancel,
            source_bytes,
            debrid_status,
        }) {
            old.cancel.cancel();
        }
        tokio::spawn(serve(listener, lease));
        Ok(json!({"url":local,"lease_id":id,"transport":"direct","via":"rust-proxy"}))
    }
    /// Size of the current source file from the server's own headers
    /// (Content-Range total or a full Content-Length), once it has been read.
    pub fn source_size(&self) -> Option<u64> {
        let active = self.active.lock().ok()?;
        let bytes = active.as_ref()?.source_bytes.load(Ordering::Relaxed);
        (bytes > 0).then_some(bytes)
    }
    /// Checks that a source really delivers a film before playback: one byte
    /// is requested through the same redirect, header and consent rules as
    /// the relay. Debrid status clips, error statuses and tiny files fail.
    /// Returns {"ok", "status", "bytes", "debrid_status"}.
    pub async fn probe(&self, url: &str, headers: &Value, allow_http: bool) -> Result<Value> {
        validate_reference(url)?;
        let original = Url::parse(url).context("invalid media URL")?;
        validate_url(&original, allow_http)?;
        let probe = Lease {
            token: random_id(),
            authority: "probe.invalid".into(),
            original: original.clone(),
            headers: scoped_headers(headers)?,
            allow_http,
            cancel: CancellationToken::new(),
            client: self.client.clone(),
            stream_timeout: self.stream_timeout,
            source_bytes: Arc::new(AtomicU64::new(0)),
            debrid_status: Arc::new(Mutex::new(None)),
        };
        let mut range = HeaderMap::new();
        range.insert("range", HeaderValue::from_static("bytes=0-0"));
        let (response, final_url) = probe.fetch(original, Method::GET, &range).await?;
        if let Some(status) = debrid_status_clip(&final_url) {
            return Ok(
                json!({"ok": false, "status": response.status().as_u16(), "bytes": null, "debrid_status": status}),
            );
        }
        let headers = response.headers();
        let status = response.status().as_u16();
        let bytes = match status {
            206 => content_range_total(headers),
            200 => headers
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok()),
            _ => None,
        };
        // Status clips and error pages are a few MB at most; a film is not.
        let ok = matches!(status, 200 | 206) && bytes.is_none_or(|n| n >= PROBE_MIN_BYTES);
        Ok(json!({"ok": ok, "status": status, "bytes": bytes, "debrid_status": null}))
    }
    /// "downloading" or "failed" when the current source turned out to be a
    /// debrid status clip (torrent not cached yet, cancelled, over a limit…).
    pub fn debrid_status(&self) -> Option<&'static str> {
        let active = self.active.lock().ok()?;
        let status = active.as_ref()?.debrid_status.lock().ok()?;
        *status
    }
    /// None stops the current lease; a stale ID deliberately does nothing.
    pub async fn stop(&self, lease_id: Option<&str>) -> Result<()> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow!("proxy state unavailable"))?;
        if active
            .as_ref()
            .is_some_and(|a| lease_id.is_none_or(|id| id == a.id))
        {
            if let Some(old) = active.take() {
                old.cancel.cancel();
            }
        }
        Ok(())
    }
}
impl Drop for MediaProxy {
    fn drop(&mut self) {
        if let Ok(active) = self.active.get_mut() {
            if let Some(a) = active.take() {
                a.cancel.cancel();
            }
        }
    }
}
fn random_id() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> Result<String> {
    ensure!(
        s.is_ascii() && s.len() <= 2048 && s.len() % 2 == 0,
        "bad route origin"
    );
    let bytes = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(String::from_utf8(bytes)?)
}
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host() == b.host()
        && a.port_or_known_default() == b.port_or_known_default()
}
fn validate_reference(reference: &str) -> Result<()> {
    ensure!(
        !reference.bytes().any(|b| b <= 32 || b == 127 || b == b'\\'),
        "URL whitespace/control/backslash is forbidden"
    );
    ensure!(!reference.contains('#'), "URL fragments are unsupported");
    // Reject noncanonical authority syntax BEFORE the URL parser erases empty
    // userinfo, extra slashes or special-scheme normalization differences.
    let authority = if let Some(rest) = reference.strip_prefix("//") {
        Some(rest)
    } else if let Some((scheme, rest)) = reference.split_once(':') {
        if !scheme.contains(['/', '?']) {
            ensure!(
                scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"),
                "unsupported URL scheme"
            );
            Some(
                rest.strip_prefix("//")
                    .ok_or_else(|| anyhow!("URL authority required"))?,
            )
        } else {
            None
        }
    } else {
        None
    };
    if let Some(authority) = authority {
        let authority = authority.split(['/', '?']).next().unwrap_or("");
        ensure!(
            !authority.is_empty() && !authority.contains('@'),
            "invalid URL authority/userinfo"
        );
    }
    Ok(())
}
fn validate_url(url: &Url, allow_http: bool) -> Result<()> {
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are forbidden"
    );
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    ensure!(url.host().is_some(), "URL must have a host");
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && (allow_http || loopback)),
        "HTTP requires explicit consent; unsupported URL scheme"
    );
    Ok(())
}
fn scoped_headers(value: &Value) -> Result<HeaderMap> {
    if value.is_null() {
        return Ok(HeaderMap::new());
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("headers must be an object"))?;
    let mut out = HeaderMap::new();
    let mut total = 0;
    for (key, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| anyhow!("header values must be strings"))?;
        ensure!(
            !key.bytes().chain(value.bytes()).any(|b| b < 32 || b == 127),
            "header control character"
        );
        let name = HeaderName::from_bytes(key.as_bytes())?;
        ensure!(
            !matches!(
                name.as_str(),
                "host"
                    | "connection"
                    | "proxy-connection"
                    | "proxy-authorization"
                    | "transfer-encoding"
                    | "content-length"
                    | "te"
                    | "trailer"
                    | "upgrade"
                    | "accept-encoding"
            ),
            "reserved relay header"
        );
        total += key.len() + value.len() + 4;
        ensure!(total <= HEADER_LIMIT, "upstream headers too large");
        ensure!(!out.contains_key(&name), "duplicate configured header");
        out.insert(name, HeaderValue::from_str(value)?);
    }
    Ok(out)
}
impl Lease {
    fn local_url(&self, url: &Url) -> Result<String> {
        validate_url(url, self.allow_http)?;
        let origin = url.origin().ascii_serialization();
        let mut result = format!(
            "http://{}/{}/{}/{}",
            self.authority,
            self.token,
            hex(origin.as_bytes()),
            url.path().strip_prefix('/').unwrap_or(url.path())
        );
        if let Some(q) = url.query() {
            result.push('?');
            result.push_str(q);
        }
        Ok(result)
    }
    fn target(&self, path: &str) -> Result<Url> {
        ensure!(path.starts_with('/'), "request target must be origin-form");
        let rest = path
            .strip_prefix(&format!("/{}/", self.token))
            .ok_or_else(|| anyhow!("unknown token"))?;
        let (origin, path) = rest.split_once('/').ok_or_else(|| anyhow!("bad route"))?;
        ensure!(
            origin.bytes().all(|b| b.is_ascii_hexdigit()),
            "bad route origin"
        );
        let origin = unhex(origin)?;
        let parsed = Url::parse(&origin)?;
        ensure!(
            parsed.origin().ascii_serialization() == origin,
            "noncanonical origin"
        );
        ensure!(
            !path.bytes().any(|b| b <= 32 || b == 127 || b == b'\\') && !path.contains('#'),
            "invalid route path"
        );
        let target = Url::parse(&format!("{origin}/{path}"))?;
        ensure!(same_origin(&parsed, &target), "route changed origin");
        validate_url(&target, self.allow_http)?;
        Ok(target)
    }
    fn map_reference(&self, base: &Url, reference: &str) -> Result<String> {
        validate_reference(reference)?;
        ensure!(!reference.is_empty(), "empty media reference");
        ensure!(
            !reference.contains("{$"),
            "HLS variable substitution is unsupported"
        );
        self.local_url(&base.join(reference)?)
    }
    async fn fetch(
        &self,
        mut url: Url,
        method: Method,
        incoming: &HeaderMap,
    ) -> Result<(Response, Url)> {
        for hop in 0..=8 {
            validate_url(&url, self.allow_http)?;
            let mut headers = if same_origin(&self.original, &url) {
                self.headers.clone()
            } else {
                HeaderMap::new()
            };
            headers.insert("accept-encoding", HeaderValue::from_static("identity"));
            // Only range/cache validators from the engine, never its auth/cookies.
            for name in ["range", "if-range", "if-none-match", "if-modified-since"] {
                if let Some(value) = incoming.get(name) {
                    headers.insert(name, value.clone());
                }
            }
            let req = self
                .client
                .request(method.clone(), url.clone())
                .headers(headers);
            let response = timeout(IO_TIMEOUT, req.send())
                .await
                .context("upstream response timeout")??;
            ensure!(
                safe_response_headers(response.headers()),
                "unsafe/oversized upstream headers"
            );
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                ensure!(hop < 8, "redirect limit exceeded");
                let location = response
                    .headers()
                    .get("location")
                    .ok_or_else(|| anyhow!("redirect without Location"))?
                    .to_str()?;
                validate_reference(location)?;
                url = url.join(location)?;
                continue;
            }
            return Ok((response, url));
        }
        unreachable!()
    }
}
async fn serve(listener: TcpListener, lease: Arc<Lease>) {
    let slots = Arc::new(Semaphore::new(SLOTS));
    loop {
        tokio::select! {
            biased;
            _ = lease.cancel.cancelled() => break,
            accepted = listener.accept() => {
                let (stream,peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_) => {
                        tokio::select! { biased; _ = lease.cancel.cancelled() => break, _ = tokio::time::sleep(ACCEPT_BACKOFF) => continue }
                    }
                };
                if !peer.ip().is_loopback() { continue; }
                let Ok(permit) = slots.clone().try_acquire_owned() else { drop(stream); continue };
                let lease = lease.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    tokio::select! { biased; _ = lease.cancel.cancelled() => {}, _ = handle(stream,&lease) => {} }
                });
            }
        }
    }
}
struct Request {
    method: Method,
    target: String,
    headers: HeaderMap,
}
async fn read_request(stream: &mut TcpStream, authority: &str) -> Result<Request> {
    let mut bytes = Vec::with_capacity(1024);
    timeout(IO_TIMEOUT, async {
        loop {
            let mut b = [0; 512];
            let n = stream.read(&mut b).await?;
            ensure!(n != 0, "client closed headers");
            bytes.extend_from_slice(&b[..n]);
            ensure!(bytes.len() <= HEADER_LIMIT, "request headers too large");
            if bytes.windows(4).any(|s| s == b"\r\n\r\n") {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("request header timeout")??;
    let text = std::str::from_utf8(&bytes)?;
    let end = text
        .find("\r\n\r\n")
        .ok_or_else(|| anyhow!("bad request"))?;
    ensure!(
        end + 4 == bytes.len(),
        "request body/pipelining is unsupported"
    );
    let mut lines = text[..end].split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = match first.next() {
        Some("GET") => Method::GET,
        Some("HEAD") => Method::HEAD,
        _ => bail!("method not allowed"),
    };
    let target = first
        .next()
        .ok_or_else(|| anyhow!("missing target"))?
        .to_string();
    ensure!(
        !target.bytes().any(|b| b < 33 || b == 127),
        "invalid target"
    );
    ensure!(
        matches!(first.next(), Some("HTTP/1.0" | "HTTP/1.1")) && first.next().is_none(),
        "invalid request line"
    );
    let mut headers = HeaderMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(|| anyhow!("bad header"))?;
        ensure!(
            !name
                .bytes()
                .chain(value.bytes())
                .any(|b| b < 32 || b == 127),
            "invalid header control"
        );
        let name = HeaderName::from_bytes(name.as_bytes())?;
        ensure!(!headers.contains_key(&name), "duplicate request header");
        headers.insert(name, HeaderValue::from_str(value.trim())?);
    }
    ensure!(
        headers.get("host").and_then(|v| v.to_str().ok()) == Some(authority),
        "invalid Host"
    );
    ensure!(
        !headers.contains_key("transfer-encoding")
            && headers.get("content-length").is_none_or(|v| v == "0"),
        "request body unsupported"
    );
    Ok(Request {
        method,
        target,
        headers,
    })
}
async fn handle(mut stream: TcpStream, lease: &Lease) -> Result<()> {
    let request = match read_request(&mut stream, &lease.authority).await {
        Ok(req) => req,
        Err(e) => {
            let status = if e.to_string().contains("method not allowed") {
                405
            } else {
                400
            };
            let _ = send_error(&mut stream, status).await;
            return Ok(());
        }
    };
    let target = match lease.target(&request.target) {
        Ok(target) => target,
        Err(_) => {
            send_error(&mut stream, 404).await?;
            return Ok(());
        }
    };
    let (mut reader, mut writer) = stream.into_split();
    let mut disconnect = [0; 1];
    tokio::select! {
        _ = reader.read(&mut disconnect) => Ok(()),
        result = relay(&mut writer,lease,request,target) => result,
    }
}
async fn send_error<W: AsyncWriteExt + Unpin>(writer: &mut W, status: u16) -> Result<()> {
    let reason = match status {
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Bad Gateway",
    };
    let message =
        format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    timeout(IO_TIMEOUT, writer.write_all(message.as_bytes())).await??;
    Ok(())
}
/// One upstream chunk with an idle deadline; no whole-movie deadline.
async fn chunk(response: &mut Response, stream_timeout: Duration) -> Result<Option<Vec<u8>>> {
    Ok(timeout(stream_timeout, response.chunk())
        .await
        .context("upstream body idle timeout")??
        .map(|b| b.to_vec()))
}
/// Buffer only formats that require complete validation before exposing bytes.
/// Progressive binary media never enters this path.
async fn buffer_body(
    response: &mut Response,
    body: &mut Vec<u8>,
    limit: usize,
    stream_timeout: Duration,
) -> Result<()> {
    ensure!(body.len() <= limit, "buffered document too large");
    while let Some(bytes) = chunk(response, stream_timeout).await? {
        ensure!(
            bytes.len() <= limit - body.len(),
            "buffered document too large"
        );
        body.extend_from_slice(&bytes);
    }
    Ok(())
}
async fn write_bytes<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    bytes: &[u8],
    stream_timeout: Duration,
) -> Result<()> {
    // Each successful write resets the idle deadline; no whole-movie deadline.
    let mut rest = bytes;
    while !rest.is_empty() {
        let n = timeout(stream_timeout, writer.write(rest))
            .await
            .context("client body idle timeout")??;
        ensure!(n > 0, "client disconnected");
        rest = &rest[n..];
    }
    Ok(())
}
fn safe_response_headers(headers: &HeaderMap) -> bool {
    headers
        .iter()
        .map(|(k, v)| k.as_str().len() + v.as_bytes().len() + 4)
        .sum::<usize>()
        <= HEADER_LIMIT
        && !headers
            .values()
            .any(|v| v.as_bytes().iter().any(|b| *b < 32 || *b == 127))
        && [
            "content-type",
            "content-encoding",
            "content-length",
            "content-range",
            "location",
        ]
        .iter()
        .all(|name| headers.get_all(*name).iter().count() <= 1)
}
fn single_range_length(headers: &HeaderMap) -> Option<u64> {
    let range = headers
        .get("content-range")?
        .to_str()
        .ok()?
        .strip_prefix("bytes ")?;
    let (span, total) = range.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    if ![start, end]
        .iter()
        .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        || (total != "*" && (total.is_empty() || !total.bytes().all(|b| b.is_ascii_digit())))
    {
        return None;
    }
    let (start, end) = (start.parse::<u64>().ok()?, end.parse::<u64>().ok()?);
    let length = end.checked_sub(start)?.checked_add(1)?;
    if total != "*" && !total.parse::<u64>().is_ok_and(|n| n > end) {
        return None;
    }
    if !headers
        .get("content-length")
        .is_none_or(|v| v.to_str().ok().and_then(|v| v.parse::<u64>().ok()) == Some(length))
    {
        return None;
    }
    Some(length)
}
fn is_text_prefix(bytes: &[u8]) -> bool {
    // Classification is separate from lossy ASCII markup sniffing below. Raw
    // encrypted keys/media can contain invalid UTF-8 without being plaintext.
    // A split final UTF-8 codepoint is not evidence of a binary format.
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or("")
        }
        Err(_) => return false,
    };
    text.chars()
        .all(|c| !c.is_control() || matches!(c, '\r' | '\n' | '\t'))
}
/// A subtitle rendition's declared MIME type or file extension. This is only a
/// hint: the body must also be positively recognized as a subtitle, otherwise a
/// mislabeled response could smuggle a playlist past the manifest rewrite layer.
fn is_subtitle_resource(content_type: &str, path: &str) -> bool {
    matches!(
        content_type,
        "text/vtt" | "application/ttml+xml" | "application/x-subrip" | "text/srt" | "text/ttml"
    ) || path.ends_with(".vtt")
        || path.ends_with(".ttml")
        || path.ends_with(".dfxp")
        || path.ends_with(".srt")
}
/// A raw AES-128 key is exactly 16 bytes. Printable keys would otherwise match the
/// "unknown plain text is a hidden playlist" rule and be rejected with 502.
fn is_aes_key_body(
    content_type: &str,
    prefix: &[u8],
    declared: Option<u64>,
    headers: &HeaderMap,
) -> bool {
    // A 16-byte slice of a larger representation is NOT a complete AES key.
    // Otherwise ID3 metadata could bypass inspection in 16-byte range fragments.
    let whole = !headers.contains_key("content-range")
        || (single_range_length(headers) == Some(16)
            && headers
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|value| value.rsplit_once('/'))
                .is_some_and(|(_, total)| total.parse::<u64>().ok() == Some(16)));
    whole
        && declared == Some(16)
        && prefix.len() == 16
        && !prefix.contains(&b'\n')
        && !prefix.contains(&b'\r')
        && (content_type.is_empty()
            || content_type == "application/octet-stream"
            || content_type == "binary/octet-stream"
            || content_type.starts_with("application/x-")
            || content_type.starts_with("text/plain"))
}
fn wide_markup(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0xff, 0xfe])
        || bytes.starts_with(&[0xfe, 0xff])
        || bytes.starts_with(&[0, 0, 0xfe, 0xff])
    {
        return true;
    }
    for little in [true, false] {
        let units = bytes
            .chunks_exact(2)
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect::<Vec<_>>();
        if decoded_markup(bytes, &String::from_utf16_lossy(&units)) {
            return true;
        }
        let text = bytes
            .chunks_exact(4)
            .filter_map(|b| {
                char::from_u32(if little {
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                } else {
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
                })
            })
            .collect::<String>();
        if decoded_markup(bytes, &text) {
            return true;
        }
    }
    false
}
/// Whether `text`, one UTF-16/UTF-32 decoding of `bytes`, is plain ASCII text
/// hidden by zero bytes, or begins like a manifest, subtitle or URL list.
fn decoded_markup(bytes: &[u8], text: &str) -> bool {
    if bytes.len() >= 16
        && bytes.contains(&0)
        && !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii() && (!c.is_control() || c.is_ascii_whitespace()))
    {
        return true;
    }
    let text = text.trim_start();
    text.starts_with(['<', '#', '[']) || text.starts_with("http:") || text.starts_with("https:")
}
/// FFmpeg strips ID3 before probing/reading a demuxer. Inspect the payload after
/// bounded ID3 metadata too, otherwise a binary prefix can hide an HLS document
/// even without a subtitle MIME/extension (including through an upstream redirect).
/// Ordinary ID3-tagged audio remains streaming; only metadata plus a sniff window
/// is accumulated, never the rest of a movie/audio file.
async fn inspect_id3_payload(
    response: &mut Response,
    prefix: &mut Vec<u8>,
    representation_continues: bool,
    stream_timeout: Duration,
) -> Result<()> {
    // A tiny range can split even the magic/header. Never let reconnect assemble
    // an ID3 document from independently unclassified fragments.
    ensure!(
        prefix.is_empty()
            || prefix.len() >= 10
            || (!b"ID3".starts_with(prefix) && !prefix.starts_with(b"ID3")),
        "incomplete ID3 header"
    );
    let mut offset = 0;
    let mut tags = 0;
    while prefix.len().saturating_sub(offset) >= 10 {
        let h = &prefix[offset..offset + 10];
        // Match the bundled FFmpeg's ff_id3v2_match/tag_len, including footer.
        if !h.starts_with(b"ID3")
            || h[3] == 0xff
            || h[4] == 0xff
            || h[6..10].iter().any(|b| b & 0x80 != 0)
        {
            break;
        }
        tags += 1;
        ensure!(tags <= 64, "ID3 metadata count limit");
        let length = h[6..10]
            .iter()
            .fold(0_usize, |n, b| (n << 7) | usize::from(*b));
        offset += 10 + length + if h[5] & 0x10 != 0 { 10 } else { 0 };
        ensure!(offset <= MANIFEST_LIMIT, "ID3 metadata too large");
        while prefix.len() < offset + 512 {
            match chunk(response, stream_timeout).await? {
                Some(bytes) => {
                    ensure!(
                        prefix.len() + bytes.len() <= MANIFEST_LIMIT + 65_536,
                        "ID3 sniff buffer too large"
                    );
                    prefix.extend_from_slice(&bytes);
                }
                None => break,
            }
        }
        ensure!(prefix.len() >= offset, "truncated ID3 metadata");
    }
    if offset != 0 {
        ensure!(
            !representation_continues || prefix.len() >= offset + 512,
            "incomplete ID3 payload window"
        );
        let sample = &prefix[offset..prefix.len().min(offset + 512)];
        let text = String::from_utf8_lossy(sample);
        let text = text.trim_start_matches('\u{feff}').trim_start();
        ensure!(
            sample.is_empty()
                || (!is_text_prefix(sample)
                    && !wide_markup(sample)
                    && !text.starts_with(['<', '#', '['])
                    && !text.starts_with("WEBVTT")
                    && !text.starts_with("http:")
                    && !text.starts_with("https:")),
            "ID3-wrapped textual media unsupported"
        );
    }
    Ok(())
}

/// Debrid add-ons (Torrentio, MediaFusion, Comet…) redirect to a status video
/// such as /videos/downloading_v2.mp4 or /videos/failed_access_v2.mp4 when a
/// torrent is not cached, was cancelled or hit a limit.
fn debrid_status_clip(url: &Url) -> Option<&'static str> {
    let path = url.path().to_ascii_lowercase();
    let (folder, file) = path.rsplit_once('/')?;
    let stem = file.strip_suffix(".mp4")?;
    if !(folder.ends_with("/videos") || folder.contains("/static") || folder.contains("exceptions"))
    {
        return None;
    }
    if stem.contains("download") || stem.contains("queue") || stem.contains("caching") {
        return Some("downloading");
    }
    const FAILED: [&str; 10] = [
        "fail",
        "error",
        "limit",
        "expired",
        "cancel",
        "infring",
        "unavailable",
        "not_found",
        "too_big",
        "no_matching",
    ];
    FAILED
        .iter()
        .any(|word| stem.contains(word))
        .then_some("failed")
}
/// The complete length from "Content-Range: bytes a-b/total"; None for "*".
fn content_range_total(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("content-range")?
        .to_str()
        .ok()?
        .rsplit_once('/')?
        .1
        .parse()
        .ok()
}
fn range_ends_at_eof(headers: &HeaderMap) -> bool {
    headers
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.rsplit_once('-'))
        .and_then(|(_, tail)| tail.split_once('/'))
        .and_then(|(end, total)| Some((end.parse::<u64>().ok()?, total.parse::<u64>().ok()?)))
        .is_some_and(|(end, total)| end.checked_add(1) == Some(total))
}

async fn relay<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    lease: &Lease,
    request: Request,
    target: Url,
) -> Result<()> {
    let is_source = target == lease.original;
    let (mut response, final_url) = match lease
        .fetch(target, request.method.clone(), &request.headers)
        .await
    {
        Ok(r) => r,
        Err(_) => {
            send_error(writer, 502).await?;
            return Ok(());
        }
    };
    let headers = response.headers().clone();
    // A debrid service that cannot deliver the file redirects to a short status
    // clip. Never play it as the film: refuse it and report why.
    if is_source {
        if let Some(status) = debrid_status_clip(&final_url) {
            if let Ok(mut slot) = lease.debrid_status.lock() {
                *slot = Some(status);
            }
            send_error(writer, 502).await?;
            return Ok(());
        }
    }
    // Never relay compressed unknown bodies: they could conceal a playlist that
    // the native engine subsequently decompresses and follows outside this relay.
    if !safe_response_headers(&headers)
        || headers
            .get("content-encoding")
            .is_some_and(|v| v != "identity")
    {
        send_error(writer, 502).await?;
        return Ok(());
    }
    let status = response.status();
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let path = final_url.path().to_ascii_lowercase();
    let mut kind =
        if content_type.contains("mpegurl") || path.ends_with(".m3u8") || path.ends_with(".m3u") {
            Some("hls")
        } else if content_type == "application/dash+xml" || path.ends_with(".mpd") {
            Some("dash")
        } else {
            None
        };
    let head = request.method == Method::HEAD;
    let range_length = if status.as_u16() == 206 {
        single_range_length(&headers)
    } else {
        None
    };
    let declared_length = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if status.is_informational()
        || (status.as_u16() == 206 && range_length.is_none())
        || (headers.contains_key("content-range") && !matches!(status.as_u16(), 206 | 416))
    {
        send_error(writer, 502).await?;
        return Ok(());
    }
    // Remember the source file's real size for the player's file information.
    if is_source && kind.is_none() {
        let total = match status.as_u16() {
            206 => content_range_total(&headers),
            200 => declared_length,
            _ => None,
        };
        if let Some(total) = total.filter(|n| *n > 0) {
            lease.source_bytes.store(total, Ordering::Relaxed);
        }
    }
    // Preserve error status/Content-Range (notably 416), but never give native
    // demuxers an HTML/playlist error page. There is no reason to download it.
    let discard_body = !head && !status.is_success() && kind.is_none();
    let mut prefix = Vec::new();
    let mut validated_subtitle = None;
    if !head && !discard_body {
        // Inspect only a bounded prefix, not UTF-8 validity of an entire binary
        // chunk. A binary byte after an M3U/XML header must not mask that header.
        loop {
            match chunk(&mut response, lease.stream_timeout).await {
                Ok(Some(bytes)) => prefix.extend_from_slice(&bytes),
                Ok(None) => break,
                Err(_) => {
                    send_error(writer, 502).await?;
                    return Ok(());
                }
            }
            let sample = &prefix[..prefix.len().min(512)];
            if prefix.len() >= 512 || (prefix.len() >= 32 && !is_text_prefix(sample)) {
                break;
            }
        }
        if !is_aes_key_body(&content_type, &prefix, declared_length, &headers)
            && inspect_id3_payload(
                &mut response,
                &mut prefix,
                status.as_u16() == 206 && !range_ends_at_eof(&headers),
                lease.stream_timeout,
            )
            .await
            .is_err()
        {
            send_error(writer, 502).await?;
            return Ok(());
        }
        let sample = &prefix[..prefix.len().min(512)];
        let sniff = String::from_utf8_lossy(sample);
        let sniff = sniff.trim_start_matches('\u{feff}').trim_start();
        if wide_markup(sample) {
            send_error(writer, 502).await?;
            return Ok(());
        }
        // Small byte ranges may legitimately contain only printable codec or
        // container bytes. Accept them only with validated single-range framing,
        // a binary-media MIME, and no playlist-like lines/URI prefix.
        let ascii_range = status.as_u16() == 206
            && (content_type.starts_with("video/")
                || content_type.starts_with("audio/")
                || content_type == "application/octet-stream")
            && !sample.contains(&b'\n')
            && !sample.contains(&b'\r')
            && !["http:", "https:", "//", "./", "../", "/"]
                .iter()
                .any(|p| sniff.starts_with(p));
        let aes_key = is_aes_key_body(&content_type, &prefix, declared_length, &headers);
        // A complete AES key is raw key material even when a key byte happens to
        // be '<'; honouring the exemption everywhere keeps it out of the subtitle
        // and manifest classifiers (a 1-in-256 key used to fail with 502).
        let xml_candidate = !aes_key && sniff.starts_with('<');
        let unknown_text = is_text_prefix(sample) && !sample.is_empty() && !ascii_range
            || content_type.starts_with("text/")
            || content_type.contains("xml")
            || content_type.contains("json")
            || content_type.contains("html")
            || content_type.starts_with("multipart/")
            || sniff.starts_with(['#', '['])
            || sample.starts_with(&[0xff, 0xfe])
            || sample.starts_with(&[0xfe, 0xff])
            || sample.starts_with(&[0, 0, 0xfe, 0xff])
            || sample.starts_with(&[0x1f, 0x8b]);
        // Hints select a validation path, NEVER a pass-through exemption. Only
        // full, bounded documents can be subtitles. No scanning past ID3/garbage
        // for a <tt> root, and no 512-byte approval of an unchecked remainder.
        let subtitle_candidate = is_subtitle_resource(&content_type, &path)
            || sniff.starts_with("WEBVTT")
            || (!aes_key && !ascii_range && sniff.starts_with(|c: char| c.is_ascii_digit()))
            || xml_candidate;
        if sniff.starts_with("#EXTM3U") {
            kind = Some("hls");
        } else if kind.is_none() && subtitle_candidate {
            let buffered = async {
                ensure!(
                    status.is_success() && status.as_u16() != 206,
                    "partial/error subtitles unsupported"
                );
                ensure!(
                    declared_length.is_none_or(|n| n <= subtitle::LIMIT as u64),
                    "subtitle too large"
                );
                buffer_body(
                    &mut response,
                    &mut prefix,
                    subtitle::LIMIT,
                    lease.stream_timeout,
                )
                .await
            }
            .await;
            if buffered.is_err() {
                send_error(writer, 502).await?;
                return Ok(());
            }
            match subtitle::parse(&prefix, &lease.cancel) {
                Ok(subtitle) => validated_subtitle = Some(subtitle),
                // XML that is not text-only TTML must still pass the DASH
                // parser/rewriter; HTML and malformed/unsafe TTML fail there.
                Err(_) if xml_candidate => kind = Some("dash"),
                Err(_) => {
                    send_error(writer, 502).await?;
                    return Ok(());
                }
            }
        }
        if kind.is_none() && validated_subtitle.is_none() && !aes_key && unknown_text {
            send_error(writer, 502).await?;
            return Ok(());
        }
    }
    if !head && range_length.is_some_and(|n| prefix.len() as u64 > n) {
        send_error(writer, 502).await?;
        return Ok(());
    }
    let mut output_content_type = validated_subtitle.as_ref().map(|s| s.content_type);
    let mut rewritten = validated_subtitle.map(|s| s.body);
    if let Some(kind) = kind {
        if !head {
            let result = async {
                ensure!(
                    status.is_success() && status.as_u16() != 206,
                    "partial/error manifests unsupported"
                );
                ensure!(
                    headers
                        .get("content-encoding")
                        .is_none_or(|v| v == "identity"),
                    "encoded manifests unsupported"
                );
                ensure!(prefix.len() <= MANIFEST_LIMIT, "manifest too large");
                while let Some(bytes) = chunk(&mut response, lease.stream_timeout).await? {
                    ensure!(
                        prefix.len() + bytes.len() <= MANIFEST_LIMIT,
                        "manifest too large"
                    );
                    prefix.extend_from_slice(&bytes);
                }
                let text = std::str::from_utf8(&prefix).context("manifest is not UTF-8")?;
                if kind == "hls" {
                    manifest::hls(lease, &final_url, text)
                } else {
                    manifest::dash(lease, &final_url, text)
                }
            }
            .await;
            match result {
                Ok(body) => {
                    rewritten = Some(body.into_bytes());
                    output_content_type = Some(if kind == "hls" {
                        "application/vnd.apple.mpegurl"
                    } else {
                        "application/dash+xml"
                    });
                }
                Err(_) => {
                    send_error(writer, 502).await?;
                    return Ok(());
                }
            }
        }
    }
    let mut output = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Response")
    );
    if let Some(content_type) = output_content_type {
        output.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    for name in [
        "content-type",
        "content-range",
        "accept-ranges",
        "content-encoding",
        "etag",
        "last-modified",
    ] {
        if rewritten.is_some()
            && matches!(
                name,
                "content-type" | "content-range" | "accept-ranges" | "content-encoding" | "etag"
            )
        {
            continue;
        }
        if let Some(value) = headers.get(name) {
            if let Ok(value) = value.to_str() {
                output.push_str(&format!("{name}: {value}\r\n"));
            }
        }
    }
    if discard_body {
        output.push_str("Content-Length: 0\r\n");
    } else if let Some(body) = &rewritten {
        output.push_str(&format!("Content-Length: {}\r\n", body.len()));
    } else if let Some(length) = range_length {
        output.push_str(&format!("Content-Length: {length}\r\n"));
    } else if !(head && (kind.is_some() || is_subtitle_resource(&content_type, &path))) {
        if let Some(value) = headers.get("content-length").and_then(|v| v.to_str().ok()) {
            output.push_str(&format!("Content-Length: {value}\r\n"));
        }
    }
    output.push_str("\r\n");
    write_bytes(writer, output.as_bytes(), lease.stream_timeout).await?;
    if !head && !discard_body {
        if let Some(body) = rewritten {
            write_bytes(writer, &body, lease.stream_timeout).await?;
        } else {
            let mut sent = prefix.len() as u64;
            write_bytes(writer, &prefix, lease.stream_timeout).await?;
            while let Some(bytes) = chunk(&mut response, lease.stream_timeout).await? {
                sent = sent
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| anyhow!("body length overflow"))?;
                ensure!(
                    range_length.is_none_or(|n| sent <= n),
                    "oversized range body"
                );
                write_bytes(writer, &bytes, lease.stream_timeout).await?;
            }
            ensure!(
                range_length.is_none_or(|n| sent == n),
                "truncated range body"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redirect_downgrade_and_origin_policy() {
        let original = Url::parse("https://media.example:443/a").unwrap();
        let downgraded = original.join("http://media.example/b").unwrap();
        assert!(validate_url(&downgraded, false).is_err());
        assert!(validate_url(&downgraded, true).is_ok());
        assert!(!same_origin(&original, &downgraded));
        assert!(same_origin(
            &original,
            &Url::parse("https://media.example/b").unwrap()
        ));
        assert!(!same_origin(
            &original,
            &Url::parse("https://media.example:444/b").unwrap()
        ));
        for url in ["http://127.0.0.1/a", "http://[::1]/a", "http://localhost/a"] {
            assert!(validate_url(&Url::parse(url).unwrap(), false).is_ok());
        }
        for reference in [
            "https://@media.example/a",
            "//user@media.example/a",
            "https://media.example/\nfoo",
        ] {
            assert!(validate_reference(reference).is_err());
        }
    }
    fn lease() -> Lease {
        Lease {
            token: "token".to_string(),
            authority: "127.0.0.1:9999".to_string(),
            original: Url::parse("https://example.invalid/a").unwrap(),
            headers: HeaderMap::new(),
            allow_http: false,
            cancel: CancellationToken::new(),
            client: MediaProxy::new().client.clone(),
            stream_timeout: STREAM_TIMEOUT,
            source_bytes: Arc::new(AtomicU64::new(0)),
            debrid_status: Arc::new(Mutex::new(None)),
        }
    }
    #[test]
    fn debrid_status_clips_are_recognised() {
        let status = |url: &str| debrid_status_clip(&Url::parse(url).unwrap());
        assert_eq!(
            status("https://torrentio.strem.fun/videos/downloading_v2.mp4"),
            Some("downloading")
        );
        assert_eq!(
            status("https://torrentio.strem.fun/videos/failed_access_v2.mp4"),
            Some("failed")
        );
        assert_eq!(
            status("https://torrentio.strem.fun/videos/limits_exceeded_v2.mp4"),
            Some("failed")
        );
        assert_eq!(
            status("https://mediafusion.example/static/exceptions/torrent_cancelled.mp4"),
            Some("failed")
        );
        assert_eq!(status("https://cdn.example/dl/Film.2160p.REMUX.mkv"), None);
        assert_eq!(
            status("https://cdn.example/videos/My.Download.Story.2024.mkv"),
            None
        );
    }
    #[test]
    fn decoded_origins_must_be_exactly_canonical_and_consent_checked() {
        let lease = lease();
        for origin in [
            "https://EXAMPLE.invalid",
            "https://example.invalid:443",
            "https://%65xample.invalid",
            "https://example.invalid/",
            "http://example.invalid",
            "https://@example.invalid",
            "https://example.invalid/path",
            "https://example.invalid?x",
            "http://127.0.0.1%2fexample.invalid",
            "https://example.invalid%00.evil",
        ] {
            assert!(
                lease
                    .target(&format!("/token/{}/a", hex(origin.as_bytes())))
                    .is_err(),
                "{origin}"
            );
        }
        let origin = hex(b"https://example.invalid");
        assert_eq!(
            lease
                .target(&format!("/token/{origin}/a%2Fb/c%252Fd?q=%2f"))
                .unwrap()
                .as_str(),
            "https://example.invalid/a%2Fb/c%252Fd?q=%2f"
        );
        for path in [
            format!("/token/{origin}/\\evil"),
            format!("/token/{origin}/a#b"),
            format!("/%74oken/{origin}/a"),
            "/token/ff00/a".to_string(),
            "/token/%36%31/a".to_string(),
            "/token/é/a".to_string(),
        ] {
            assert!(lease.target(&path).is_err(), "{path}");
        }
    }
    #[test]
    fn ambiguous_url_authorities_whitespace_and_backslashes_are_rejected() {
        for reference in [
            "https:////@example.invalid/a",
            "https:/example.invalid/a",
            "https:example.invalid/a",
            "https://user@example.invalid/a",
            "//user@example.invalid/a",
            "HTTPS://@example.invalid/a",
            "https://example.invalid/\\x",
            "a\\b",
            " a",
            "a b",
            "a\tb",
            "a\u{7f}",
        ] {
            assert!(validate_reference(reference).is_err(), "{reference}");
        }
        for reference in [
            "/a/b",
            "../a",
            "a?url=https://example.invalid/x",
            "https://example.invalid/a",
            "//example.invalid/b",
        ] {
            assert!(validate_reference(reference).is_ok(), "{reference}");
        }
    }
    #[test]
    fn loopback_preparation_does_not_grant_remote_http_children() {
        let mut lease = lease();
        lease.original = Url::parse("http://127.0.0.1:7777/movie").unwrap();
        assert!(lease
            .map_reference(&lease.original, "http://example.invalid/child")
            .is_err());
        assert!(lease
            .map_reference(&lease.original, "http://localhost:7777/child")
            .is_ok());
        assert!(lease
            .map_reference(&lease.original, "https://example.invalid/child")
            .is_ok());
    }
    #[test]
    fn manifest_template_validators_reject_ambiguous_forms() {
        let lease = lease();
        let base = &lease.original;
        for reference in [
            "$Number%$.m4s",
            "$Number%00d$.m4s",
            "$Number%021d$.m4s",
            "$RepresentationID%05d$",
            "$$",
            "$",
            "http://$Number$.example.invalid/a",
        ] {
            assert!(
                manifest::validate_template(reference, base).is_err(),
                "{reference}"
            );
        }
        for reference in [
            "$Number$.m4s",
            "$Number%05d$.m4s",
            "$Time$-$Bandwidth$.m4s",
            "$RepresentationID$/a",
        ] {
            assert!(
                manifest::validate_template(reference, base).is_ok(),
                "{reference}"
            );
        }
    }
}
