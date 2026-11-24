//! Network-independent startup, Stremio addon protocol, and local media state.
//! Stream objects are returned verbatim and cached only in volatile memory.
mod addon;
mod db;
mod local_history;
mod recommendations;
mod subtitle;
mod subtitle_cache;

use addon::Addon;
use anyhow::{anyhow, bail, ensure, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{sync::Semaphore, task::JoinSet};

/// Bounds one user search: a hostile manifest can otherwise declare thousands of
/// search catalogs and make the whole manifest get copied once per catalog.
const MAX_SEARCH_CATALOGS_PER_ADDON: usize = 32;
const MAX_SEARCH_TASKS: usize = 64;
/// Manifest id of the legacy embedded metadata addon (kept for history migration compatibility).
const LOCAL_ADDON_ID: &str = "com.aio.metadata.rs";

/// Which loopback port the embedded metadata addon is actually serving on.
/// `Unknown` (standalone core, tests) keeps every installed addon usable.
#[derive(Clone, Copy, Debug, PartialEq)]
enum LocalAddon {
    Unknown,
    Running(Option<u16>),
}

/// One catalog query of a search plan.
struct SearchTarget {
    addon: Arc<Addon>,
    kind: String,
    id: String,
    extra: Map<String, Value>,
}

#[derive(Clone)]
pub struct Core {
    inner: Arc<Inner>,
}
struct Inner {
    db: Mutex<rusqlite::Connection>,
    client: reqwest::Client,
    network: Semaphore,
    streams: Mutex<HashMap<String, (Instant, Value, usize)>>,
    subtitles: subtitle_cache::SubtitleCache,
    local_addon: Mutex<LocalAddon>,
    /// Parsed installed manifests, dropped whenever the addons table changes.
    addons: Mutex<Option<Vec<Addon>>>,
}
impl Core {
    /// Creates/migrates local storage synchronously. No network requests occur here.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        Ok(Self {
            inner: Arc::new(Inner {
                db: Mutex::new(db::open(&data_dir)?),
                client: addon::client()?,
                network: Semaphore::new(8),
                streams: Mutex::new(HashMap::new()),
                subtitles: subtitle_cache::SubtitleCache::new(&data_dir),
                local_addon: Mutex::new(LocalAddon::Unknown),
                addons: Mutex::new(None),
            }),
        })
    }
    fn with_db<T>(&self, f: impl FnOnce(&mut rusqlite::Connection) -> Result<T>) -> Result<T> {
        let mut db = self
            .inner
            .db
            .lock()
            .map_err(|_| anyhow!("Storage lock poisoned"))?;
        f(&mut db)
    }
    fn installed(&self) -> Result<Vec<Addon>> {
        // Read and fill the cache under the storage lock; every write to the addons table clears
        // it under the same lock, so a reader never sees a list older than the table.
        self.with_db(|db| {
            if let Some(cached) = self.inner.addons.lock().ok().and_then(|a| a.clone()) {
                return Ok(cached);
            }
            let addons = read_installed(db)?;
            if let Ok(mut cache) = self.inner.addons.lock() {
                *cache = Some(addons.clone());
            }
            Ok(addons)
        })
    }
    /// Forget the parsed manifests after any write to the addons table.
    fn addons_changed(&self) {
        if let Ok(mut cache) = self.inner.addons.lock() {
            *cache = None;
        }
    }
    /// Records the manifest URL of the embedded metadata addon this process is
    /// serving (None when it failed to start). Older loopback registrations stay
    /// installed only as history identities: their port may now belong to
    /// another app, and loopback HTTP needs no consent, so they are never
    /// fetched again.
    pub fn set_local_addon(&self, manifest_url: Option<&str>) {
        let port = manifest_url
            .and_then(|url| url::Url::parse(url).ok())
            .and_then(|url| url.port());
        if let Ok(mut local) = self.inner.local_addon.lock() {
            *local = LocalAddon::Running(port);
        }
    }
    /// Removes cached addon responses whose URL starts with `prefix`, e.g. after
    /// the embedded addon's TMDB key or language changes.
    pub fn purge_cached_responses(&self, prefix: &str) -> Result<usize> {
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        self.with_db(|db| {
            Ok(db.execute(
                "DELETE FROM cache WHERE key LIKE ?1 || '%' ESCAPE '\\'",
                [escaped],
            )?)
        })
    }
    /// Delete the cached responses of one addon: keys are its base path, a resource, then (for
    /// an addon whose manifest URL has a query) that same query. Other addons on the same host,
    /// or the same path with another query, keep theirs.
    fn purge_addon_responses(&self, manifest_url: &str) -> Result<usize> {
        let mut base = url::Url::parse(manifest_url).map_err(|_| anyhow!("Invalid addon URL"))?;
        let query = base.query().map(|q| format!("?{q}")).unwrap_or_default();
        base.set_query(None);
        base.set_fragment(None);
        let prefix = base
            .as_str()
            .strip_suffix("manifest.json")
            .context("Expected manifest.json URL")?
            .to_owned();
        self.with_db(|db| {
            let mut removed = 0;
            for resource in ["catalog", "meta", "stream", "subtitles"] {
                let start = format!("{prefix}{resource}/");
                removed += db.execute(
                    "DELETE FROM cache WHERE substr(key,1,length(?1))=?1
                     AND (CASE WHEN ?2='' THEN instr(key,'?')=0 ELSE substr(key,-length(?2))=?2 END)",
                    params![start, query],
                )?;
            }
            Ok(removed)
        })
    }
    /// Embedded-addon revision (`?v=`) for a loopback registration of it.
    fn local_revision(a: &Addon) -> Option<(u16, u64)> {
        if a.manifest["id"] != LOCAL_ADDON_ID {
            return None;
        }
        let url = local_history::local_manifest(&a.url).ok()?;
        let revision = url
            .query_pairs()
            .find(|(key, _)| key == "v")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        Some((url.port()?, revision))
    }
    /// Installed addons that may be queried. Excludes embedded-addon
    /// registrations not on the running port, and keeps only the newest
    /// revision on that port, so retained history revisions never add
    /// duplicate (or foreign) search, stream, subtitle or refresh targets.
    fn active(&self) -> Result<Vec<Addon>> {
        let running = *self
            .inner
            .local_addon
            .lock()
            .map_err(|_| anyhow!("Addon state lock poisoned"))?;
        let installed = self.installed()?;
        let newest = installed
            .iter()
            .filter_map(Self::local_revision)
            .filter(|(port, _)| match running {
                LocalAddon::Running(current) => current == Some(*port),
                LocalAddon::Unknown => true,
            })
            .map(|(_, revision)| revision)
            .max();
        let mut kept_local = false;
        Ok(installed
            .into_iter()
            .filter(|a| match Self::local_revision(a) {
                None => true,
                Some((port, revision)) => {
                    let running_port = match running {
                        LocalAddon::Running(current) => current == Some(port),
                        LocalAddon::Unknown => true,
                    };
                    // One registration per newest revision, first one wins.
                    let keep = running_port && Some(revision) == newest && !kept_local;
                    kept_local |= keep;
                    keep
                }
            })
            .collect())
    }
    fn get_addon(&self, url: &str) -> Result<Addon> {
        // This normalization does not grant consent; only an installed URL is accepted.
        let url = addon::normalize(url, true)?;
        let found = self
            .installed()?
            .into_iter()
            .find(|a| a.url == url)
            .context("Addon is not installed")?;
        if Self::local_revision(&found).is_none() {
            return Ok(found);
        }
        // A retired embedded-addon revision is answered by the running one.
        let active = self.active()?;
        if active.iter().any(|a| a.url == found.url) {
            return Ok(found);
        }
        active
            .into_iter()
            .find(|a| Self::local_revision(a).is_some())
            .context("The embedded metadata addon is not running")
    }
    async fn fetch(
        &self,
        url: &str,
        allow_http: bool,
        ttl: i64,
        cap: i64,
    ) -> Result<addon::Fetched> {
        let _permit = self
            .inner
            .network
            .acquire()
            .await
            .context("Network scheduler closed")?;
        addon::fetch(&self.inner.client, url, allow_http, ttl, cap).await
    }
    async fn install(&self, url: &str, allow_http: bool) -> Result<Value> {
        let url = addon::normalize(url, allow_http)?;
        let fetched = self.fetch(&url, allow_http, 3600, 86400).await?;
        let manifest = addon::validate_manifest(fetched.body)?;
        self.with_db(|db| {
            db.execute(
                "INSERT INTO addons(url,manifest,allow_http,generation) VALUES(?1,?2,?3,lower(hex(randomblob(16))))
                 ON CONFLICT(url) DO UPDATE SET manifest=excluded.manifest,allow_http=excluded.allow_http,generation=excluded.generation",
                params![url,manifest.to_string(),allow_http],
            )?;
            self.addons_changed();
            Ok(())
        })?;
        Ok(Addon {
            url,
            manifest: manifest.into(),
            allow_http,
            generation: String::new(),
        }
        .descriptor())
    }
    /// Requests use an `op` discriminator. No DB guard is held across await.
    pub async fn execute(&self, request: Value) -> Result<Value> {
        ensure!(request.is_object(), "Request must be an object");
        match text(&request, "op")? {
            "addons" => Ok(
                json!({"addons":self.installed()?.iter().map(Addon::descriptor).collect::<Vec<_>>()}),
            ),
            "install_addon" => {
                self.install(
                    text(&request, "url")?,
                    boolean(&request, "allow_http", false)?,
                )
                .await
            }
            "remove_addon" => {
                let url = addon::normalize(text(&request, "url")?, true)?;
                let removed = self.with_db(|db| {
                    let removed = db.execute("DELETE FROM addons WHERE url=?1", [&url])? > 0;
                    self.addons_changed();
                    Ok(removed)
                })?;
                self.inner
                    .streams
                    .lock()
                    .map_err(|_| anyhow!("Stream lock poisoned"))?
                    .clear();
                // Cached responses are keyed by the addon's (possibly secret-bearing) URL. The
                // addon is already gone: a failed purge leaves them to normal eviction.
                if removed {
                    let _ = self.purge_addon_responses(&url);
                }
                Ok(json!({"removed":removed}))
            }
            "refresh_addons" => self.refresh().await,
            "catalog" => self.catalog_request(&request).await,
            "meta" => self.meta_request(&request).await,
            "streams" => self.streams(&request).await,
            "streams_targets" => self.streams_targets(&request).await,
            "subtitles" => self.subtitles(&request).await,
            "subtitle_file" => self.subtitle_file(&request).await,
            "subtitle_cache" => self.inner.subtitles.stats(),
            "clear_subtitle_cache" => {
                self.inner.subtitles.clear()?;
                self.inner.subtitles.stats()
            }
            "search" => self.search(&request).await,
            "search_targets" => self.search_targets(&request).await,
            "library" => {
                let items = self
                    .with_db(|db| db::library(db))?
                    .into_iter()
                    .map(|m| m.value())
                    .collect::<Vec<_>>();
                Ok(json!({"items":items}))
            }
            "personal" => {
                let limit = limit(&request)?;
                let all = self.with_db(|db| db::personal_media(db))?;
                let library = all
                    .iter()
                    .filter(|m| m.in_library())
                    .map(|m| m.value())
                    .collect::<Vec<_>>();
                let recommendations = recommendations::recommend(&all, None, limit);
                let collections = recommendations::collections(&all);
                Ok(
                    json!({"library":library,"recommendations":recommendations["metas"],
                    "collections":collections["collections"],"needs_history":recommendations["needs_history"]}),
                )
            }
            "migrate_local_history" => self.migrate_local_history(&request),
            "playback" => self.playback(&request),
            "playback_all" => self.playback_all(&request),
            "watched_videos" => self.watched_videos(&request),
            "item" => {
                // One title as the store has it (saved, watched, liked, progress), or null.
                let kind = text(&request, "type")?;
                let id = text(&request, "id")?;
                let addon = addon::normalize(text(&request, "addon_url")?, true)?;
                let key = db::identity(kind, id, &addon);
                let item = self.with_db(|db| db::media_item(db, &key))?;
                Ok(json!({ "item": item.map(|m| m.value()) }))
            }
            "history" => self.history(&request),
            "set_library" | "progress" | "mark_watched" | "mark_videos" | "feedback" | "forget" => {
                self.mutate(&request)
            }
            // Sync between a person's devices (the app's account): what changed here, and another's changes.
            "sync_changes" => {
                let since = request["since"].as_i64().unwrap_or(0);
                self.with_db(|db| db::sync_changes(db, since))
            }
            "sync_apply" => self.with_db(|db| db::sync_apply(db, &request)),
            "recommendations" => Ok(recommendations::recommend(
                &self.with_db(|db| db::personal_media(db))?,
                None,
                limit(&request)?,
            )),
            "similar" => {
                let kind = text(&request, "type")?;
                let id = text(&request, "id")?;
                let addon = addon::normalize(text(&request, "addon_url")?, true)?;
                Ok(recommendations::recommend(
                    &self.with_db(|db| db::personal_media(db))?,
                    Some(&db::identity(kind, id, &addon)),
                    limit(&request)?,
                ))
            }
            "collections" => Ok(recommendations::collections(
                &self.with_db(|db| db::personal_media(db))?,
            )),
            "diagnostics" => self.diagnostics(),
            _ => bail!("Unknown operation"),
        }
    }
    async fn refresh_addon(&self, a: &Addon) -> Result<Value> {
        let fetched = self.fetch(&a.url, a.allow_http, 3600, 86400).await?;
        let manifest = addon::validate_manifest(fetched.body)?;
        let updated = self.with_db(|db| {
            let updated = db.execute(
                "UPDATE addons SET manifest=?2,generation=lower(hex(randomblob(16))) WHERE url=?1 AND generation=?3",
                params![a.url,manifest.to_string(),a.generation],
            )? > 0;
            self.addons_changed();
            Ok(updated)
        })?;
        ensure!(
            updated,
            "Addon changed or was removed during refresh; stale response ignored"
        );
        let mut refreshed = a.clone();
        refreshed.manifest = manifest.into();
        Ok(refreshed.descriptor())
    }
    async fn refresh(&self) -> Result<Value> {
        let mut jobs = JoinSet::new();
        for (i, a) in self.active()?.into_iter().enumerate() {
            let core = self.clone();
            jobs.spawn(async move {
                let result = core.refresh_addon(&a).await;
                (
                    i,
                    match result {
                        Ok(v) => json!({"addon":v}),
                        Err(e) => {
                            json!({"addon":a.descriptor(),"error":addon::redact(e.to_string())})
                        }
                    },
                )
            });
        }
        let results = collect(&mut jobs).await?;
        Ok(
            json!({"results":results,"addons":self.installed()?.iter().map(Addon::descriptor).collect::<Vec<_>>()}),
        )
    }
    async fn catalog_request(&self, req: &Value) -> Result<Value> {
        let a = self.get_addon(text(req, "addon_url")?)?;
        self.catalog(
            &a,
            text(req, "type")?,
            text(req, "id")?,
            &extras(req)?,
            boolean(req, "force", false)?,
        )
        .await
    }
    async fn catalog(
        &self,
        a: &Addon,
        kind: &str,
        id: &str,
        extras: &Map<String, Value>,
        force: bool,
    ) -> Result<Value> {
        let catalog = a
            .catalog(kind, id)
            .context("Catalog not declared by addon")?;
        addon::validate_extras(catalog, extras)?;
        let url = addon::endpoint(&a.url, "catalog", kind, id, extras)?;
        self.cached_resource(a, &url, kind, "metas", force, 900)
            .await
    }
    async fn meta_request(&self, req: &Value) -> Result<Value> {
        let a = self.get_addon(text(req, "addon_url")?)?;
        let kind = text(req, "type")?;
        let id = text(req, "id")?;
        ensure!(
            a.supports("meta", kind, id),
            "Addon does not support this metadata identity"
        );
        let url = addon::endpoint(&a.url, "meta", kind, id, &Map::new())?;
        self.cached_resource(&a, &url, kind, "meta", boolean(req, "force", false)?, 21600)
            .await
    }
    async fn cached_resource(
        &self,
        a: &Addon,
        url: &str,
        kind: &str,
        field: &str,
        force: bool,
        default_ttl: i64,
    ) -> Result<Value> {
        let stored: Option<(String, i64)> = self.with_db(|db| {
            Ok(db
                .query_row("SELECT body,expires FROM cache WHERE key=?1", [url], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?)
        })?;
        let cached =
            stored.and_then(|(s, e)| serde_json::from_str::<Value>(&s).ok().map(|v| (v, e)));
        if !force {
            if let Some((v, e)) = &cached {
                if *e > db::now() {
                    let mut v = v.clone();
                    v["cached"] = json!(true);
                    return Ok(v);
                }
            }
        }
        let fetched = match self.fetch(url, a.allow_http, default_ttl, 86400).await {
            Ok(f) => f,
            Err(e) => return stale_or_error(cached, e),
        };
        let mut body = fetched.body;
        if field == "metas" {
            if body.get("metas").is_none() {
                body["metas"] = json!([]);
            }
            if !body["metas"].is_array() {
                return stale_or_error(cached, anyhow!("Addon metas must be an array"));
            }
        } else if !body.get("meta").is_some_and(Value::is_object) {
            return stale_or_error(cached, anyhow!("Addon did not return metadata"));
        }
        // Preserve inline metadata streams in fresh responses, never in persistent caches.
        let has_inline_streams = db::contains_streams(&body);
        let mut persistent_body = body.clone();
        db::strip_streams(&mut persistent_body);
        self.with_db(|db| {
            if fetched.persist {
                // Dropping inline streams would change exclusive playback semantics. Never
                // serve a stripped resource response; keep only a sanitized ranking index.
                if has_inline_streams {
                    db.execute("DELETE FROM cache WHERE key=?1",[url])?;
                } else {
                db.execute(
                    "INSERT INTO cache(key,body,expires,updated) VALUES(?1,?2,?3,?4)
                     ON CONFLICT(key) DO UPDATE SET body=excluded.body,expires=excluded.expires,updated=excluded.updated",
                    params![url,persistent_body.to_string(),db::now()+fetched.ttl,db::now()],
                )?;
                }
                db::cache_metas(db,&persistent_body,kind,&a.url)?;
                // Keep at most 500 resource responses and 64 MiB of raw cache bodies.
                db.execute("DELETE FROM cache WHERE key IN (SELECT key FROM cache ORDER BY updated DESC,key LIMIT -1 OFFSET 500)",[])?;
                db.execute("DELETE FROM cache WHERE key IN (
                    SELECT key FROM (SELECT key,SUM(length(CAST(body AS BLOB))) OVER (ORDER BY updated DESC,key) AS bytes FROM cache)
                    WHERE bytes>67108864)",[])?;
                // Saved/history/feedback rows are never removed by catalog eviction.
                db.execute("DELETE FROM media WHERE saved=0 AND watched=0 AND position_ms=0 AND watched_ms=0 AND liked=0 AND key IN (
                    SELECT key FROM media WHERE saved=0 AND watched=0 AND position_ms=0 AND watched_ms=0 AND liked=0
                    ORDER BY updated DESC,key LIMIT -1 OFFSET 10000)",[])?;
            } else {
                db.execute("DELETE FROM cache WHERE key=?1",[url])?;
            }
            Ok(())
        })?;
        body["cached"] = json!(false);
        Ok(body)
    }
    fn matching(&self, req: &Value, resource: &str, kind: &str, id: &str) -> Result<Vec<Addon>> {
        let addons = if let Some(url) = req.get("addon_url") {
            vec![self.get_addon(url.as_str().context("addon_url must be a string")?)?]
        } else {
            self.active()?
        };
        Ok(addons
            .into_iter()
            .filter(|a| a.supports(resource, kind, id))
            .collect())
    }
    async fn streams_targets(&self, req: &Value) -> Result<Value> {
        let kind = text(req, "type")?;
        let id = text(req, "id")?;
        let targets = self
            .matching(req, "stream", kind, id)?
            .into_iter()
            .map(|a| json!({"addon_name": a.name(), "addon_url": a.url}))
            .collect::<Vec<_>>();
        Ok(json!({"targets": targets}))
    }
    async fn streams(&self, req: &Value) -> Result<Value> {
        let kind = text(req, "type")?;
        let id = text(req, "id")?;
        // An optional addon_url narrows this to one addon (resolved and
        // normalized by `matching`): the UI fans out one call per addon so
        // results stream in instead of waiting for the slowest addon.
        let force = req["force"] == true;
        let mut jobs = JoinSet::new();
        for (i, a) in self
            .matching(req, "stream", kind, id)?
            .into_iter()
            .enumerate()
        {
            let url = addon::endpoint(&a.url, "stream", kind, id, &Map::new())?;
            let core = self.clone();
            jobs.spawn(async move {
                let result=core.stream_resource(&a,&url,force).await;
                let group=match result {
                    Ok(v)=>json!({"addon_url":a.url,"addon_name":a.name(),"streams":v}),
                    Err(e)=>json!({"addon_url":a.url,"addon_name":a.name(),"streams":[],"error":addon::redact(e.to_string())}),
                };
                (i,group)
            });
        }
        Ok(json!({"groups":collect(&mut jobs).await?}))
    }
    async fn stream_resource(&self, a: &Addon, url: &str, force: bool) -> Result<Value> {
        if !force {
            let mut cache = self
                .inner
                .streams
                .lock()
                .map_err(|_| anyhow!("Stream lock poisoned"))?;
            cache.retain(|_, (until, _, _)| *until > Instant::now());
            if let Some((_, v, _)) = cache.get(url) {
                return Ok(v.clone());
            }
        }
        let fetched = self.fetch(url, a.allow_http, 60, 180).await?;
        let streams = fetched.body.get("streams").cloned().unwrap_or(json!([]));
        ensure!(streams.is_array(), "Addon streams must be an array");
        if fetched.persist && fetched.ttl > 0 {
            let mut cache = self
                .inner
                .streams
                .lock()
                .map_err(|_| anyhow!("Stream lock poisoned"))?;
            const MAX_BYTES: usize = 16 * 1024 * 1024;
            let size = serde_json::to_vec(&streams)?.len();
            cache.retain(|_, (until, _, _)| *until > Instant::now());
            if cache.len() >= 64
                || cache.values().map(|(_, _, size)| size).sum::<usize>() + size > MAX_BYTES
            {
                cache.clear();
            }
            if size <= MAX_BYTES {
                cache.insert(
                    url.to_owned(),
                    (
                        Instant::now() + Duration::from_secs(fetched.ttl as u64),
                        streams.clone(),
                        size,
                    ),
                );
            }
        }
        Ok(streams)
    }
    async fn subtitles(&self, req: &Value) -> Result<Value> {
        let kind = text(req, "type")?;
        let id = text(req, "id")?;
        let extra = extras(req)?;
        let mut jobs = JoinSet::new();
        for (i, a) in self
            .matching(req, "subtitles", kind, id)?
            .into_iter()
            .enumerate()
        {
            let url = addon::endpoint(&a.url, "subtitles", kind, id, &extra)?;
            let core = self.clone();
            jobs.spawn(async move {
                let v=match core.fetch(&url,a.allow_http,0,0).await {
                    Ok(f)=>match f.body.get("subtitles") {
                        None=>json!({"subtitles":[]}),
                        Some(v) if v.is_array()=>json!({"subtitles":v}),
                        _=>json!({"subtitles":[],"error":"Addon subtitles must be an array","addon_url":a.url}),
                    },
                    Err(e)=>json!({"subtitles":[],"error":addon::redact(e.to_string()),"addon_url":a.url}),
                };
                (i,v)
            });
        }
        let groups = collect(&mut jobs).await?;
        let subtitles = groups
            .iter()
            .filter_map(|g| g["subtitles"].as_array())
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let errors = groups
            .iter()
            .filter(|g| g.get("error").is_some())
            .cloned()
            .collect::<Vec<_>>();
        Ok(json!({"subtitles":subtitles,"errors":errors}))
    }
    /// The catalogs one search queries: bounded per addon and overall, with the
    /// request extras validated once. Shared by `search_targets` and `search`.
    fn search_plan(&self, req: &Value) -> Result<Vec<SearchTarget>> {
        let query = text(req, "query")?.trim();
        ensure!(!query.is_empty(), "Search query is empty");
        let mut base = extras(req)?;
        base.insert("search".into(), json!(query));
        let mut plan = Vec::new();
        'addons: for a in self.active()? {
            // One immutable copy per addon: the manifest is large and untrusted.
            let addon = Arc::new(a);
            let mut catalogs = 0;
            for c in addon.manifest["catalogs"].as_array().into_iter().flatten() {
                if !addon::searchable(c) {
                    continue;
                }
                let (Some(kind), Some(id)) = (c["type"].as_str(), c["id"].as_str()) else {
                    continue;
                };
                if plan.len() >= MAX_SEARCH_TASKS {
                    break 'addons;
                }
                if catalogs >= MAX_SEARCH_CATALOGS_PER_ADDON {
                    break; // this addon is capped; the remaining addons still search
                }
                if addon::validate_extras(c, &base).is_err() {
                    continue;
                }
                catalogs += 1;
                plan.push(SearchTarget {
                    addon: Arc::clone(&addon),
                    kind: kind.to_owned(),
                    id: id.to_owned(),
                    extra: base.clone(),
                });
            }
        }
        Ok(plan)
    }
    /// Non-network search plan: exactly the catalogs `search` would query, so the
    /// UI can fan out per catalog and stream results as each addon answers.
    async fn search_targets(&self, req: &Value) -> Result<Value> {
        let targets = self
            .search_plan(req)?
            .into_iter()
            .map(|t| {
                json!({
                    "addon_name": t.addon.name(),
                    "addon_url": t.addon.url,
                    "catalog_id": t.id,
                    "type": t.kind,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"targets":targets}))
    }
    async fn search(&self, req: &Value) -> Result<Value> {
        let mut jobs = JoinSet::new();
        for (i, target) in self.search_plan(req)?.into_iter().enumerate() {
            let core = self.clone();
            jobs.spawn(async move {
                let SearchTarget { addon: a, kind, id, extra } = target;
                let result=core.catalog(&a,&kind,&id,&extra,false).await;
                let mut group=json!({"addon_name":a.name(),"addon_url":a.url,"catalog_id":id,"type":kind,"metas":[]});
                match result {
                    Ok(v)=>{
                        group["metas"]=v["metas"].clone();
                        group["cached"]=v["cached"].clone();
                        if let Some(e)=v.get("error") {group["error"]=e.clone();}
                        if let Some(stale)=v.get("stale") {group["stale"]=stale.clone();}
                    },
                    Err(e)=>group["error"]=json!(addon::redact(e.to_string())),
                }
                (i,group)
            });
        }
        Ok(json!({"groups":collect(&mut jobs).await?}))
    }
    fn migrate_local_history(&self, req: &Value) -> Result<Value> {
        let from = addon::normalize(text(req, "from_addon_url")?, true)?;
        let to = addon::normalize(text(req, "addon_url")?, true)?;
        local_history::local_manifest(&from)?;
        local_history::local_manifest(&to)?;
        let destination = self.get_addon(&to)?;
        ensure!(
            destination.manifest["id"] == "com.aio.metadata.rs",
            "Not the embedded metadata addon"
        );
        if let Some(source) = self.installed()?.iter().find(|addon| addon.url == from) {
            ensure!(
                source.manifest["id"] == "com.aio.metadata.rs",
                "Not the embedded metadata addon"
            );
        }
        let kind = text(req, "type")?;
        let id = text(req, "id")?;
        ensure!(
            destination.supports("meta", kind, id),
            "Unsupported history identity"
        );
        let meta = req.get("meta").context("Metadata is required")?;
        self.with_db(|db| {
            let generation: Option<String> = db
                .query_row("SELECT generation FROM addons WHERE url=?1", [&to], |row| {
                    row.get(0)
                })
                .optional()?;
            ensure!(
                generation.as_deref() == Some(destination.generation.as_str()),
                "Addon changed or was removed during history migration"
            );
            local_history::migrate(db, kind, id, &from, &to, meta)
        })
    }
    fn playback(&self, req: &Value) -> Result<Value> {
        let kind = text(req, "type")?;
        let id = text(req, "id")?;
        let addon = addon::normalize(text(req, "addon_url")?, true)?;
        let video = text(req, "video_id")?;
        let key = db::identity(kind, id, &addon);
        self.with_db(|db| {
            let state: Option<(i64, i64, i64, bool)> = db.query_row(
                "SELECT position_ms,duration_ms,watched_ms,watched FROM playback WHERE media_key=?1 AND video_id=?2",
                params![key,video],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).optional()?;
            let (position,duration,watched_ms,watched) = state.unwrap_or((0,0,0,false));
            Ok(json!({"video_id":video,"position_ms":position,"duration_ms":duration,"watched_ms":watched_ms,"watched":watched}))
        })
    }
    /// All per-episode playback entries for one title (under any addon), for episode progress bars.
    fn playback_all(&self, req: &Value) -> Result<Value> {
        let kind = text(req, "type")?.to_owned();
        let id = text(req, "id")?.to_owned();
        self.with_db(|db| {
            let mut q = db.prepare(
                "SELECT p.video_id, p.position_ms, p.duration_ms, p.watched_ms, p.watched
                 FROM playback p JOIN media m ON m.key=p.media_key
                 WHERE m.type=?1 AND m.raw_id=?2",
            )?;
            let entries = q
                .query_map(params![kind, id], |r| {
                    Ok(json!({
                        "video_id": r.get::<_, String>(0)?,
                        "position_ms": r.get::<_, i64>(1)?,
                        "duration_ms": r.get::<_, i64>(2)?,
                        "watched_ms": r.get::<_, i64>(3)?,
                        "watched": r.get::<_, i64>(4)? != 0,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({ "entries": entries }))
        })
    }
    /// Videos of a title marked watched (by hand, or by measured playback), under any addon.
    fn watched_videos(&self, req: &Value) -> Result<Value> {
        let kind = text(req, "type")?.to_owned();
        let id = text(req, "id")?.to_owned();
        self.with_db(|db| {
            let mut q = db.prepare(
                "SELECT p.video_id FROM playback p JOIN media m ON m.key=p.media_key
                 WHERE m.type=?1 AND m.raw_id=?2 AND p.watched<>0",
            )?;
            let ids = q
                .query_map(params![kind, id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({ "video_ids": ids }))
        })
    }
    /// Everything watched or started, latest first, with when it was last seen (unix seconds).
    fn history(&self, req: &Value) -> Result<Value> {
        let limit = req["limit"].as_u64().unwrap_or(600).clamp(1, 2000) as i64;
        self.with_db(|db| {
            let mut q = db.prepare(
                "SELECT m.type,m.raw_id,m.addon_url,m.meta,m.watched,m.position_ms,m.duration_ms,m.video_id,m.saved,m.liked,
                        COALESCE((SELECT MAX(p.updated) FROM playback p WHERE p.media_key=m.key),m.updated) AS seen
                 FROM media m
                 WHERE m.watched<>0 OR m.position_ms>0 OR m.watched_ms>0 OR EXISTS (SELECT 1 FROM playback p WHERE p.media_key=m.key)
                 ORDER BY seen DESC LIMIT ?1",
            )?;
            let rows = q.query_map([limit], |r| {
                let mut meta: Value =
                    serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_else(|_| json!({}));
                if !meta.is_object() {
                    meta = json!({});
                }
                if let Some(o) = meta.as_object_mut() {
                    o.insert("type".into(), json!(r.get::<_, String>(0)?));
                    o.insert("id".into(), json!(r.get::<_, String>(1)?));
                    o.insert("addon_url".into(), json!(r.get::<_, String>(2)?));
                    o.insert("watched".into(), json!(r.get::<_, i64>(4)? != 0));
                    o.insert("position_ms".into(), json!(r.get::<_, i64>(5)?));
                    o.insert("duration_ms".into(), json!(r.get::<_, i64>(6)?));
                    if let Some(v) = r.get::<_, Option<String>>(7)? {
                        o.insert("video_id".into(), json!(v));
                    }
                    o.insert("library".into(), json!(r.get::<_, i64>(8)? != 0));
                    o.insert("liked".into(), json!(r.get::<_, i64>(9)?));
                    o.insert("seen".into(), json!(r.get::<_, i64>(10)?));
                }
                Ok(meta)
            })?;
            // A row that can't be read is left out, not the whole history.
            Ok(json!({ "items": rows.filter_map(rusqlite::Result::ok).collect::<Vec<_>>() }))
        })
    }
    fn mutate(&self, req: &Value) -> Result<Value> {
        let op = text(req, "op")?;
        let kind = text(req, "type")?;
        let id = text(req, "id")?;
        let addon = addon::normalize(text(req, "addon_url")?, true)?;
        let empty_meta = json!({});
        let meta = req.get("meta").unwrap_or(&empty_meta);
        ensure!(meta.is_object(), "meta must be an object");
        self.with_db(|db| {
            let tx=db.transaction()?;
            let key=db::upsert_meta(&tx,kind,id,&addon,meta)?;
            match op {
                "set_library"=>{
                    tx.execute("UPDATE media SET saved=?2,updated=?3 WHERE key=?1",params![key,boolean(req,"saved",false)?,db::now()])?;
                },
                "mark_watched"=>{
                    let watched=boolean(req,"watched",true)?;
                    tx.execute("UPDATE media SET watched=?2,updated=?3 WHERE key=?1",params![key,watched,db::now()])?;
                    if !watched {
                        // A film watched through is unmarked to watch again: its own playback record
                        // starts over too, or the next report would mark it watched straight away.
                        tx.execute("UPDATE playback SET watched=0,watched_ms=0 WHERE media_key=?1 AND video_id=?2",params![key,id])?;
                        tx.execute("UPDATE media SET watched_ms=(SELECT COALESCE(SUM(watched_ms),0) FROM playback WHERE media_key=?1) WHERE key=?1",params![key])?;
                    }
                },
                "mark_videos"=>{
                    // Episodes you say you watched (or not), without touching where you are.
                    let watched=boolean(req,"watched",true)?;
                    let videos=req["video_ids"].as_array().context("video_ids must be an array")?;
                    ensure!(videos.len()<=2000,"Too many videos");
                    let now=db::now();
                    for v in videos {
                        let v=v.as_str().filter(|v| !v.is_empty() && v.len()<=512).context("Invalid video id")?;
                        if watched {
                            tx.execute(
                                "INSERT INTO playback(media_key,video_id,position_ms,duration_ms,watched_ms,updated,watched) VALUES(?1,?2,0,0,0,?3,1)
                                 ON CONFLICT(media_key,video_id) DO UPDATE SET watched=1,updated=excluded.updated",
                                params![key,v,now],
                            )?;
                        } else {
                            tx.execute("UPDATE playback SET watched=0,watched_ms=0,updated=?3 WHERE media_key=?1 AND video_id=?2",params![key,v,now])?;
                            // A row that only said "watched" goes entirely.
                            tx.execute("DELETE FROM playback WHERE media_key=?1 AND video_id=?2 AND position_ms=0",params![key,v])?;
                        }
                    }
                    // The title's measured total follows its episodes.
                    tx.execute("UPDATE media SET watched_ms=(SELECT COALESCE(SUM(watched_ms),0) FROM playback WHERE media_key=?1),updated=?2 WHERE key=?1",params![key,now])?;
                },
                "forget"=>{
                    // Stop tracking a title: its history, progress, marks and rating all go.
                    tx.execute("DELETE FROM playback WHERE media_key=?1",params![key])?;
                    tx.execute("DELETE FROM media WHERE key=?1",params![key])?;
                },
                "feedback"=>{
                    let value=req["value"].as_i64().context("Feedback value must be -1, 0, or 1")?;
                    ensure!((-1..=1).contains(&value),"Feedback value must be -1, 0, or 1");
                    tx.execute("UPDATE media SET liked=?2,updated=?3 WHERE key=?1",params![key,value,db::now()])?;
                },
                "progress"=>{
                    let position=nonnegative(req,"position_ms",None)?;
                    let duration=nonnegative(req,"duration_ms",None)?;
                    let watched=nonnegative(req,"watched_ms",Some(0))?;
                    // Measured playback since the player's previous report, added to what is stored,
                    // so a film watched over several sessions adds up.
                    let added=nonnegative(req,"watched_add_ms",Some(0))?;
                    let video=if req.get("video_id").is_some() { text(req,"video_id")? } else { id };
                    // Cumulative measured playback per video, never inferred from seeking.
                    tx.execute(
                        "INSERT INTO playback(media_key,video_id,position_ms,duration_ms,watched_ms,updated) VALUES(?1,?2,?3,?4,?5,?6)
                         ON CONFLICT(media_key,video_id) DO UPDATE SET position_ms=excluded.position_ms,duration_ms=excluded.duration_ms,
                         watched_ms=MAX(playback.watched_ms,excluded.watched_ms),updated=excluded.updated",
                        params![key,video,position,duration,watched,db::now()],
                    )?;
                    if added>0 {
                        tx.execute(
                            "UPDATE playback SET watched_ms=MIN(watched_ms+?3,MAX(duration_ms,?4)) WHERE media_key=?1 AND video_id=?2",
                            params![key,video,added,duration],
                        )?;
                    }
                    // Use retained measured time, including when the incoming report omits
                    // it or comes from a fresh player session with a smaller counter.
                    tx.execute(
                        "UPDATE playback SET watched=MAX(watched,duration_ms>0 AND watched_ms*5>=duration_ms*4 AND position_ms*5>=duration_ms*4)
                         WHERE media_key=?1 AND video_id=?2", params![key,video],
                    )?;
                    let total:i64=tx.query_row("SELECT COALESCE(SUM(watched_ms),0) FROM playback WHERE media_key=?1",[&key],|r|r.get(0))?;
                    let auto=kind=="movie" && tx.query_row("SELECT watched FROM playback WHERE media_key=?1 AND video_id=?2",params![key,video],|r|r.get::<_,bool>(0))?;
                    tx.execute(
                        "UPDATE media SET position_ms=?2,duration_ms=?3,watched_ms=?4,video_id=?5,watched=MAX(watched,?6),updated=?7 WHERE key=?1",
                        params![key,position,duration,total,video,auto,db::now()],
                    )?;
                },
                _=>unreachable!(),
            }
            tx.commit()?;
            if op=="forget" { return Ok(json!({"forgotten":key})); }
            let item=db::media_item(db,&key)?.context("History item missing")?;
            Ok(json!({"item":item.value()}))
        })
    }
    fn diagnostics(&self) -> Result<Value> {
        let mut result = self.with_db(|db| {
            let count = |table: &str| -> Result<i64> {
                Ok(db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
            };
            Ok(json!({
                "schema_version":3,"addons":count("addons")?,"cache_entries":count("cache")?,
                "items":count("media")?,"playback_entries":count("playback")?,
                "network_concurrency":8,"stream_urls_persisted":false
            }))
        })?;
        result["memory_stream_entries"] = json!(self
            .inner
            .streams
            .lock()
            .map_err(|_| anyhow!("Stream lock poisoned"))?
            .len());
        Ok(result)
    }

    /// Downloads one subtitle document for the player and returns its parsed cues.
    ///
    /// VLC keeps downloaded subtitle files and reuses them; here the raw file is
    /// cached under the app data directory for the same reason, while the parsed
    /// cues are what the Compose caption consumes. Parsing itself lives in
    /// `subtitle.rs` so the Android layer never duplicates subtitle format logic.
    async fn subtitle_file(&self, req: &Value) -> Result<Value> {
        let url = text(req, "url")?.trim().to_owned();
        let parsed = url::Url::parse(&url).context("Invalid subtitle URL")?;
        let allow_http = boolean(req, "allow_http", false)?;
        addon::validate_transport(&parsed, allow_http)?;
        let is_ass = |bytes: &[u8]| {
            let content = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
            let first = content
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(content.len());
            content[first..].starts_with(b"[Script Info]")
        };
        // Parse a cache hit once; an unreadable cached file is dropped and refetched.
        // ASS/SSA subtitles are passed verbatim to libmpv, so they are cached without cue parsing.
        let cached = self.inner.subtitles.read(&url).and_then(|(bytes, path)| {
            if is_ass(&bytes) {
                return Some((bytes, path, Vec::new(), true));
            }
            match subtitle::parse(&bytes) {
                Ok(cues) if !cues.is_empty() => Some((bytes, path, cues, false)),
                _ => {
                    self.inner.subtitles.remove(&url);
                    None
                }
            }
        });
        let from_cache = cached.is_some();
        let (bytes, existing_path, cacheable, expiry, cues, ass) = match cached {
            Some((bytes, path, cues, ass)) => (bytes, Some(path), true, None, cues, ass),
            None => {
                let (bytes, cacheable, expiry) = self.download_subtitle(&url, allow_http).await?;
                if is_ass(&bytes) {
                    (bytes, None, cacheable, expiry, Vec::new(), true)
                } else {
                    let cues = subtitle::parse(&bytes)?;
                    (bytes, None, cacheable, expiry, cues, false)
                }
            }
        };
        ensure!(ass || !cues.is_empty(), "Subtitle has no readable cues");
        // On a hit keep its original deadline: reading must not refresh it.
        // no-store never leaves a file; no-cache gets an already-expired
        // marker but its path remains available for the user's export action.
        let path = if let Some(path) = existing_path {
            Some(path.to_string_lossy().into_owned())
        } else if cacheable {
            Some(
                self.inner
                    .subtitles
                    .store(&url, &bytes, expiry)?
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            self.inner.subtitles.remove(&url);
            None
        };
        Ok(json!({
            "url": url,
            "count": cues.len(),
            "cached": from_cache,
            "cache_path": path,
            "cues": cues.iter().map(|cue| json!({
                "start_ms": cue.start_ms,
                "end_ms": cue.end_ms,
                "text": cue.text,
            })).collect::<Vec<_>>(),
        }))
    }

    async fn download_subtitle(
        &self,
        url: &str,
        allow_http: bool,
    ) -> Result<(Vec<u8>, bool, Option<std::time::SystemTime>)> {
        let _permit = self
            .inner
            .network
            .acquire()
            .await
            .context("Network scheduler closed")?;
        let mut target = url::Url::parse(url).context("Invalid subtitle URL")?;
        let mut cacheable = true;
        let mut expiry = None;
        for redirects in 0..=3 {
            // A redirect is a separate network request and must not bypass
            // scoped consent or inject URL credentials.
            addon::validate_transport(&target, allow_http)?;
            let sent_at = std::time::SystemTime::now();
            let mut response = self
                .inner
                .client
                .get(target.clone())
                .header(reqwest::header::USER_AGENT, "Veylo/1.0")
                .header(reqwest::header::ACCEPT, "*/*")
                .send()
                .await
                .context("Subtitle download failed")?;
            let received_at = std::time::SystemTime::now();
            let (may_store, hop_expiry) =
                subtitle_freshness(response.headers(), sent_at, received_at);
            cacheable &= may_store;
            let hop_expiry = Some(effective_subtitle_expiry(
                response.status().is_redirection(),
                hop_expiry,
                response.headers(),
                sent_at,
                received_at,
            ));
            expiry = match (expiry, hop_expiry) {
                (Some(a), Some(b)) => Some(std::cmp::min(a, b)),
                (None, value) | (value, None) => value,
            };
            if response.status().is_redirection() {
                ensure!(redirects < 3, "Too many subtitle redirects");
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .context("Invalid subtitle redirect")?;
                target = target
                    .join(location)
                    .map_err(|_| anyhow!("Invalid subtitle redirect"))?;
                continue;
            }
            ensure!(
                response.status().is_success(),
                "Subtitle download failed: HTTP {}",
                response.status()
            );
            if let Some(length) = response.content_length() {
                ensure!(length <= subtitle::LIMIT as u64, "subtitle too large");
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .context("Subtitle download interrupted")?
            {
                ensure!(
                    body.len() + chunk.len() <= subtitle::LIMIT,
                    "subtitle too large"
                );
                body.extend_from_slice(&chunk);
            }
            ensure!(!body.is_empty(), "Empty subtitle response");
            return Ok((body, cacheable, expiry));
        }
        bail!("Too many subtitle redirects")
    }
}
// No freshness on a redirect cannot establish that its destination is still
// correct. For final responses without explicit freshness, cap our local
// heuristic at one hour rather than refreshing it on every cache hit.
fn effective_subtitle_expiry(
    redirect: bool,
    expiry: Option<std::time::SystemTime>,
    headers: &reqwest::header::HeaderMap,
    sent_at: std::time::SystemTime,
    now: std::time::SystemTime,
) -> std::time::SystemTime {
    if let Some(expiry) = expiry {
        return expiry;
    }
    if redirect {
        return now;
    }
    // Apply the same Date/Age/response-delay calculation used for max-age,
    // with a bounded one-hour heuristic lifetime when no lifetime is supplied.
    let mut heuristic = headers.clone();
    heuristic.insert(
        reqwest::header::CACHE_CONTROL,
        reqwest::header::HeaderValue::from_static("max-age=3600"),
    );
    subtitle_freshness(&heuristic, sent_at, now)
        .1
        .unwrap_or(now)
}
/// Returns whether bytes may be stored and their absolute freshness deadline.
/// `None` means no explicit deadline; an unparseable freshness instruction is
/// immediately stale. `no-cache` can be stored for export, but never reused.
fn subtitle_freshness(
    headers: &reqwest::header::HeaderMap,
    sent_at: std::time::SystemTime,
    now: std::time::SystemTime,
) -> (bool, Option<std::time::SystemTime>) {
    use reqwest::header::{AGE, CACHE_CONTROL, DATE, EXPIRES};
    let mut may_store = true;
    let mut force_stale = false;
    let mut max_age: Option<u64> = None;
    for value in headers.get_all(CACHE_CONTROL).iter() {
        let Ok(value) = value.to_str() else {
            force_stale = true;
            continue;
        };
        for part in value.split(',') {
            let (name, value) = part.trim().split_once('=').unwrap_or((part.trim(), ""));
            if name.trim().eq_ignore_ascii_case("no-store") {
                may_store = false;
            }
            if name.trim().eq_ignore_ascii_case("no-cache") {
                force_stale = true;
            }
            if name.trim().eq_ignore_ascii_case("max-age") {
                match value.trim().trim_matches('"').parse::<u64>() {
                    Ok(seconds) => max_age = Some(max_age.map_or(seconds, |old| old.min(seconds))),
                    Err(_) => force_stale = true,
                }
            }
        }
    }
    let parse_single_date =
        |header: reqwest::header::HeaderName| -> Result<Option<std::time::SystemTime>, ()> {
            let all = headers.get_all(header);
            let mut values = all.iter();
            let Some(value) = values.next() else {
                return Ok(None);
            };
            if values.next().is_some() {
                return Err(());
            }
            Ok(Some(
                httpdate::parse_http_date(value.to_str().map_err(|_| ())?).map_err(|_| ())?,
            ))
        };
    let date = parse_single_date(DATE);
    let expires = parse_single_date(EXPIRES);
    let mut age_secs = 0u64;
    for value in headers.get_all(AGE).iter() {
        match value
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            Some(age) => age_secs = age_secs.max(age),
            None => force_stale = true,
        }
    }
    if date.is_err() || expires.is_err() {
        force_stale = true;
    }
    if force_stale {
        return (may_store, Some(now));
    }
    let date = date.ok().flatten().unwrap_or(now);
    let apparent_age = now.duration_since(date).unwrap_or_default().as_secs();
    let response_delay = now.duration_since(sent_at).unwrap_or_default().as_secs();
    let current_age = age_secs.saturating_add(response_delay).max(apparent_age);
    let lifetime = max_age.or_else(|| {
        expires
            .ok()
            .flatten()
            .map(|e| e.duration_since(date).unwrap_or_default().as_secs())
    });
    let deadline = lifetime.map(|seconds| {
        now.checked_add(Duration::from_secs(seconds.saturating_sub(current_age)))
            .unwrap_or(now)
    });
    (may_store, deadline)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    let s = v
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("Missing string field: {key}"))?;
    ensure!(
        !s.is_empty() && s.len() <= 16384,
        "Invalid string field: {key}"
    );
    Ok(s)
}
fn boolean(v: &Value, key: &str, default: bool) -> Result<bool> {
    v.get(key)
        .map(|v| {
            v.as_bool()
                .with_context(|| format!("{key} must be boolean"))
        })
        .unwrap_or(Ok(default))
}
fn extras(v: &Value) -> Result<Map<String, Value>> {
    match v.get("extra") {
        None => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m.clone()),
        _ => bail!("extra must be an object"),
    }
}
fn read_installed(db: &rusqlite::Connection) -> Result<Vec<Addon>> {
    let mut s =
        db.prepare("SELECT url,manifest,allow_http,generation FROM addons ORDER BY rowid")?;
    let items = s.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, bool>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut addons = Vec::new();
    for item in items {
        let (url, manifest, allow_http, generation) = item?;
        addons.push(Addon {
            url,
            manifest: std::sync::Arc::new(serde_json::from_str(&manifest)?),
            allow_http,
            generation,
        });
    }
    Ok(addons)
}

fn nonnegative(v: &Value, key: &str, default: Option<i64>) -> Result<i64> {
    let n = v
        .get(key)
        .and_then(Value::as_i64)
        .or_else(|| if v.get(key).is_none() { default } else { None })
        .with_context(|| format!("{key} must be a nonnegative integer"))?;
    ensure!(
        (0..=31_536_000_000i64).contains(&n),
        "{key} is out of range"
    );
    Ok(n)
}
fn limit(v: &Value) -> Result<usize> {
    Ok(match v.get("limit") {
        None => 24,
        Some(n) => n
            .as_u64()
            .context("limit must be a nonnegative integer")?
            .min(100) as usize,
    })
}
fn stale_or_error(cached: Option<(Value, i64)>, error: anyhow::Error) -> Result<Value> {
    if let Some((mut v, _)) = cached {
        v["cached"] = json!(true);
        v["stale"] = json!(true);
        v["error"] = json!(addon::redact(error.to_string()));
        Ok(v)
    } else {
        Err(anyhow!(addon::redact(error.to_string())))
    }
}
async fn collect(jobs: &mut JoinSet<(usize, Value)>) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    while let Some(v) = jobs.join_next().await {
        results.push(v.context("Addon worker failed")?);
    }
    results.sort_by_key(|(i, _)| *i);
    Ok(results.into_iter().map(|(_, v)| v).collect())
}

#[cfg(test)]
mod subtitle_freshness_tests {
    use super::{effective_subtitle_expiry, subtitle_freshness};
    use reqwest::header::{HeaderMap, HeaderValue, AGE, CACHE_CONTROL, DATE, EXPIRES};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn unspecified_redirect_is_immediately_stale_and_final_has_absolute_cap() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let headers = HeaderMap::new();
        assert_eq!(
            effective_subtitle_expiry(true, None, &headers, now, now),
            now
        );
        assert_eq!(
            effective_subtitle_expiry(false, None, &headers, now, now),
            now + Duration::from_secs(3600)
        );
        let explicit = now + Duration::from_secs(20);
        assert_eq!(
            effective_subtitle_expiry(true, Some(explicit), &headers, now, now),
            explicit
        );
    }

    #[test]
    fn heuristic_subtracts_age_date_and_request_delay() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut headers = HeaderMap::new();
        headers.insert(AGE, HeaderValue::from_static("7200"));
        assert_eq!(
            effective_subtitle_expiry(false, None, &headers, now, now),
            now
        );
        headers.insert(AGE, HeaderValue::from_static("20"));
        assert_eq!(
            effective_subtitle_expiry(false, None, &headers, now - Duration::from_secs(5), now),
            now + Duration::from_secs(3575)
        );
        headers.remove(AGE);
        headers.insert(
            DATE,
            HeaderValue::from_str(&httpdate::fmt_http_date(now - Duration::from_secs(120)))
                .unwrap(),
        );
        assert_eq!(
            effective_subtitle_expiry(false, None, &headers, now, now),
            now + Duration::from_secs(3480)
        );
    }

    #[test]
    fn max_age_respects_date_age_and_response_delay() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut headers = HeaderMap::new();
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("max-age=60"));
        headers.insert(
            DATE,
            HeaderValue::from_str(&httpdate::fmt_http_date(now - Duration::from_secs(10))).unwrap(),
        );
        headers.insert(AGE, HeaderValue::from_static("20"));
        let (store, expires) = subtitle_freshness(&headers, now - Duration::from_secs(5), now);
        assert!(store);
        assert_eq!(expires, Some(now + Duration::from_secs(35)));
    }

    #[test]
    fn expires_and_invalid_directives_do_not_exceed_server_freshness() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut headers = HeaderMap::new();
        headers.insert(
            DATE,
            HeaderValue::from_str(&httpdate::fmt_http_date(now - Duration::from_secs(10))).unwrap(),
        );
        headers.insert(
            EXPIRES,
            HeaderValue::from_str(&httpdate::fmt_http_date(now + Duration::from_secs(20))).unwrap(),
        );
        assert_eq!(
            subtitle_freshness(&headers, now, now).1,
            Some(now + Duration::from_secs(20))
        );
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("max-age=invalid"));
        assert_eq!(subtitle_freshness(&headers, now, now).1, Some(now));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        assert!(!subtitle_freshness(&headers, now, now).0);
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        assert_eq!(subtitle_freshness(&headers, now, now), (true, Some(now)));
    }
}
