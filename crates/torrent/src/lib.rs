//! Isolated, single-lease BitTorrent v1 streaming on the caller's Tokio runtime.
//! No remote-control API, persistent DHT, automatic resume, or rolling cache.
use anyhow::{anyhow, bail, ensure, Context, Result};
use fs2::FileExt;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
};
use rand::RngCore;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    future::Future,
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

const GIB: u64 = 1024 * 1024 * 1024;
const MAX_FILES: usize = 512;
const MAX_PIECE: u64 = 16 * 1024 * 1024;
const MAX_HTTP_CLIENTS: usize = 4;
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct TorrentConfig {
    pub cache_budget_bytes: u64,
    pub free_space_reserve_bytes: u64,
    pub metadata_timeout: Duration,
}
impl Default for TorrentConfig {
    fn default() -> Self {
        Self {
            cache_budget_bytes: 8 * GIB,
            free_space_reserve_bytes: GIB,
            metadata_timeout: Duration::from_secs(90),
        }
    }
}

pub struct TorrentEngine {
    cache_dir: PathBuf,
    config: TorrentConfig,
    state: Mutex<Option<Active>>,
    cancellation: StdMutex<CancellationEpoch>,
}
// All prepares register against the epoch BEFORE waiting for state. A stop keeps
// that epoch cancelled until every overlapping stop has left (even if aborted).
// Never hold this synchronous mutex across an await or while acquiring state.
struct CancellationEpoch {
    token: CancellationToken,
    stops: usize,
}
struct StopGuard<'a>(&'a StdMutex<CancellationEpoch>);
impl Drop for StopGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut epoch) = self.0.lock() {
            epoch.stops -= 1;
            if epoch.stops == 0 {
                epoch.token = CancellationToken::new();
            }
        }
    }
}
struct Active {
    lease: Lease,
    torrent: Arc<ManagedTorrent>,
    server: JoinHandle<()>,
    response: Value,
}
struct Lease {
    dir: PathBuf,
    lock: Option<File>,
    token: CancellationToken,
    session: Option<Arc<Session>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.token.cancel();
        let session = self.session.take();
        let lock = self.lock.take();
        let dir = self.dir.clone();
        // Dropping an in-flight prepare must also release networking, not seed forever.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Some(s) = session {
                    s.stop().await;
                }
                let _lock = lock;
                let _ = remove_lease_dir(&dir);
            });
        }
        // On runtime shutdown retain the files; next prepare only reclaims unlocked leases.
    }
}
impl Drop for TorrentEngine {
    fn drop(&mut self) {
        if let Ok(epoch) = self.cancellation.lock() {
            epoch.token.cancel();
        }
    }
}
impl TorrentEngine {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self::with_config(cache_dir, TorrentConfig::default())
    }
    pub fn with_config(cache_dir: PathBuf, config: TorrentConfig) -> Self {
        Self {
            cache_dir,
            config,
            state: Mutex::new(None),
            cancellation: StdMutex::new(CancellationEpoch {
                token: CancellationToken::new(),
                stops: 0,
            }),
        }
    }
    /// Accepts Stremio {infoHash: 40hex, fileIdx?: original_index, sources?: ["tracker:..."]}.
    /// Replaces the previous lease; caller must stop on player release/backgrounding.
    pub fn prepare(&self, stream: Value) -> impl std::future::Future<Output = Result<Value>> + '_ {
        // Register cancellation synchronously, before this future can be queued or delayed.
        let registered = parse_input(&stream).and_then(|input| {
            let token = self
                .cancellation
                .lock()
                .map_err(|_| anyhow!("cancellation lock poisoned"))?
                .token
                .child_token();
            Ok((input, token))
        });
        async move {
            let (input, token) = registered?;
            let mut state = self.state.lock().await;
            ensure!(!token.is_cancelled(), "torrent preparation cancelled");
            if let Some(active) = state.take() {
                shutdown(active).await?;
            }
            // stop can run while old shutdown is awaited. Do not create a new
            // session/lease (or spend the metadata timeout) after that cancellation.
            ensure!(!token.is_cancelled(), "torrent preparation cancelled");
            // Directory sweeps and the manager flock are blocking; keep them off
            // the bridge's two async workers.
            let mut lease = {
                let (cache, lease_token) = (self.cache_dir.clone(), token.clone());
                tokio::task::spawn_blocking(move || create_lease(&cache, lease_token))
                    .await
                    .context("torrent cache worker failed")??
            };
            let result = tokio::select! {
                biased;
                _ = token.cancelled() => Err(anyhow!("torrent preparation cancelled")),
                result = tokio::time::timeout(self.config.metadata_timeout, self.prepare_lease(&input, &mut lease)) => {
                    result.context("torrent metadata/initialization timeout")?
                }
            };
            let (torrent, file_idx, file_name, file_length, reserved) = result?;
            let mime = mime_guess::from_path(&file_name)
                .first_or_octet_stream()
                .to_string();
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
            ensure!(!token.is_cancelled(), "torrent preparation cancelled");
            let authority = listener.local_addr()?.to_string();
            let lease_id = random_token();
            let path = format!("/stream/{lease_id}");
            let response = json!({"url": format!("http://{authority}{path}"), "lease_id": lease_id,
            "mime_type": mime, "file_name": file_name, "file_length": file_length,
            "file_index": file_idx, "info_hash": input.hash, "transport": "bittorrent-v1",
            "cache_reserved_bytes": reserved, "ignored_sources": input.ignored_sources});
            let server = tokio::spawn(serve(
                listener,
                StreamEndpoint {
                    host: authority,
                    path,
                    mime,
                    length: file_length,
                    index: file_idx,
                    torrent: torrent.clone(),
                },
                token,
            ));
            *state = Some(Active {
                lease,
                torrent,
                server,
                response: response.clone(),
            });
            Ok(response)
        }
    }
    async fn prepare_lease(
        &self,
        input: &Input,
        lease: &mut Lease,
    ) -> Result<(Arc<ManagedTorrent>, usize, String, u64, u64)> {
        let session = Session::new_with_opts(
            lease.dir.clone(),
            SessionOptions {
                disable_dht_persistence: true,
                enable_upnp_port_forwarding: false,
                cancellation_token: Some(lease.token.clone()),
                concurrent_init_limit: Some(1),
                ..Default::default()
            },
        )
        .await?;
        lease.session = Some(session.clone());
        // Resolve metadata without creating/admitting torrent payload files.
        let listed = session
            .add_torrent(
                AddTorrent::from_url(input.magnet.clone()),
                Some(AddTorrentOptions {
                    list_only: true,
                    ..Default::default()
                }),
            )
            .await?;
        let listed = match listed {
            AddTorrentResponse::ListOnly(r) => r,
            _ => bail!("unexpected metadata response"),
        };
        ensure!(
            listed.torrent_bytes.len() <= 32 * 1024 * 1024,
            "torrent metadata exceeds 32 MiB"
        );
        let mut files = Vec::new();
        for d in listed.info.iter_file_details()? {
            ensure!(
                files.len() < MAX_FILES,
                "too many torrent files (maximum {MAX_FILES})"
            );
            ensure!(
                !d.attrs().symlink && d.symlink_path.is_none(),
                "symlink metadata is not supported"
            );
            let components = d.filename.to_vec()?;
            validate_components(&components)?;
            files.push((components.join("/"), d.len));
        }
        if let Some(name) = listed.info.name.as_ref() {
            validate_components(&[std::str::from_utf8(name.as_ref())?.to_string()])?;
        }
        let idx = select_file(&files, input.file_idx)?;
        let (name, length) = files[idx].clone();
        ensure!(length > 0, "selected file is empty");
        let piece = listed.info.piece_length as u64;
        ensure!(
            piece > 0 && piece <= MAX_PIECE,
            "unsupported torrent piece length"
        );
        let reserved = length
            .checked_add(piece.checked_mul(2).context("piece overflow")?)
            .context("length overflow")?;
        ensure!(
            reserved <= self.config.cache_budget_bytes,
            "selected file plus boundary pieces exceeds cache budget"
        );
        let free = fs2::available_space(&lease.dir)?;
        ensure!(
            free >= reserved.saturating_add(self.config.free_space_reserve_bytes),
            "insufficient free space (reserve protected)"
        );
        // Reuse verified metadata rather than resolve the magnet twice. Keep original index.
        let added = session
            .add_torrent(
                AddTorrent::from_bytes(listed.torrent_bytes),
                Some(AddTorrentOptions {
                    only_files: Some(vec![idx]),
                    output_folder: Some(lease.dir.to_string_lossy().into_owned()),
                    initial_peers: Some(listed.seen_peers),
                    ..Default::default()
                }),
            )
            .await?;
        let torrent = added.into_handle().context("torrent was not admitted")?;
        torrent.wait_until_initialized().await?;
        Ok((torrent, idx, name, length, reserved))
    }
    pub async fn stop(&self) -> Result<()> {
        // Registration is synchronous: every prepare already queued, or begun
        // while any stop is waiting/shutting down, observes a cancelled epoch.
        let _stop = {
            let mut epoch = self
                .cancellation
                .lock()
                .map_err(|_| anyhow!("cancellation lock poisoned"))?;
            epoch.stops += 1;
            epoch.token.cancel();
            StopGuard(&self.cancellation)
        };
        let mut state = self.state.lock().await;
        if let Some(active) = state.take() {
            shutdown(active).await?;
        }
        Ok(())
    }
    /// Release only the lease owned by a retiring Android service, never a newer source.
    /// Unlike stop(), a stale scoped stop must not cancel replacement preparations.
    pub async fn stop_lease(&self, lease_id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        if state
            .as_ref()
            .is_some_and(|active| active.response["lease_id"].as_str() == Some(lease_id))
        {
            if let Some(active) = state.take() {
                shutdown(active).await?;
            }
        }
        Ok(())
    }
    pub async fn stats(&self) -> Result<Value> {
        let state = self.state.lock().await;
        Ok(match state.as_ref() {
            Some(active) => {
                json!({"active": !active.lease.token.is_cancelled(), "lease_id": active.response["lease_id"],
                "torrent": active.torrent.stats(), "cache_budget_bytes": self.config.cache_budget_bytes})
            }
            None => json!({"active": false, "cache_budget_bytes": self.config.cache_budget_bytes}),
        })
    }
}
async fn shutdown(mut active: Active) -> Result<()> {
    active.lease.token.cancel();
    let _ = active.server.await;
    if let Some(session) = active.lease.session.take() {
        session.stop().await;
    }
    // remove_dir_all of a multi-GiB cache must not stall an async worker.
    let dir = active.lease.dir.clone();
    tokio::task::spawn_blocking(move || remove_lease_dir(&dir))
        .await
        .context("torrent cache worker failed")??;
    Ok(())
}

/// Validate the Stremio torrent fields without starting metadata/network work,
/// so a rejected replacement cannot tear down the current playback first.
pub fn validate_stream(stream: &Value) -> Result<()> {
    parse_input(stream).map(|_| ())
}
struct Input {
    hash: String,
    magnet: String,
    file_idx: Option<usize>,
    ignored_sources: Vec<String>,
}
fn parse_input(stream: &Value) -> Result<Input> {
    let hash = stream
        .get("infoHash")
        .and_then(Value::as_str)
        .context("torrent stream requires infoHash; direct HTTP is handled by the player")?;
    ensure!(
        hash.len() == 40 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
        "infoHash must be exactly 40 hexadecimal characters (BitTorrent v1)"
    );
    let hash = hash.to_ascii_lowercase();
    let file_idx = match stream.get("fileIdx") {
        None | Some(Value::Null) => None,
        Some(v) => Some(usize::try_from(
            v.as_u64()
                .context("fileIdx must be a nonnegative integer")?,
        )?),
    };
    let mut magnet = url::Url::parse(&format!("magnet:?xt=urn:btih:{hash}"))?;
    let mut ignored_sources = Vec::new();
    if let Some(sources) = stream.get("sources") {
        let sources = sources.as_array().context("sources must be an array")?;
        ensure!(sources.len() <= 64, "too many sources");
        for source in sources {
            let source = source.as_str().context("source must be a string")?;
            ensure!(source.len() <= 2048, "source URL too long");
            if let Some(tracker) = source.strip_prefix("tracker:") {
                let u = url::Url::parse(tracker).context("invalid tracker URL")?;
                ensure!(
                    matches!(u.scheme(), "http" | "https" | "udp")
                        && u.host_str().is_some()
                        && u.username().is_empty()
                        && u.password().is_none()
                        && u.fragment().is_none(),
                    "unsupported tracker URL"
                );
                magnet.query_pairs_mut().append_pair("tr", tracker);
            } else if source.starts_with("dht:") {
                // Stremio dht:<hash> is not a tracker/peer address. Never pretend it was mapped.
                ignored_sources.push(source.to_owned());
            } else {
                bail!("unsupported torrent source scheme");
            }
        }
    }
    Ok(Input {
        hash,
        magnet: magnet.into(),
        file_idx,
        ignored_sources,
    })
}
fn validate_components(components: &[String]) -> Result<()> {
    ensure!(
        !components.is_empty() && components.len() <= 32,
        "invalid metadata path depth"
    );
    for c in components {
        ensure!(
            !c.is_empty()
                && c != "."
                && c != ".."
                && c.len() <= 255
                && !c.contains(['/', '\\', ':', '\0'])
                && !c.chars().any(char::is_control),
            "unsafe torrent metadata path"
        );
    }
    Ok(())
}
fn select_file(files: &[(String, u64)], requested: Option<usize>) -> Result<usize> {
    ensure!(!files.is_empty(), "torrent contains no files");
    if let Some(i) = requested {
        ensure!(i < files.len(), "fileIdx out of bounds");
        return Ok(i);
    }
    // max_by_key's tie behavior is made explicit: first original index wins.
    Ok(files
        .iter()
        .enumerate()
        .max_by_key(|(i, (_, len))| (*len, std::cmp::Reverse(*i)))
        .unwrap()
        .0)
}
fn random_token() -> String {
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}
fn is_lease_name(name: &str) -> bool {
    name.strip_prefix("lease-")
        .map(|s| s.len() == 48 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or(false)
}
fn remove_lease_dir(path: &Path) -> Result<()> {
    ensure!(
        path.file_name().and_then(|s| s.to_str()).map(is_lease_name) == Some(true),
        "refusing unsafe cache cleanup"
    );
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "unsafe cache directory"
            );
            std::fs::remove_dir_all(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn create_lease(cache: &Path, token: CancellationToken) -> Result<Lease> {
    let root = cache.join("torrent-v1");
    std::fs::create_dir_all(&root)?;
    ensure!(
        !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
        "cache root must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    let root = root.canonicalize()?;
    let manager = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".manager.lock"))?;
    manager.lock_exclusive()?;
    // Only reclaim recognized, unlocked private lease directories; never active files.
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !is_lease_name(&entry.file_name().to_string_lossy()) || !entry.file_type()?.is_dir() {
            continue;
        }
        let lock_path = entry.path().join(".lock");
        if std::fs::symlink_metadata(&lock_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        if let Ok(lock) = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
        {
            if lock.try_lock_exclusive().is_ok() {
                remove_lease_dir(&entry.path())?;
            }
        }
    }
    let dir = root.join(format!("lease-{}", random_token()));
    std::fs::create_dir(&dir)?;
    let lock = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(dir.join(".lock"))?;
    lock.lock_exclusive()?;
    Ok(Lease {
        dir,
        lock: Some(lock),
        token,
        session: None,
    })
}

/// One range only; RFC-style suffix/open-ended ranges; multi-range returns 416.
fn parse_range(value: Option<&str>, length: u64) -> Result<Option<(u64, u64)>> {
    let Some(value) = value else { return Ok(None) };
    let value = value
        .strip_prefix("bytes=")
        .context("unsupported range unit")?;
    ensure!(!value.contains(',') && length > 0, "unsatisfiable range");
    let (start, end) = value.split_once('-').context("invalid range")?;
    if start.is_empty() {
        let count: u64 = end.parse()?;
        ensure!(count > 0, "zero suffix range");
        return Ok(Some((length.saturating_sub(count), length - 1)));
    }
    let start: u64 = start.parse()?;
    ensure!(start < length, "range beyond end");
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>()?.min(length - 1)
    };
    ensure!(start <= end, "reversed range");
    Ok(Some((start, end)))
}
#[derive(Clone)]
struct StreamEndpoint {
    host: String,
    path: String,
    mime: String,
    length: u64,
    index: usize,
    torrent: Arc<ManagedTorrent>,
}
async fn serve(listener: TcpListener, endpoint: StreamEndpoint, cancel: CancellationToken) {
    serve_connections(listener, cancel, move |socket| {
        let endpoint = endpoint.clone();
        async move {
            let _ = handle_http(
                socket,
                &endpoint.host,
                &endpoint.path,
                &endpoint.mime,
                endpoint.length,
                endpoint.index,
                endpoint.torrent,
            )
            .await;
        }
    })
    .await;
}
async fn serve_connections<F, Fut>(listener: TcpListener, cancel: CancellationToken, handle: F)
where
    F: Fn(TcpStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    // Oldest first. A player abandons the previous range request when it
    // seeks; that handler may sit on a missing piece until the idle timeout.
    // The newest request wins the slot instead of being refused.
    let mut order: VecDeque<tokio::task::AbortHandle> = VecDeque::new();
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            Some(done) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                let id = match done { Ok((id, ())) => id, Err(error) => error.id() };
                order.retain(|task| task.id() != id);
            },
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(accepted) => accepted,
                    // Transient (EMFILE, ECONNABORTED): keep the lease's listener alive.
                    Err(_) => {
                        tokio::select! { biased; _ = cancel.cancelled() => break, _ = tokio::time::sleep(ACCEPT_BACKOFF) => continue }
                    }
                };
                if !peer.ip().is_loopback() { continue; }
                if order.len() >= MAX_HTTP_CLIENTS {
                    if let Some(oldest) = order.pop_front() { oldest.abort(); }
                }
                let cancel = cancel.clone();
                let handler = handle(socket);
                order.push_back(tasks.spawn(async move {
                    tokio::select! { _ = cancel.cancelled() => (), _ = handler => () }
                }));
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}
async fn error_response(socket: &mut TcpStream, code: &str, extra: &str) -> Result<()> {
    socket
        .write_all(
            format!("HTTP/1.1 {code}\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n")
                .as_bytes(),
        )
        .await?;
    Ok(())
}
async fn handle_http(
    socket: TcpStream,
    host: &str,
    path: &str,
    mime: &str,
    length: u64,
    index: usize,
    torrent: Arc<ManagedTorrent>,
) -> Result<()> {
    handle_http_with_reader(
        socket,
        host,
        path,
        mime,
        length,
        || torrent.stream(index),
        BODY_IDLE_TIMEOUT,
    )
    .await
}
async fn handle_http_with_reader<R, F>(
    mut socket: TcpStream,
    host: &str,
    path: &str,
    mime: &str,
    length: u64,
    open_reader: F,
    body_idle_timeout: Duration,
) -> Result<()>
where
    R: AsyncRead + AsyncSeek + Unpin,
    F: FnOnce() -> Result<R>,
{
    let header = tokio::time::timeout(Duration::from_secs(10), async {
        let mut header = Vec::new();
        let mut byte = [0u8; 1];
        while !header.ends_with(b"\r\n\r\n") {
            ensure!(header.len() < 8192, "HTTP headers too large");
            socket.read_exact(&mut byte).await?;
            header.push(byte[0]);
        }
        Ok::<_, anyhow::Error>(String::from_utf8(header)?)
    })
    .await??;
    let mut lines = header.split("\r\n");
    let parts: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" {
        return error_response(&mut socket, "400 Bad Request", "").await;
    }
    let method = parts[0];
    if parts[1] != path {
        return error_response(&mut socket, "404 Not Found", "").await;
    }
    if !matches!(method, "GET" | "HEAD") {
        return error_response(
            &mut socket,
            "405 Method Not Allowed",
            "Allow: GET, HEAD\r\n",
        )
        .await;
    }
    let mut hosts = Vec::new();
    let mut ranges = Vec::new();
    for line in lines.take_while(|s| !s.is_empty()) {
        let Some((key, val)) = line.split_once(':') else {
            return error_response(&mut socket, "400 Bad Request", "").await;
        };
        if key.eq_ignore_ascii_case("host") {
            hosts.push(val.trim());
        }
        if key.eq_ignore_ascii_case("range") {
            ranges.push(val.trim());
        }
        if key.eq_ignore_ascii_case("transfer-encoding")
            || (key.eq_ignore_ascii_case("content-length") && val.trim() != "0")
        {
            return error_response(&mut socket, "400 Bad Request", "").await;
        }
    }
    if hosts != [host] {
        return error_response(&mut socket, "403 Forbidden", "").await;
    }
    let range = if ranges.len() > 1 {
        Err(anyhow!("duplicate range"))
    } else {
        parse_range(ranges.first().copied(), length)
    };
    let range = match range {
        Ok(r) => r,
        Err(_) => {
            return error_response(
                &mut socket,
                "416 Range Not Satisfiable",
                &format!("Content-Range: bytes */{length}\r\nAccept-Ranges: bytes\r\n"),
            )
            .await
        }
    };
    let (start, count, code, extra) = match range {
        Some((s, e)) => (
            s,
            e - s + 1,
            "206 Partial Content",
            format!("Content-Range: bytes {s}-{e}/{length}\r\n"),
        ),
        None => (0, length, "200 OK", String::new()),
    };
    socket.write_all(format!("HTTP/1.1 {code}\r\nContent-Length: {count}\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n{extra}\r\n").as_bytes()).await?;
    if method == "GET" {
        let mut reader = open_reader()?;
        tokio::time::timeout(body_idle_timeout, reader.seek(SeekFrom::Start(start)))
            .await
            .context("torrent body seek idle timeout")??;
        copy_body(&mut reader.take(count), &mut socket, body_idle_timeout).await?;
    }
    socket.shutdown().await?;
    Ok(())
}

// Bound time without progress, NOT total response duration. Checking socket EOF
// would mistake a legitimate TCP write-half-close for a disconnected reader.
// A client gone while pieces are missing therefore releases its slot within
// this deadline; each successful source read / partial socket write resets it.
async fn copy_body<R, W>(reader: &mut R, writer: &mut W, idle: Duration) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = tokio::time::timeout(idle, reader.read(&mut buffer))
            .await
            .context("torrent body read idle timeout")??;
        if read == 0 {
            return Ok(());
        }
        let mut remaining = &buffer[..read];
        while !remaining.is_empty() {
            let written = tokio::time::timeout(idle, writer.write(remaining))
                .await
                .context("torrent body write idle timeout")??;
            ensure!(written > 0, "torrent body socket closed");
            remaining = &remaining[written..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_engine_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<TorrentEngine>();
    }
    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 100).unwrap(), None);
        for (s, expected) in [
            ("bytes=0-9", (0, 9)),
            ("bytes=90-", (90, 99)),
            ("bytes=-10", (90, 99)),
            ("bytes=-200", (0, 99)),
            ("bytes=99-999", (99, 99)),
        ] {
            assert_eq!(parse_range(Some(s), 100).unwrap(), Some(expected));
        }
        for s in [
            "bytes=100-",
            "bytes=9-1",
            "bytes=-0",
            "bytes=0-1,4-5",
            "items=0-1",
            "bytes=18446744073709551616-",
        ] {
            assert!(parse_range(Some(s), 100).is_err(), "{s}");
        }
        assert!(parse_range(Some("bytes=0-"), 0).is_err());
    }
    #[test]
    fn original_indices() {
        let files = vec![
            ("small".into(), 2),
            ("largest".into(), 20),
            ("tie".into(), 20),
        ];
        assert_eq!(select_file(&files, None).unwrap(), 1);
        assert_eq!(select_file(&files, Some(0)).unwrap(), 0);
        assert!(select_file(&files, Some(3)).is_err());
    }
    #[test]
    fn tracker_mapping_and_validation() {
        let h = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        let i = parse_input(&json!({"infoHash":h,"fileIdx":7,"sources":["tracker:https://example.org/a?x=1&y=2","dht:wrong"]})).unwrap();
        assert_eq!(i.file_idx, Some(7));
        let u = url::Url::parse(&i.magnet).unwrap();
        assert!(u
            .query_pairs()
            .any(|(k, v)| k == "tr" && v == "https://example.org/a?x=1&y=2"));
        assert_eq!(i.ignored_sources, vec!["dht:wrong"]);
        for v in [
            json!({"infoHash":"bad"}),
            json!({"infoHash":h,"fileIdx":-1}),
            json!({"infoHash":h,"sources":["tracker:file:///tmp/x"]}),
            json!({"infoHash":h,"sources":["tracker:https://u:p@example.org/x"]}),
        ] {
            assert!(parse_input(&v).is_err());
        }
    }
    #[test]
    fn metadata_paths() {
        for s in ["..", ".", "/tmp", "a/b", "a\\b", "C:", "a\0b", ""] {
            assert!(validate_components(&[s.into()]).is_err());
        }
        assert!(validate_components(&["folder".into(), "movie.mkv".into()]).is_ok());
    }
    #[test]
    fn only_inactive_owned_cache_is_cleaned() {
        let temp = tempfile::tempdir().unwrap();
        let first = create_lease(temp.path(), CancellationToken::new()).unwrap();
        let first_path = first.dir.clone();
        let second = create_lease(temp.path(), CancellationToken::new()).unwrap();
        assert!(first_path.exists());
        let untouched = temp.path().join("torrent-v1/other");
        std::fs::create_dir(&untouched).unwrap();
        drop(first);
        let third = create_lease(temp.path(), CancellationToken::new()).unwrap();
        assert!(!first_path.exists());
        assert!(untouched.exists());
        assert!(second.dir.exists());
        assert!(third.dir.exists());
        assert!(remove_lease_dir(&untouched).is_err());
    }
    async fn offline_fixture() -> Result<(tempfile::TempDir, Arc<Session>, Arc<ManagedTorrent>)> {
        let temp = tempfile::tempdir()?;
        let media = b"0123456789abcdef";
        let file = temp.path().join("fixture.mp4");
        std::fs::write(&file, media)?;
        let meta = librqbit::create_torrent(
            &file,
            librqbit::CreateTorrentOptions {
                piece_length: Some(16384),
                ..Default::default()
            },
        )
        .await?;
        let session = Session::new_with_opts(
            temp.path().into(),
            SessionOptions {
                disable_dht: true,
                disable_dht_persistence: true,
                ..Default::default()
            },
        )
        .await?;
        let torrent = session
            .add_torrent(
                AddTorrent::from_bytes(meta.as_bytes()?),
                Some(AddTorrentOptions {
                    overwrite: true,
                    disable_trackers: true,
                    output_folder: Some(temp.path().to_string_lossy().into()),
                    ..Default::default()
                }),
            )
            .await?
            .into_handle()
            .context("fixture admission")?;
        torrent.wait_until_initialized().await?;
        Ok((temp, session, torrent))
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offline_verified_file_http_contract() -> Result<()> {
        let (_temp, session, torrent) = offline_fixture().await?;
        let media = b"0123456789abcdef";
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let host = listener.local_addr()?.to_string();
        let cancel = CancellationToken::new();
        let server = tokio::spawn(serve(
            listener,
            StreamEndpoint {
                host: host.clone(),
                path: "/stream/test-token".into(),
                mime: "video/mp4".into(),
                length: 16,
                index: 0,
                torrent,
            },
            cancel.clone(),
        ));
        async fn request(host: &str, request: String) -> Result<Vec<u8>> {
            let mut s = TcpStream::connect(host).await?;
            s.write_all(request.as_bytes()).await?;
            // A write-half-close is not a disconnected response reader.
            s.shutdown().await?;
            let mut b = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut b)).await??;
            Ok(b)
        }
        for (method, path, extra, authority, status, body) in [
            (
                "GET",
                "/stream/test-token",
                "",
                host.as_str(),
                "200 OK",
                media.as_slice(),
            ),
            (
                "GET",
                "/stream/test-token",
                "Range: bytes=2-5\r\n",
                host.as_str(),
                "206 Partial Content",
                b"2345".as_slice(),
            ),
            (
                "GET",
                "/stream/test-token",
                "Range: bytes=-3\r\n",
                host.as_str(),
                "206 Partial Content",
                b"def".as_slice(),
            ),
            (
                "HEAD",
                "/stream/test-token",
                "",
                host.as_str(),
                "200 OK",
                b"".as_slice(),
            ),
            (
                "GET",
                "/stream/test-token",
                "Range: bytes=16-\r\n",
                host.as_str(),
                "416 Range Not Satisfiable",
                b"".as_slice(),
            ),
            (
                "GET",
                "/stream/test-token",
                "",
                "evil.example",
                "403 Forbidden",
                b"".as_slice(),
            ),
            (
                "POST",
                "/stream/test-token",
                "",
                host.as_str(),
                "405 Method Not Allowed",
                b"".as_slice(),
            ),
            (
                "GET",
                "/stream/wrong",
                "",
                host.as_str(),
                "404 Not Found",
                b"".as_slice(),
            ),
        ] {
            let response = request(
                &host,
                format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\n{extra}\r\n"),
            )
            .await?;
            let split = response
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .context("headers")?;
            let headers = std::str::from_utf8(&response[..split])?;
            assert!(
                headers.starts_with(&format!("HTTP/1.1 {status}")),
                "{headers}"
            );
            assert_eq!(&response[split + 4..], body);
            if status.starts_with("206") {
                assert!(headers.contains("Content-Range: bytes"));
            }
        }
        cancel.cancel();
        server.await?;
        assert!(TcpStream::connect(&host).await.is_err());
        session.stop().await;
        Ok(())
    }
    #[tokio::test]
    async fn stop_during_replacement_shutdown_cancels_all_queued_prepares() -> Result<()> {
        let (temp, session, torrent) = offline_fixture().await?;
        let engine = TorrentEngine::new(temp.path().into());
        let old_token = engine.cancellation.lock().unwrap().token.child_token();
        let lease = create_lease(temp.path(), old_token.clone())?;
        let (release, wait) = tokio::sync::oneshot::channel();
        // Hold old shutdown at its server join, without a swarm or wall-clock sleeps.
        *engine.state.lock().await = Some(Active {
            lease,
            torrent,
            server: tokio::spawn(async {
                let _ = wait.await;
            }),
            response: json!({}),
        });
        let input = json!({"infoHash":"0123456789abcdef0123456789abcdef01234567"});
        let mut replacement = Box::pin(engine.prepare(input.clone()));
        tokio::select! {
            biased;
            result = &mut replacement => panic!("shutdown did not wait: {result:?}"),
            _ = std::future::ready(()) => {},
        }
        assert!(old_token.is_cancelled());
        let mut queued = Box::pin(engine.prepare(input.clone()));
        tokio::select! {
            biased;
            result = &mut queued => panic!("queued prepare did not wait: {result:?}"),
            _ = std::future::ready(()) => {},
        }
        let mut first_stop = Box::pin(engine.stop());
        let mut second_stop = Box::pin(engine.stop());
        for stop in [&mut first_stop, &mut second_stop] {
            tokio::select! {
                biased;
                result = stop => panic!("stop did not wait: {result:?}"),
                _ = std::future::ready(()) => {},
            }
        }
        let mut during_stop = Box::pin(engine.prepare(input));
        tokio::select! {
            biased;
            result = &mut during_stop => panic!("prepare did not queue: {result:?}"),
            _ = std::future::ready(()) => {},
        }
        assert_eq!(engine.cancellation.lock().unwrap().stops, 2);
        // Aborting one overlapping stop must NOT reopen the epoch prematurely.
        drop(first_stop);
        assert_eq!(engine.cancellation.lock().unwrap().stops, 1);
        assert!(engine.cancellation.lock().unwrap().token.is_cancelled());
        release.send(()).unwrap();
        let results = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(replacement, queued, second_stop, during_stop)
        })
        .await?;
        for result in [results.0, results.1, results.3] {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("preparation cancelled"));
        }
        results.2?;
        assert!(!engine.cancellation.lock().unwrap().token.is_cancelled());
        assert_eq!(engine.cancellation.lock().unwrap().stops, 0);
        assert!(engine.state.lock().await.is_none());
        // No replacement filesystem lease or metadata resolver was started.
        assert!(std::fs::read_dir(temp.path().join("torrent-v1"))?
            .all(|e| e.unwrap().file_name() == ".manager.lock"));
        session.stop().await;
        Ok(())
    }

    struct StalledReader {
        started: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    }
    impl AsyncRead for StalledReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            std::task::Poll::Pending
        }
    }
    impl AsyncSeek for StalledReader {
        fn start_seek(self: std::pin::Pin<&mut Self>, _: SeekFrom) -> std::io::Result<()> {
            Ok(())
        }
        fn poll_complete(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<u64>> {
            std::task::Poll::Ready(Ok(0))
        }
    }
    /// An HTTP server whose every body read stalls, reporting each handler's
    /// start and its error (if any) on completion.
    struct StalledServer {
        host: String,
        cancel: CancellationToken,
        server: JoinHandle<()>,
        started_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
        done_rx: tokio::sync::mpsc::UnboundedReceiver<Option<String>>,
    }
    async fn stalled_server() -> Result<StalledServer> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let host = listener.local_addr()?.to_string();
        let server_host = host.clone();
        let cancel = CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::mpsc::unbounded_channel();
        let (done_tx, done_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(serve_connections(listener, cancel.clone(), move |socket| {
            let host = server_host.clone();
            let started = started_tx.clone();
            let done = done_tx.clone();
            async move {
                let result = handle_http_with_reader(
                    socket,
                    &host,
                    "/test",
                    "video/mp4",
                    16,
                    || {
                        Ok(StalledReader {
                            started: Some(started),
                        })
                    },
                    BODY_IDLE_TIMEOUT,
                )
                .await;
                let _ = done.send(result.err().map(|e| e.to_string()));
            }
        }));
        Ok(StalledServer {
            host,
            cancel,
            server,
            started_rx,
            done_rx,
        })
    }
    /// Occupies every HTTP slot with a GET whose body is pending.
    async fn fill_slots(
        host: &str,
        started: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    ) -> Result<Vec<TcpStream>> {
        let mut clients = Vec::new();
        for _ in 0..MAX_HTTP_CLIENTS {
            let mut client = TcpStream::connect(host).await?;
            client
                .write_all(format!("GET /test HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
                .await?;
            tokio::time::timeout(Duration::from_secs(5), started.recv())
                .await?
                .context("mock reader not polled")?;
            clients.push(client);
        }
        Ok(clients)
    }
    #[tokio::test]
    async fn closed_clients_with_stalled_pieces_release_all_http_slots() -> Result<()> {
        let StalledServer {
            host,
            cancel,
            server,
            mut started_rx,
            mut done_rx,
        } = stalled_server().await?;
        let clients = fill_slots(&host, &mut started_rx).await?;
        // Every source is now deterministically pending, with all four slots held.
        // Advance only after socket setup: paused-time auto-advance and real IO
        // must not race each other during setup or subsequent HTTP assertions.
        tokio::time::pause();
        drop(clients);
        tokio::time::advance(BODY_IDLE_TIMEOUT).await;
        tokio::time::resume();
        for _ in 0..MAX_HTTP_CLIENTS {
            let error = tokio::time::timeout(Duration::from_secs(5), done_rx.recv())
                .await?
                .context("handler did not release slot")?
                .context("stalled body succeeded")?;
            assert!(error.contains("body read idle timeout"), "{error}");
        }
        // The same accept loop/cap now admits another request. HEAD + Range must
        // finish even though opening/reading the mocked payload would stall.
        let mut client = TcpStream::connect(&host).await?;
        client
            .write_all(
                format!("HEAD /test HTTP/1.1\r\nHost: {host}\r\nRange: bytes=2-5\r\n\r\n")
                    .as_bytes(),
            )
            .await?;
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(5), client.read_to_string(&mut response))
            .await??;
        assert!(response.starts_with("HTTP/1.1 206 Partial Content"));
        assert!(response.contains("Content-Length: 4\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert!(tokio::time::timeout(Duration::from_secs(5), done_rx.recv())
            .await?
            .context("HEAD handler completion")?
            .is_none());
        assert!(started_rx.try_recv().is_err(), "HEAD opened the source");
        cancel.cancel();
        server.await?;
        Ok(())
    }

    #[tokio::test]
    async fn newest_request_replaces_oldest_stalled_handler() -> Result<()> {
        let StalledServer {
            host,
            cancel,
            server,
            mut started_rx,
            ..
        } = stalled_server().await?;
        // Abandoned seeks: every slot waits on a missing piece, clients still open.
        let mut clients = fill_slots(&host, &mut started_rx).await?;
        // Without eviction this request would be dropped until a 60 s idle timeout.
        let mut client = TcpStream::connect(&host).await?;
        client
            .write_all(
                format!("HEAD /test HTTP/1.1\r\nHost: {host}\r\nRange: bytes=0-3\r\n\r\n")
                    .as_bytes(),
            )
            .await?;
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(5), client.read_to_string(&mut response))
            .await??;
        assert!(
            response.starts_with("HTTP/1.1 206 Partial Content"),
            "{response}"
        );
        // The oldest stalled request was the one closed.
        let mut oldest = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            clients[0].read_to_string(&mut oldest),
        )
        .await??;
        assert!(oldest.starts_with("HTTP/1.1 200 OK"));
        cancel.cancel();
        server.await?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn body_idle_timeout_bounds_both_stalled_reads_and_writes() {
        let mut reader = StalledReader { started: None };
        let mut writer = tokio::io::sink();
        let error = copy_body(&mut reader, &mut writer, BODY_IDLE_TIMEOUT)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("body read idle timeout"));
        let (mut writer, _unread_client) = tokio::io::duplex(1);
        let mut reader = b"ab".as_slice();
        let error = copy_body(&mut reader, &mut writer, BODY_IDLE_TIMEOUT)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("body write idle timeout"));
    }
    #[tokio::test(start_paused = true)]
    async fn body_idle_timeout_does_not_limit_total_stream_duration() -> Result<()> {
        let (mut source, mut reader) = tokio::io::duplex(1);
        let (mut writer, mut client) = tokio::io::duplex(1);
        let transfer =
            tokio::spawn(
                async move { copy_body(&mut reader, &mut writer, BODY_IDLE_TIMEOUT).await },
            );
        let start = tokio::time::Instant::now();
        for byte in b"0123456789" {
            source.write_all(&[*byte]).await?;
            let mut received = [0];
            client.read_exact(&mut received).await?;
            assert_eq!(received, [*byte]);
            tokio::time::advance(BODY_IDLE_TIMEOUT / 2).await;
        }
        assert!(start.elapsed() > BODY_IDLE_TIMEOUT);
        drop(source);
        transfer.await??;
        Ok(())
    }

    #[tokio::test]
    async fn unpolled_preparation_is_registered_before_stop() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let blocked = temp.path().join("blocked-cache");
        // Even a regression cannot create a network session: this is not a directory.
        std::fs::write(&blocked, b"no network")?;
        let engine = TorrentEngine::new(blocked);
        let future = engine.prepare(json!({"infoHash":"0123456789012345678901234567890123456789"}));
        engine.stop().await?;
        let error = tokio::time::timeout(Duration::from_secs(1), future)
            .await?
            .unwrap_err();
        assert!(error.to_string().contains("preparation cancelled"));
        Ok(())
    }
    #[tokio::test]
    async fn scoped_stop_does_not_cancel_newer_preparation_epoch() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let engine = TorrentEngine::new(temp.path().into());
        let token = engine.cancellation.lock().unwrap().token.child_token();
        engine.stop_lease("retired-lease").await?;
        assert!(!token.is_cancelled());
        engine.stop().await?;
        assert!(token.is_cancelled());
        Ok(())
    }

    #[tokio::test]
    async fn idle_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let engine = TorrentEngine::new(temp.path().into());
        fn future_is_send<T: Send>(_: T) {}
        future_is_send(engine.prepare(json!({"infoHash":"bad"})));
        future_is_send(engine.stop());
        future_is_send(engine.stats());
        assert_eq!(engine.stats().await.unwrap()["active"], false);
        engine.stop().await.unwrap();
        assert!(engine.prepare(json!({"infoHash":"bad"})).await.is_err());
    }
}
