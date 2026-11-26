//! Your Veylo account and the sync between your devices, through Firebase (Authentication and
//! Firestore, called over REST). Every sign-in happens in the app:
//! - email and password;
//! - Google: Android's own account sheet (VeyloAccount.kt) on Android; on a computer, the project's
//!   sign-in page in the browser (firebase/hosting/sign-in.html), which hands the token back;
//! - a computer linked from a signed-in phone or tablet: the computer shows a QR code, the phone
//!   scans it and hands the computer its session, sealed with the key in that code.
//!
//! What syncs: titles with history (saved, watched, rated, progress), each episode's record
//! (`sync_changes` / `sync_apply`, newest change winning), and your audio language,
//! preferences and add-ons (`users/{uid}/config/keys`).
//!
//! Zero-knowledge E2EE (v3): both watch-history items and `config/keys` are encrypted on-device
//! with AES-256-GCM using a randomly generated 256-bit Sync Key that never leaves your devices
//! in plaintext. QR device linking transfers the Sync Key inside its ephemeral encrypted handshake;
//! email/Google sign-ins on additional devices can enter the Recovery Key once in Settings.
//! The session (refresh token) and Sync Key are kept in the app's private folder (`0600` on Unix)
//! and are never logged. The Sync Key file says which account it belongs to: signing out keeps
//! it, so the same account signing in again finds its data; another account never uses it (the
//! old one is parked under its own name), and only deleting the account deletes its key.
use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::{AcqRel, Acquire}};

// ---------------------------------------------------------------- configuration

/// The Firebase project and Google sign-in clients a build is made with: release builds get them
/// from the release workflow's secrets, a local build from the environment. Without them, a build
/// has no Account.
pub struct Config {
    pub api_key: &'static str,
    pub project: &'static str,
    /// Google's "web" client: Android's account sheet and the computer's sign-in page ask for a
    /// token for it.
    pub web_client: Option<&'static str>,
}

pub fn config() -> Option<Config> {
    let set = |v: Option<&'static str>| v.filter(|v| !v.trim().is_empty());
    Some(Config {
        api_key: set(option_env!("VEYLO_FIREBASE_API_KEY"))?,
        project: set(option_env!("VEYLO_FIREBASE_PROJECT"))?,
        web_client: set(option_env!("VEYLO_GOOGLE_WEB_CLIENT")),
    })
}

/// Whether this build can sign in at all.
pub fn accounts_available() -> bool { config().is_some() }

/// Google sign-in this device offers: Android's account sheet, or the browser on a computer.
pub fn google_available() -> bool { config().is_some_and(|c| c.web_client.is_some()) }

// ---------------------------------------------------------------- session

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Account { pub uid: String, pub email: String, pub name: String, pub provider: String }

#[derive(Clone, Default)]
struct Session { account: Account, refresh: String, id_token: String, expires: i64 }

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static SESSION_IO: Mutex<()> = Mutex::new(());
static LOADED: OnceLock<()> = OnceLock::new();
static LAST_SYNCED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static SYNC_KEY_NEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STATE_LOCK: Mutex<()> = Mutex::new(());
static SYNC_KEY_LOCK: Mutex<()> = Mutex::new(());
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Bumped whenever the signed-in session changes (sign-in, sign-out, deletion): a sync that began
/// under an earlier one must not write its progress or take anything in afterwards.
static SESSION_GEN: AtomicU64 = AtomicU64::new(0);
static FORGOTTEN: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn session_path() -> PathBuf { backend().data_dir.join("account.json") }
const SYNC_KEY_FILE: &str = "sync-key.json";
const STATE_FILE: &str = "sync-state.json";

/// Encode a 32-byte E2EE sync key as a human-copyable recovery code (`VEYLO-...`).
pub fn format_recovery_code(key: &[u8; 32]) -> String {
    format!("VEYLO-{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key))
}

/// Parse a `VEYLO-...` (or raw base64url) 256-bit recovery key.
pub fn parse_recovery_code(raw: &str) -> Result<[u8; 32]> {
    let clean: String = raw.trim().chars().filter(|c| !c.is_whitespace()).collect();
    let body = clean.strip_prefix("VEYLO-").or_else(|| clean.strip_prefix("veylo-")).unwrap_or(&clean);
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body)
        .map_err(|_| anyhow!("That doesn't look like a valid Veylo recovery key."))?;
    let key: [u8; 32] = bytes.try_into()
        .map_err(|_| anyhow!("That recovery key is the wrong length."))?;
    Ok(key)
}

/// A Sync Key file as found on disk. A missing file and one that cannot be read are different
/// things: only a missing one may be replaced by a new key.
#[derive(Debug, PartialEq)]
enum KeyFile { Found { key: [u8; 32], uid: Option<String> }, Missing, Unreadable(String) }

fn read_key_file(path: &Path) -> KeyFile {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return KeyFile::Missing,
        Err(e) => return KeyFile::Unreadable(format!("{:?}", e.kind())),
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return KeyFile::Unreadable("not valid JSON".into()) };
    match v["key"].as_str().and_then(|k| parse_recovery_code(k).ok()) {
        Some(key) => KeyFile::Found { key, uid: v["uid"].as_str().filter(|u| !u.is_empty()).map(str::to_owned) },
        None => KeyFile::Unreadable("no valid key in it".into()),
    }
}

/// The key and the account it belongs to (files from before accounts were recorded have none:
/// they belong to whoever was signed in, see `ensure_sync_key_in`).
fn write_key_file(path: &Path, key: &[u8; 32], uid: &str) -> Result<()> {
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
    let data = json!({"v": 3, "key": encoded, "uid": uid}).to_string();
    write_private_file(path, data.as_bytes()).context("Couldn't save the encryption key")
}

fn uid_tag(uid: &str) -> String { format!("{:x}", Sha256::digest(format!("veylo-sync-key-owner\0{uid}").as_bytes()))[..16].to_owned() }
/// Where the key of an account that is not the active one waits.
fn parked_key_path(dir: &Path, uid: &str) -> PathBuf { dir.join(format!("sync-key.{}.json", uid_tag(uid))) }
/// The key an account had before a Recovery Key replaced it.
fn previous_key_path(dir: &Path) -> PathBuf { dir.join("sync-key.prev.json") }

/// This account's Sync Key in `dir`. The active file's key if the file is this account's (or
/// from before files recorded their account); else one parked for the account earlier; else a
/// new one, which an account with encrypted cloud data asks to replace with its Recovery Key.
/// A file that cannot be read is an error, never replaced: it may hold the only copy of the key.
fn ensure_sync_key_in(dir: &Path, uid: &str) -> Result<[u8; 32]> {
    let active = dir.join(SYNC_KEY_FILE);
    match read_key_file(&active) {
        KeyFile::Unreadable(why) => anyhow::bail!("Couldn't read this device's encryption key ({why}). It was left untouched."),
        KeyFile::Found { key, uid: owner } => {
            match owner {
                None => { let _ = write_key_file(&active, &key, uid); return Ok(key); }
                Some(o) if o == uid => return Ok(key),
                // Another account's key is parked under its own name, never overwritten.
                Some(o) => write_key_file(&parked_key_path(dir, &o), &key, &o)?,
            }
        }
        KeyFile::Missing => {}
    }
    let parked = parked_key_path(dir, uid);
    match read_key_file(&parked) {
        KeyFile::Found { key, .. } => { write_key_file(&active, &key, uid)?; let _ = std::fs::remove_file(&parked); return Ok(key); }
        KeyFile::Unreadable(why) => anyhow::bail!("Couldn't read this account's saved encryption key ({why}). It was left untouched."),
        KeyFile::Missing => {}
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|_| anyhow!("Couldn't generate an encryption key"))?;
    write_key_file(&active, &key, uid)?;
    Ok(key)
}

/// Make `key` this account's Sync Key (a Recovery Key from another device, or the one a linked
/// device handed over). The key it replaces is backed up first, so a wrong entry loses nothing.
fn replace_sync_key_in(dir: &Path, key: &[u8; 32], uid: &str) -> Result<()> {
    let active = dir.join(SYNC_KEY_FILE);
    match read_key_file(&active) {
        KeyFile::Found { key: old, uid: owner } => {
            let owner = owner.unwrap_or_else(|| uid.to_owned());
            if old == *key && owner == uid { return write_key_file(&active, key, uid); }
            let backup = if owner == uid { previous_key_path(dir) } else { parked_key_path(dir, &owner) };
            write_key_file(&backup, &old, &owner).context("Couldn't back up the current encryption key first")?;
        }
        KeyFile::Unreadable(_) => {
            // Keep whatever is there too, as bytes, before it is replaced.
            if let Ok(raw) = std::fs::read(&active) { let _ = write_private_file(&dir.join("sync-key.unreadable.json"), &raw); }
        }
        KeyFile::Missing => {}
    }
    write_key_file(&active, key, uid)
}

/// Delete everything kept of an account's Sync Key (only when the account itself is deleted).
fn remove_sync_key_in(dir: &Path, uid: &str) {
    if let KeyFile::Found { uid: owner, .. } = read_key_file(&dir.join(SYNC_KEY_FILE)) {
        if owner.as_deref().is_none_or(|o| o == uid) { let _ = std::fs::remove_file(dir.join(SYNC_KEY_FILE)); }
    }
    let _ = std::fs::remove_file(parked_key_path(dir, uid));
    if let KeyFile::Found { uid: owner, .. } = read_key_file(&previous_key_path(dir)) {
        if owner.as_deref().is_none_or(|o| o == uid) { let _ = std::fs::remove_file(previous_key_path(dir)); }
    }
}

fn ensure_sync_key(uid: &str) -> Result<[u8; 32]> {
    let _guard = SYNC_KEY_LOCK.lock();
    ensure_sync_key_in(&backend().data_dir, uid)
}

/// The current device's E2EE Recovery Key (generated on first use when signed in). None while
/// the account's encrypted data needs another key: this device's own would be the wrong one to show.
pub fn sync_recovery_code() -> Option<String> {
    let uid = account()?.uid;
    if sync_key_needed() { return None; }
    ensure_sync_key(&uid).ok().map(|k| format_recovery_code(&k))
}

/// True when the account has v3 encrypted cloud data that this device's current key couldn't open.
pub fn sync_key_needed() -> bool {
    SYNC_KEY_NEEDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Replace this device's local Sync Key with a Recovery Key from another device and trigger sync.
/// Verifies the candidate key against existing v3 cloud data BEFORE replacing `sync-key.json`, and
/// backs the old key up.
pub async fn set_sync_recovery_code(code: &str) -> Result<(usize, usize)> {
    let key = parse_recovery_code(code)?;
    let _one = SYNCING.lock().await;
    let gen = SESSION_GEN.load(Acquire);
    let (token, uid) = id_token().await?;
    let base = documents().context("Accounts aren't set up in this build")?;
    let project = config().map(|c| c.project).unwrap_or_default();
    if let Some(doc) = get_doc(&token, format!("{base}/users/{uid}/config/keys")).await? {
        if matches!(open_keys_payload(&key, &uid, project, &doc["fields"], true), KeysOpen::WrongKey) {
            anyhow::bail!("That Recovery Key doesn't match this account's encrypted data.");
        }
    }
    let sample = list_items(&token, &uid, None, None, 1).await?;
    if sample.iter().any(|(_, fields)| open_item_fields(fields, &key) == ItemOpen::WrongKey) {
        anyhow::bail!("That Recovery Key doesn't match this account's encrypted data.");
    }
    anyhow::ensure!(SESSION_GEN.load(Acquire) == gen, "You were signed out.");
    {
        let _k = SYNC_KEY_LOCK.lock();
        replace_sync_key_in(&backend().data_dir, &key, &uid)?;
    }
    SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
    // Reset sync cursors atomically while holding SYNCING so encrypted cloud items and keys are re-read with the new key.
    let mut st = state_for(&uid);
    st.taken_until = None;
    st.taken_doc = None;
    st.keys_hash.clear();
    st.keys_updated = 0;
    st.key_verified = false;
    anyhow::ensure!(save_state_checked(&state_path(), gen, &st), "You were signed out.");
    sync_now_locked().await
}

fn load_session() {
    LOADED.get_or_init(|| {
        let v: Value = std::fs::read_to_string(session_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let refresh = s(&v, "refresh");
        if !refresh.is_empty() && config().is_some() {
            let account = Account { uid: s(&v, "uid"), email: s(&v, "email"), name: s(&v, "name"), provider: s(&v, "provider") };
            let uid = account.uid.clone();
            if let Ok(mut slot) = SESSION.lock() { *slot = Some(Session { account, refresh, ..Session::default() }); }
            let st = load_state();
            if st.uid.is_empty() || st.uid == uid { LAST_SYNCED.store(st.last, std::sync::atomic::Ordering::Relaxed); }
        }
    });
}

fn save_session(session: Option<&Session>) {
    let path = session_path();
    match session {
        Some(x) => {
            let a = &x.account;
            let data = json!({"uid":a.uid,"email":a.email,"name":a.name,"provider":a.provider,"refresh":x.refresh}).to_string();
            let _ = write_private_file(&path, data.as_bytes());
        }
        // Signing out forgets the session and the sync progress, never the Sync Key: the same
        // account signing in again needs it to open its data. Only deleting the account removes it.
        None => clear_session_files(path.parent().unwrap_or(Path::new("."))),
    }
}

fn clear_session_files(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("account.json"));
    let _ = std::fs::remove_file(dir.join(STATE_FILE));
}

/// The signed-in account, if any.
pub fn account() -> Option<Account> {
    load_session();
    SESSION.lock().ok()?.as_ref().map(|x| x.account.clone())
}

fn set_session(session: Session) -> Account {
    let account = session.account.clone();
    {
        let _io = SESSION_IO.lock();
        // Anything still running for the earlier session must not touch this one's files.
        SESSION_GEN.fetch_add(1, AcqRel);
        LAST_SYNCED.store(0, std::sync::atomic::Ordering::Relaxed);
        save_session(Some(&session));
        if let Ok(mut slot) = SESSION.lock() { *slot = Some(session); }
    }
    start_sync();
    sync_soon();
    // Pull synced library, API keys and add-ons right away on sign-in.
    if let Some(runtime) = RUNTIME.get() {
        runtime.spawn(async {
            if let Err(e) = sync_now().await { dev_log(&format!("sync: {e}")); }
        });
    }
    account
}

// ---------------------------------------------------------------- Firebase Authentication

/// What Firebase's errors mean, in the app's words.
fn auth_error(code: &str) -> String {
    let code = code.split([' ', ':']).next().unwrap_or(code);
    match code {
        "EMAIL_EXISTS" => "An account with this email already exists. Sign in instead.",
        "INVALID_LOGIN_CREDENTIALS" | "INVALID_PASSWORD" | "EMAIL_NOT_FOUND" => "Wrong email or password.",
        "INVALID_EMAIL" => "That email address doesn't look right.",
        "WEAK_PASSWORD" => "Choose a password of at least 6 characters.",
        "MISSING_PASSWORD" => "Type a password.",
        "USER_DISABLED" => "This account has been disabled.",
        "TOO_MANY_ATTEMPTS_TRY_LATER" => "Too many attempts. Try again in a few minutes.",
        "TOKEN_EXPIRED" | "USER_NOT_FOUND" | "INVALID_REFRESH_TOKEN" | "INVALID_ID_TOKEN" => "You were signed out. Sign in again.",
        "CREDENTIAL_TOO_OLD_LOGIN_AGAIN" => "For security, sign in again first, then retry.",
        "OPERATION_NOT_ALLOWED" | "CONFIGURATION_NOT_FOUND" => "This sign-in method isn't turned on for Veylo's accounts yet.",
        _ => "Couldn't reach Veylo's accounts. Check your connection and try again.",
    }.to_owned()
}

/// One POST: its status and answer. Only a failure to reach the service is an error here.
async fn post_raw(url: String, body: Value, token: Option<String>) -> Result<(u16, Value)> {
    on_runtime(async move {
        let mut req = backend().http.post(url).json(&body).timeout(Duration::from_secs(20));
        if let Some(t) = token { req = req.bearer_auth(t); }
        let response = req.send().await.map_err(|_| anyhow!(auth_error("")))?;
        let status = response.status().as_u16();
        Ok((status, response.json().await.unwrap_or_default()))
    }).await?
}

fn answer(status: u16, v: Value) -> Result<Value> {
    if (200..300).contains(&status) { Ok(v) } else { Err(anyhow!("{}", v["error"]["message"].as_str().or(v[0]["error"]["message"].as_str()).unwrap_or("").to_owned())) }
}

async fn post(url: String, body: Value, token: Option<String>) -> Result<Value> {
    let (status, v) = post_raw(url, body, token).await?;
    answer(status, v)
}

/// After a 401 the cached ID token is dropped (revoked, or past its time by the server's clock)
/// and a fresh one asked for. None if the token is not the session's, or none could be had.
async fn token_after_401(refused: &str) -> Option<String> {
    {
        let mut slot = SESSION.lock().ok()?;
        let session = slot.as_mut()?;
        if session.id_token == refused { session.expires = 0; }
    }
    id_token().await.ok().map(|(t, _)| t).filter(|t| t != refused)
}

/// A POST with the session's token: refused with 401, it is made once more with a fresh token.
async fn post_session_raw(url: String, body: Value, token: &str) -> Result<(u16, Value)> {
    let (status, v) = post_raw(url.clone(), body.clone(), Some(token.to_owned())).await?;
    if status == 401 {
        if let Some(fresh) = token_after_401(token).await {
            return post_raw(url, body, Some(fresh)).await;
        }
    }
    Ok((status, v))
}

async fn post_session(url: String, body: Value, token: &str) -> Result<Value> {
    let (status, v) = post_session_raw(url, body, token).await?;
    answer(status, v)
}

async fn identity(method: &str, body: Value) -> Result<Value> {
    let c = config().context("Accounts aren't set up in this build")?;
    post(format!("https://identitytoolkit.googleapis.com/v1/accounts:{method}?key={}", c.api_key), body, None).await
        .map_err(|e| anyhow!(auth_error(&e.to_string())))
}

/// A session from an Authentication answer (sign-up, sign-in or Google).
fn session_from(v: &Value, provider: &str) -> Result<Session> {
    let refresh = s(v, "refreshToken");
    anyhow::ensure!(!refresh.is_empty(), auth_error(""));
    Ok(Session {
        account: Account { uid: s(v, "localId"), email: s(v, "email"), name: s(v, "displayName"), provider: provider.into() },
        refresh, id_token: s(v, "idToken"), expires: now_secs() + v["expiresIn"].as_str().and_then(|x| x.parse().ok()).unwrap_or(3600) - 60,
    })
}

/// Create an account with email and password, signed in.
pub async fn sign_up(email: String, password: String) -> Result<Account> {
    let v = identity("signUp", json!({"email":email.trim(),"password":password,"returnSecureToken":true})).await?;
    Ok(set_session(session_from(&v, "password")?))
}

/// Sign in with email and password.
pub async fn sign_in(email: String, password: String) -> Result<Account> {
    let v = identity("signInWithPassword", json!({"email":email.trim(),"password":password,"returnSecureToken":true})).await?;
    Ok(set_session(session_from(&v, "password")?))
}

/// Email a link to choose a new password.
pub async fn reset_password(email: String) -> Result<()> {
    identity("sendOobCode", json!({"requestType":"PASSWORD_RESET","email":email.trim()})).await.map(|_| ())
}

/// Sign in with a Google ID token (from Android's account sheet or the computer's browser).
async fn sign_in_google_token(id_token: String) -> Result<Account> {
    let post_body: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("id_token", &id_token)
        .append_pair("providerId", "google.com")
        .finish();
    let v = identity("signInWithIdp", json!({"postBody":post_body,
        "requestUri":"http://localhost","returnIdpCredential":true,"returnSecureToken":true})).await?;
    Ok(set_session(session_from(&v, "google")?))
}

/// Sign in with Google: Android's account sheet, or the browser on a computer.
pub async fn sign_in_google() -> Result<Account> {
    let token = google_id_token().await?;
    sign_in_google_token(token).await
}

#[cfg(target_os = "android")]
async fn google_id_token() -> Result<String> {
    let client = config().and_then(|c| c.web_client).context("Google sign-in isn't set up in this build")?;
    crate::android::google_id_token(client)?;
    for _ in 0..1200 {
        sleep(250).await;
        match crate::android::account_result() {
            Some(r) if r.starts_with("token:") => return Ok(r["token:".len()..].to_owned()),
            Some(r) if r.starts_with("cancel:") => anyhow::bail!("Sign-in cancelled."),
            Some(r) if r.starts_with("error:") => anyhow::bail!("Google sign-in didn't work: {}", &r["error:".len()..]),
            _ => {}
        }
    }
    anyhow::bail!("Sign-in took too long. Try again.")
}

/// Parse `state` and `id_token` from either a POST form body (preferred: keeps tokens out of
/// browser history) or a GET query string (fallback for older sign-in pages).
#[cfg_attr(target_os = "android", allow(dead_code))]
fn parse_callback_params(request: &str) -> HashMap<String, String> {
    let first_line = request.lines().next().unwrap_or_default();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or("/");
    if method.eq_ignore_ascii_case("POST") {
        let body = request.split_once("\r\n\r\n").or_else(|| request.split_once("\n\n")).map(|(_, b)| b).unwrap_or_default();
        return url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    }
    url::Url::parse(&format!("http://127.0.0.1{path}")).ok()
        .map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default()
}

/// On a computer: the project's sign-in page (Firebase Hosting, firebase/hosting/sign-in.html) in
/// the browser, where Google's button gives an ID token for the web client; the page posts it to
/// a one-off local address (127.0.0.1) with the check value Veylo gave it.
#[cfg(not(target_os = "android"))]
async fn google_id_token() -> Result<String> {
    let c = config().context("Google sign-in isn't set up in this build")?;
    c.web_client.context("Google sign-in isn't set up in this build")?;
    let project = c.project;
    on_runtime(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let state = random_text(24);
        crate::open_external(&format!("https://{project}.firebaseapp.com/sign-in.html?port={port}&state={state}"));
        tokio::time::timeout(Duration::from_secs(300), async {
            loop {
                let (mut stream, _) = listener.accept().await?;
                let mut buf = Vec::with_capacity(4096);
                let mut chunk = [0u8; 4096];
                let mut header_end = None;
                let mut content_len = 0usize;
                while buf.len() < 65536 {
                    let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await else { break };
                    if n == 0 { break; }
                    buf.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let head = String::from_utf8_lossy(&buf[..pos]);
                            content_len = head.lines().find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok()).flatten()
                            }).unwrap_or(0).min(65536);
                        }
                    }
                    if let Some(end) = header_end {
                        if buf.len() >= end + content_len { break; }
                    }
                }
                let request = String::from_utf8_lossy(&buf);
                let q = parse_callback_params(&request);
                if q.get("state") != Some(&state) {
                    let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                }
                let token = q.get("id_token").cloned().filter(|t| !t.is_empty());
                let page = if token.is_some() { "You're signed in. You can close this tab and go back to Veylo." } else { "Sign-in was cancelled. You can close this tab." };
                let body = format!("<!doctype html><meta charset=utf-8><title>Veylo</title><body style=\"font:18px system-ui;background:#0e0e10;color:#f2efea;display:grid;place-items:center;height:90vh\">{page}</body>");
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                return token.context("Sign-in cancelled.");
            }
        }).await.map_err(|_| anyhow!("Sign-in took too long. Try again."))?
    }).await?
}

fn random_text(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    let _ = getrandom::fill(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// A fresh ID token for the session (refreshed when it is about to expire).
async fn id_token() -> Result<(String, String)> {
    load_session();
    let current = SESSION.lock().ok().and_then(|x| x.clone()).context("Not signed in")?;
    if !current.id_token.is_empty() && current.expires > now_secs() { return Ok((current.id_token, current.account.uid)); }
    let _refreshing = REFRESH_LOCK.lock().await;
    let current = SESSION.lock().ok().and_then(|x| x.clone()).context("Not signed in")?;
    if !current.id_token.is_empty() && current.expires > now_secs() { return Ok((current.id_token, current.account.uid)); }
    let c = config().context("Accounts aren't set up in this build")?;
    let refresh = current.refresh.clone();
    let expected_uid = current.account.uid.clone();
    let api = c.api_key;
    let v: Result<Value> = on_runtime(async move {
        let response = backend().http.post(format!("https://securetoken.googleapis.com/v1/token?key={api}"))
            .form(&[("grant_type", "refresh_token"), ("refresh_token", refresh.as_str())]).timeout(Duration::from_secs(20)).send().await
            .map_err(|_| anyhow!("Could not reach the account service"))?;
        let ok = response.status().is_success();
        let v: Value = response.json().await.unwrap_or_default();
        if ok { Ok(v) } else { Err(anyhow!("{}", v["error"]["message"].as_str().unwrap_or(""))) }
    }).await?;
    let v = match v {
        Ok(v) => v,
        Err(e) => {
            // Refused for good (the account was deleted, or signed out everywhere): signed out here,
            // only if the session hasn't already changed to another account.
            let code = e.to_string();
            if ["TOKEN_EXPIRED", "USER_NOT_FOUND", "INVALID_REFRESH_TOKEN", "USER_DISABLED"].iter().any(|c| code.starts_with(c)) {
                let _io = SESSION_IO.lock();
                let same_session = SESSION.lock().ok().and_then(|s| s.as_ref().map(|x| x.account.uid == expected_uid && x.refresh == current.refresh)).unwrap_or(false);
                if same_session { forget_session_locked(); }
            }
            anyhow::bail!(auth_error(&code));
        }
    };
    let mut next = current.clone();
    next.id_token = s(&v, "id_token");
    next.refresh = Some(s(&v, "refresh_token")).filter(|r| !r.is_empty()).unwrap_or(next.refresh);
    next.expires = now_secs() + v["expires_in"].as_str().and_then(|x| x.parse().ok()).unwrap_or(3600) - 60;
    let out = (next.id_token.clone(), next.account.uid.clone());
    // Never resurrect a session that was signed out or replaced while the refresh request was in flight.
    // Hold SESSION_IO across both the memory update and save_session so sign-out cannot race the disk write.
    let updated = {
        let _io = SESSION_IO.lock();
        let still_same = if let Ok(mut slot) = SESSION.lock() {
            let matches = slot.as_ref().is_some_and(|s| s.account.uid == expected_uid && s.refresh == current.refresh);
            if matches { *slot = Some(next.clone()); true } else { false }
        } else { false };
        if still_same { save_session(Some(&next)); }
        still_same
    };
    anyhow::ensure!(updated, "Not signed in");
    Ok(out)
}

fn forget_session_locked() {
    // First, so a sync in flight sees it before anything else changes: its progress and what it
    // took are not for the next session.
    SESSION_GEN.fetch_add(1, AcqRel);
    if let Ok(mut slot) = SESSION.lock() { *slot = None; }
    if let Ok(mut f) = FORGOTTEN.lock() { f.clear(); }
    LAST_SYNCED.store(0, std::sync::atomic::Ordering::Relaxed);
    SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
    save_session(None);
}

fn forget_session() {
    let _io = SESSION_IO.lock();
    forget_session_locked();
}

/// Sign out on this device (the account and the other devices stay).
pub fn sign_out() { forget_session(); }

/// Delete the account and everything synced to it, then sign out. This device's library stays.
/// Waits for a sync in progress first, so nothing is written to the account while it is deleted;
/// this is also the only place the Sync Key is deleted.
pub async fn delete_account() -> Result<()> {
    let _one = SYNCING.lock().await;
    let (token, uid) = id_token().await?;
    // The synced titles first: a deleted account can't reach them any more.
    loop {
        let docs = list_items(&token, &uid, None, None, 300).await?;
        if docs.is_empty() { break; }
        let writes: Vec<Value> = docs.iter().map(|(name, _)| json!({"delete": name})).collect();
        commit(&token, writes).await?;
    }
    if let Some(base) = documents() {
        // Synced keys & add-ons config document (Firestore delete is idempotent if absent).
        commit(&token, vec![json!({"delete": format!("{base}/users/{uid}/config/keys")})]).await?;
        // Diagnostic reports are the account's too, but clearing them is best effort: whatever
        // is refused or unreachable here must never stop the account itself from being deleted.
        // (The rules know no other collections; the old key hand-off requests are gone.)
        'reports: loop {
            let Ok(Some(list)) = get_doc(&token, format!("{base}/users/{uid}/diagnostics?pageSize=300")).await else { break };
            let writes: Vec<Value> = list["documents"].as_array().into_iter().flatten().filter_map(|d| d["name"].as_str()).map(|n| json!({"delete": n})).collect();
            if writes.is_empty() || commit(&token, writes).await.is_err() { break 'reports; }
        }
    }
    // A fresh token: the clean-up above may have taken a while.
    let (token, _) = id_token().await?;
    identity("delete", json!({"idToken":token})).await?;
    forget_session();
    {
        let _k = SYNC_KEY_LOCK.lock();
        remove_sync_key_in(&backend().data_dir, &uid);
    }
    Ok(())
}

// ---------------------------------------------------------------- Firestore

fn documents() -> Option<String> {
    config().map(|c| format!("projects/{}/databases/(default)/documents", c.project))
}

pub(super) fn documents_pub() -> Option<String> { documents() }
pub(super) async fn id_token_pub() -> Result<(String, String)> { id_token().await }
pub(super) async fn commit_pub(token: &str, writes: Vec<Value>) -> Result<()> { commit(token, writes).await }
pub(super) async fn get_doc_pub(token: &str, path: String) -> Result<Option<Value>> { get_doc(token, path).await }
pub(super) fn random_id_pub(bytes: usize) -> String { random_text(bytes).replace(['-', '_'], "x") }
pub(super) async fn anon_sign_up_pub() -> Result<Value> { identity("signUp", json!({"returnSecureToken": true})).await }
pub(super) async fn anon_delete_pub(token: &str) -> Result<Value> { identity("delete", json!({"idToken": token})).await }

/// Firestore refuses a request past 10 MiB (and past 500 writes): batches stay far below both.
const COMMIT_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Strictly below list_items' 300-doc page size, so a batch sharing one server REQUEST_TIME
/// never spans across a GREATER_THAN pagination boundary.
const COMMIT_MAX_WRITES: usize = 200;
/// The rules refuse a sealed field past 256 KiB, and one refused write fails its whole commit
/// (the same rows would then be retried for ever): a row sealing to more than this is left out.
const MAX_SEALED_BYTES: usize = 240 * 1024;

/// Split writes into batches of at most `COMMIT_MAX_WRITES` writes and about `COMMIT_MAX_BYTES`
/// bytes, in order. One write bigger than that goes alone.
fn commit_batches(writes: Vec<Value>) -> Vec<Vec<Value>> {
    let mut batches: Vec<Vec<Value>> = Vec::new();
    let (mut current, mut bytes): (Vec<Value>, usize) = (Vec::new(), 0);
    for w in writes {
        let size = w.to_string().len();
        if !current.is_empty() && (current.len() >= COMMIT_MAX_WRITES || bytes + size > COMMIT_MAX_BYTES) {
            batches.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += size;
        current.push(w);
    }
    if !current.is_empty() { batches.push(current); }
    batches
}

async fn commit(token: &str, writes: Vec<Value>) -> Result<()> {
    let base = documents().context("Accounts aren't set up in this build")?;
    for chunk in commit_batches(writes) {
        post_session(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":chunk}), token).await?;
    }
    Ok(())
}

/// Build the Firestore structuredQuery for paginated item listing using a composite
/// `(synced, __name__)` cursor so equal server timestamps across a page boundary are never skipped.
fn items_query(since: Option<&str>, after_doc: Option<&str>, limit: usize) -> Value {
    let mut query = json!({
        "from": [{"collectionId": "items"}],
        "orderBy": [
            {"field": {"fieldPath": "synced"}},
            {"field": {"fieldPath": "__name__"}},
        ],
        "limit": limit,
    });
    match (since, after_doc) {
        (Some(t), Some(doc)) => {
            query["startAt"] = json!({
                "values": [{"timestampValue": t}, {"referenceValue": doc}],
                "before": false,
            });
        }
        (Some(t), None) => {
            query["where"] = json!({"fieldFilter":{"field":{"fieldPath":"synced"},"op":"GREATER_THAN","value":{"timestampValue":t}}});
        }
        _ => {}
    }
    query
}

/// The account's synced documents changed after `(since, after_doc)` (the server's time), oldest
/// first: (document name, its fields).
async fn list_items(token: &str, uid: &str, since: Option<&str>, after_doc: Option<&str>, limit: usize) -> Result<Vec<(String, Value)>> {
    let base = documents().context("Accounts aren't set up in this build")?;
    let query = items_query(since, after_doc, limit);
    let v = post_session(format!("https://firestore.googleapis.com/v1/{base}/users/{uid}:runQuery"), json!({"structuredQuery":query}), token).await?;
    Ok(v.as_array().into_iter().flatten().filter_map(|r| {
        let doc = &r["document"];
        Some((doc["name"].as_str()?.to_owned(), doc["fields"].clone()))
    }).collect())
}

/// An already sealed row as a document write, plus the server's timestamp.
fn sealed_write(base: &str, uid: &str, id: &str, sealed: String) -> Value {
    json!({
        "update": {
            "name": format!("{base}/users/{uid}/items/{id}"),
            "fields": {
                "sealed": {"stringValue": sealed},
                "v": {"integerValue": "3"},
            }
        },
        "updateTransforms": [{"fieldPath": "synced", "setToServerValue": "REQUEST_TIME"}]
    })
}

/// A synced row as an end-to-end encrypted document write (AES-256-GCM with the user's 256-bit
/// Sync Key), plus the server's timestamp. A row too big for the rules to accept is an error.
#[cfg(test)]
fn item_write(base: &str, uid: &str, id: &str, data: &Value, sync_key: &[u8; 32]) -> Result<Value> {
    let sealed = seal(sync_key, data.to_string().as_bytes())?;
    anyhow::ensure!(sealed.len() <= MAX_SEALED_BYTES, "A synced row is too large");
    Ok(sealed_write(base, uid, id, sealed))
}

/// The write for one row, or None when even without its bulkiest parts (synopsis, episode
/// list) it seals to more than the rules accept: such a row is left out, so that it cannot make
/// every commit that carries it fail.
fn fitting_item_write(base: &str, uid: &str, id: &str, data: &Value, sync_key: &[u8; 32]) -> Result<Option<Value>> {
    let mut data = data.clone();
    for attempt in 0..2 {
        let sealed = seal(sync_key, data.to_string().as_bytes())?;
        if sealed.len() <= MAX_SEALED_BYTES { return Ok(Some(sealed_write(base, uid, id, sealed))); }
        match data.get_mut("meta").and_then(Value::as_object_mut) {
            Some(meta) if attempt == 0 => { meta.remove("videos"); meta.remove("description"); }
            _ => break,
        }
    }
    Ok(None)
}

#[derive(Debug, PartialEq)]
enum ItemOpen { Ok(Value), /** A plaintext document from before v3 encryption. */ Legacy(Value), WrongKey, Empty }

fn open_item_fields(fields: &Value, sync_key: &[u8; 32]) -> ItemOpen { open_item_fields_with(fields, sync_key, true) }

/// `allow_legacy`: still accept documents from before v3 (plaintext, or marked with an older
/// version). Once this device has verified its key against v3 data they are refused, so that
/// plaintext cannot be slipped into an account that has moved to end-to-end encryption.
fn open_item_fields_with(fields: &Value, sync_key: &[u8; 32], allow_legacy: bool) -> ItemOpen {
    if let Some(sealed) = fields["sealed"]["stringValue"].as_str() {
        let version = fields["v"]["integerValue"].as_str().and_then(|v| v.parse::<i64>().ok()).unwrap_or(3);
        if version < 3 && !allow_legacy { return ItemOpen::Empty; }
        return match open_sealed(sync_key, sealed).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            Some(v) => ItemOpen::Ok(v),
            None => ItemOpen::WrongKey,
        };
    }
    if !allow_legacy { return ItemOpen::Empty; }
    // Seamless migration fallback for legacy plaintext item docs.
    match fields["data"]["stringValue"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        Some(v) => ItemOpen::Legacy(v),
        None => ItemOpen::Empty,
    }
}

fn doc_id(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes()))[..40].to_owned() }

// ---------------------------------------------------------------- sync

/// Where sync is up to: the last local change sent (seconds, this device's clock), the last
/// server change taken, the (key, time) of rows exchanged lately so none is sent back, and the
/// hash/timestamp of the last keys & add-ons snapshot synced with the account. It belongs to one
/// account (`uid`): progress made for another is never used.
#[derive(Default)]
struct State {
    /// The account this progress is for (empty in files from before it was recorded).
    uid: String,
    sent_until: i64,
    media_sent_until: i64,
    playback_sent_until: i64,
    taken_until: Option<String>,
    taken_doc: Option<String>,
    seen: BTreeMap<String, i64>,
    known: std::collections::BTreeSet<String>,
    forgotten: Vec<Value>,
    last: i64,
    keys_hash: String,
    keys_updated: i64,
    key_verified: bool,
}

impl State {
    /// This progress as account `uid`'s: a file from before accounts were recorded is taken as
    /// the signed-in account's own; progress made for another account starts over.
    fn bound_to(mut self, uid: &str) -> State {
        if !self.uid.is_empty() && self.uid != uid { return State { uid: uid.to_owned(), ..State::default() }; }
        self.uid = uid.to_owned();
        self
    }
}

fn state_path() -> PathBuf { backend().data_dir.join(STATE_FILE) }

/// Sync progress from its saved form. A device clock that was ahead once must never freeze
/// sending for good, so the cursors are never later than now.
fn parse_state(v: &Value, now: i64) -> State {
    let last = v["last"].as_i64().unwrap_or(0);
    let keys_hash = s(v, "keys_hash");
    let sent_until = v["sent_until"].as_i64().unwrap_or(0).min(now);
    // Legacy state files stored one shared `sent_until` cursor; if upgrading from that format,
    // rescan both tables from 0 once (deduplicated against `seen`) so rows skipped by a newer
    // playback timestamp are recovered.
    let split_cursors = v.get("media_sent_until").is_some() && v.get("playback_sent_until").is_some();
    let media_sent_until = if split_cursors { v["media_sent_until"].as_i64().unwrap_or(0).min(now) } else { 0 };
    let playback_sent_until = if split_cursors { v["playback_sent_until"].as_i64().unwrap_or(0).min(now) } else { 0 };
    State {
        uid: s(v, "uid"),
        sent_until,
        media_sent_until,
        playback_sent_until,
        taken_until: v["taken_until"].as_str().map(str::to_owned),
        taken_doc: v["taken_doc"].as_str().map(str::to_owned),
        seen: v["seen"].as_object().map(|o| o.iter().filter_map(|(k, t)| Some((k.clone(), t.as_i64()?))).collect()).unwrap_or_default(),
        known: v["known"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        forgotten: v["forgotten"].as_array().cloned().unwrap_or_default(),
        last,
        key_verified: v["key_verified"].as_bool().unwrap_or(!keys_hash.is_empty()),
        keys_hash,
        keys_updated: v["keys_updated"].as_i64().unwrap_or(0),
    }
}

fn load_state_unlocked() -> State {
    let v: Value = std::fs::read_to_string(state_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    parse_state(&v, now_secs())
}

fn load_state() -> State {
    let _guard = STATE_LOCK.lock();
    load_state_unlocked()
}

/// The saved progress as account `uid`'s.
fn state_for(uid: &str) -> State { load_state().bound_to(uid) }

fn state_json(st: &State, forgotten: &[Value]) -> Value {
    // Only the newest exchanged rows are worth remembering.
    let mut seen: Vec<(&String, &i64)> = st.seen.iter().collect();
    seen.sort_by_key(|(_, t)| std::cmp::Reverse(**t));
    let seen: serde_json::Map<String, Value> = seen.into_iter().take(5000).map(|(k, t)| (k.clone(), json!(t))).collect();
    json!({
        "uid": st.uid,
        "sent_until": st.media_sent_until.min(st.playback_sent_until),
        "media_sent_until": st.media_sent_until,
        "playback_sent_until": st.playback_sent_until,
        "taken_until": st.taken_until,
        "taken_doc": st.taken_doc,
        "seen": seen,
        "known": st.known,
        "forgotten": forgotten,
        "last": st.last,
        "keys_hash": st.keys_hash,
        "keys_updated": st.keys_updated,
        "key_verified": st.key_verified,
    })
}

fn write_state_to(path: &Path, st: &State) {
    // Merge any tombstones queued while a sync was in flight so they are never clobbered.
    let mut forgotten = st.forgotten.clone();
    if let Ok(pending) = FORGOTTEN.lock() {
        for item in pending.iter() {
            if !forgotten.contains(item) { forgotten.push(item.clone()); }
        }
    }
    LAST_SYNCED.store(st.last, std::sync::atomic::Ordering::Relaxed);
    let _ = write_private_file(path, state_json(st, &forgotten).to_string().as_bytes());
}

fn save_state_unlocked(st: &State) { write_state_to(&state_path(), st); }

/// Save sync progress, unless the session changed since `gen` (sign-out, another sign-in,
/// deletion): a sync still running after that must not bring the old account's progress back.
/// The check and the write happen under the session lock that sign-out also takes.
fn save_state_checked(path: &Path, gen: u64, st: &State) -> bool {
    let _io = SESSION_IO.lock();
    if SESSION_GEN.load(Acquire) != gen { return false; }
    let _guard = STATE_LOCK.lock();
    write_state_to(path, st);
    true
}

/// Err if the session changed since `gen`.
fn still_signed_in(gen: u64) -> Result<()> {
    anyhow::ensure!(SESSION_GEN.load(Acquire) == gen, "You were signed out.");
    Ok(())
}

/// The fields of a title's metadata worth syncing (the rest comes back from its add-on).
fn slim_meta(meta: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in ["name", "poster", "background", "logo", "releaseInfo", "year", "genres", "imdbRating", "runtime", "imdb_id"] {
        if let Some(v) = meta.get(k).filter(|v| !v.is_null()) { out.insert(k.into(), v.clone()); }
    }
    if let Some(d) = meta["description"].as_str() { out.insert("description".into(), json!(d.chars().take(600).collect::<String>())); }
    // Episodes as labels only: where you are in a series reads "S2 E5".
    if let Some(videos) = meta["videos"].as_array() {
        let slim: Vec<Value> = videos.iter().take(3000).map(|e| json!({"id":e["id"],"season":e["season"],"episode":e["episode"]})).collect();
        out.insert("videos".into(), json!(slim));
    }
    Value::Object(out)
}

/// A synced row's identity: "m|…" a title, "p|…|video" one episode's (or film's) record. Rows
/// stored on the server say which they are ("t"); the core's own lists tell titles by "history".
fn row_key(row: &Value) -> String {
    let episode = match row["t"].as_str() { Some(t) => t == "p", None => row.get("history").is_none() };
    if episode { format!("p|{}|{}|{}|{}", s(row, "type"), s(row, "id"), s(row, "addon_url"), s(row, "video_id")) }
    else { format!("m|{}|{}|{}", s(row, "type"), s(row, "id"), s(row, "addon_url")) }
}

static SYNCING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// When the last sync finished (seconds), 0 if never.
pub fn last_synced() -> i64 {
    if account().is_some() { LAST_SYNCED.load(std::sync::atomic::Ordering::Relaxed) } else { 0 }
}

/// Take the other devices' changes, then send this one's. Returns how many rows came and went.
pub async fn sync_now() -> Result<(usize, usize)> {
    let _one = SYNCING.lock().await;
    sync_now_locked().await
}

/// Match SQLite's MAX_REMOTE_SKEW_SECS so clamped remote timestamps stored locally match `st.seen`.
const MAX_REMOTE_SKEW_SECS: i64 = 300;

/// Apply the same clock-skew clamp that SQLite uses on pulled rows before recording `st.seen`
/// so a far-future remote stamp never causes endless re-pushes.
fn record_pulled_row(key: &str, row: &mut Value, legacy: bool, horizon: i64, st: &mut State) {
    if row["deleted"] == true {
        st.known.remove(key);
    } else {
        st.known.insert(key.to_owned());
    }
    // A row from a plaintext document is not marked as exchanged: it is sent again,
    // sealed, when this device sends its own rows.
    if let Some(t) = row["updated"].as_i64() {
        let clamped = t.min(horizon);
        if clamped != t { row["updated"] = json!(clamped); }
        if !legacy { st.seen.insert(key.to_owned(), clamped); }
    }
}

async fn sync_now_locked() -> Result<(usize, usize)> {
    // Everything below is for the session at this moment: if it changes (sign-out, another
    // account), the sync stops and writes nothing more of its own.
    let gen = SESSION_GEN.load(Acquire);
    let (token, uid) = id_token().await?;
    let base = documents().context("Accounts aren't set up in this build")?;
    let sync_key = ensure_sync_key(&uid)?;
    let mut st = state_for(&uid);
    // Keep legacy acceptance stable across the entire sync pass so encountering one v3 doc never
    // causes older plaintext/v2 docs later on the same or following page to be skipped.
    let allow_legacy = !st.key_verified;
    let mut saw_v3 = st.key_verified;
    let horizon = now_secs().saturating_add(MAX_REMOTE_SKEW_SECS);
    // Take: every document the server changed since the last time, in pages.
    let mut taken = 0;
    let mut wrong_key_seen = false;
    loop {
        let docs = list_items(&token, &uid, st.taken_until.as_deref(), st.taken_doc.as_deref(), 300).await?;
        if docs.is_empty() { break; }
        let (mut media, mut playback) = (Vec::new(), Vec::new());
        for (_, fields) in &docs {
            let (mut row, legacy) = match open_item_fields_with(fields, &sync_key, allow_legacy) {
                ItemOpen::Ok(row) => {
                    let ver = fields["v"]["integerValue"].as_str().and_then(|v| v.parse::<i64>().ok()).unwrap_or(3);
                    if fields["sealed"]["stringValue"].as_str().is_some() && ver >= 3 { saw_v3 = true; }
                    (row, false)
                }
                ItemOpen::Legacy(row) => (row, true),
                ItemOpen::WrongKey => { wrong_key_seen = true; continue; }
                ItemOpen::Empty => continue,
            };
            let key = row_key(&row);
            record_pulled_row(&key, &mut row, legacy, horizon, &mut st);
            if key.starts_with("p|") { playback.push(row) } else { media.push(row) }
        }
        if wrong_key_seen {
            SYNC_KEY_NEEDED.store(true, std::sync::atomic::Ordering::Relaxed);
            anyhow::bail!("Your synced data is encrypted with another device's Recovery Key. Enter it under Settings → Account.");
        }
        taken += docs.len();
        still_signed_in(gen)?;
        exec(json!({"op":"sync_apply","media":media,"playback":playback})).await?;
        if let Some((last_name, last_fields)) = docs.last() {
            if let Some(ts) = last_fields["synced"]["timestampValue"].as_str() {
                st.taken_until = Some(ts.to_owned());
                st.taken_doc = Some(last_name.clone());
            }
        }
        anyhow::ensure!(save_state_checked(&state_path(), gen, &st), "You were signed out.");
        if docs.len() < 300 { break; }
    }
    still_signed_in(gen)?;
    // Sync encrypted keys & add-ons before pushing so we detect a mismatched key before overwriting.
    // On an unverified local key, abort rather than pushing items encrypted under an unchecked key.
    let mut keys_synced = false;
    if let Err(e) = sync_keys(&token, &uid, &base, &sync_key, &mut st, gen, allow_legacy).await {
        if sync_key_needed() || !saw_v3 { return Err(e); }
        dev_log(&format!("keys: {e}"));
    } else {
        keys_synced = true;
    }
    // Send: page media and playback independently with (updated, key) cursors so a newer
    // playback row can never advance the cursor past unread media rows.
    let mut total_sent = 0usize;
    let mut media_cursor = (st.media_sent_until, None::<String>);
    let mut playback_cursor = (st.playback_sent_until, None::<(String, String)>);
    let mut media_done = false;
    let mut playback_done = false;
    let mut newest_media = st.media_sent_until;
    let mut newest_playback = st.playback_sent_until;
    let mut first_page = true;
    loop {
        let mut req = json!({
            "op": "sync_changes",
            "media_since": if media_done { i64::MAX - 1 } else { media_cursor.0 },
            "playback_since": if playback_done { i64::MAX - 1 } else { playback_cursor.0 },
        });
        if !media_done {
            if let Some(k) = &media_cursor.1 { req["media_after_key"] = json!(k); }
        }
        if !playback_done {
            if let Some((mk, vk)) = &playback_cursor.1 {
                req["playback_after_media"] = json!(mk);
                req["playback_after_video"] = json!(vk);
            }
        }
        let changes = exec(req).await?;
        let media_rows = changes["media"].as_array().cloned().unwrap_or_default();
        let playback_rows = changes["playback"].as_array().cloned().unwrap_or_default();
        let media_more = changes["media_more"].as_bool().unwrap_or(false);
        let playback_more = changes["playback_more"].as_bool().unwrap_or(false);
        // Drain both persisted and concurrently queued tombstones on the first page; coalesce
        // with live media writes by row key so re-saving after forget publishes the newer state
        // and never emits duplicate document writes in one Firestore commit.
        let mut pending_forgotten: Vec<Value> = Vec::new();
        let mut forgotten_by_key: HashMap<String, Value> = HashMap::new();
        if first_page {
            pending_forgotten = std::mem::take(&mut st.forgotten);
            if let Ok(mut queued) = FORGOTTEN.lock() {
                for item in queued.drain(..) {
                    if !pending_forgotten.contains(&item) { pending_forgotten.push(item); }
                }
            }
            for gone in &pending_forgotten {
                let key = row_key(gone);
                let ts = gone["updated"].as_i64().unwrap_or(0);
                let replace = forgotten_by_key.get(&key).is_none_or(|prev| prev["updated"].as_i64().unwrap_or(0) <= ts);
                if replace { forgotten_by_key.insert(key, gone.clone()); }
            }
        }
        let mut writes = Vec::new();
        for row in &media_rows {
            let key = row_key(row);
            let updated = row["updated"].as_i64().unwrap_or(0);
            newest_media = newest_media.max(updated);
            if let Some(gone) = forgotten_by_key.get(&key) {
                let del_ts = gone["updated"].as_i64().unwrap_or(0);
                if row["history"] == true && updated >= del_ts {
                    // User re-saved or re-watched after forgetting: media wins, drop stale tombstone.
                    forgotten_by_key.remove(&key);
                } else {
                    // Tombstone is newer (or row is metadata-only): keep tombstone, skip media write.
                    continue;
                }
            }
            if st.seen.get(&key) == Some(&updated) { continue; }
            if row["history"] == false && !st.known.contains(&key) { continue; }
            let mut data = row.clone();
            if let Some(o) = data.as_object_mut() { o.remove("key"); o.remove("history"); }
            data["t"] = json!("m");
            if data.get("meta").is_some() { data["meta"] = slim_meta(&row["meta"]); }
            // A row too big for the rules to accept is left out (and said so, without its name),
            // never allowed to fail the whole commit it would travel in.
            let Some(write) = fitting_item_write(&base, &uid, &doc_id(&key), &data, &sync_key)? else { dev_log("sync: a title was too large to sync and was left out"); continue };
            writes.push(write);
            st.seen.insert(key.clone(), updated);
            st.known.insert(key);
        }
        for row in &playback_rows {
            let key = row_key(row);
            let updated = row["updated"].as_i64().unwrap_or(0);
            newest_playback = newest_playback.max(updated);
            if st.seen.get(&key) == Some(&updated) { continue; }
            let mut data = row.clone();
            if let Some(o) = data.as_object_mut() { o.remove("key"); o.remove("history"); }
            data["t"] = json!("p");
            let Some(write) = fitting_item_write(&base, &uid, &doc_id(&key), &data, &sync_key)? else { dev_log("sync: an episode record was too large to sync and was left out"); continue };
            writes.push(write);
            st.seen.insert(key.clone(), updated);
            st.known.insert(key);
        }
        for (key, gone) in &forgotten_by_key {
            let del_ts = gone["updated"].as_i64().unwrap_or(0);
            let Some(write) = fitting_item_write(&base, &uid, &doc_id(key), gone, &sync_key)? else { continue };
            writes.push(write);
            st.seen.insert(key.clone(), del_ts);
            st.known.remove(key);
        }
        first_page = false;
        let page_sent = writes.len();
        still_signed_in(gen)?;
        if let Err(e) = commit(&token, writes).await {
            if !pending_forgotten.is_empty() {
                if let Ok(mut queued) = FORGOTTEN.lock() {
                    for item in pending_forgotten {
                        if !queued.contains(&item) { queued.push(item); }
                    }
                }
            }
            return Err(e);
        }
        total_sent += page_sent;
        if media_more {
            if let Some(last) = media_rows.last() {
                media_cursor = (last["updated"].as_i64().unwrap_or(media_cursor.0), last["key"].as_str().map(str::to_owned));
            } else {
                media_done = true;
            }
        } else {
            media_done = true;
        }
        if playback_more {
            if let Some(last) = playback_rows.last() {
                let pair = last["key"].as_str().zip(last["video_id"].as_str()).map(|(m, v)| (m.to_owned(), v.to_owned()));
                playback_cursor = (last["updated"].as_i64().unwrap_or(playback_cursor.0), pair);
            } else {
                playback_done = true;
            }
        } else {
            playback_done = true;
        }
        if media_done && playback_done { break; }
    }
    // A second's changes may still be coming: the next sync looks at that second again. Never
    // past now: a row stamped by a clock that was ahead would otherwise move the cursor past every
    // later change and freeze sending.
    let now = now_secs();
    st.media_sent_until = (newest_media - 1).max(st.media_sent_until).min(now);
    st.playback_sent_until = (newest_playback - 1).max(st.playback_sent_until).min(now);
    st.sent_until = st.media_sent_until.min(st.playback_sent_until);
    st.last = now;
    if keys_synced || saw_v3 {
        st.key_verified = true;
        SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    anyhow::ensure!(save_state_checked(&state_path(), gen, &st), "You were signed out.");
    Ok((taken, total_sent))
}

/// A title forgotten here is forgotten on the other devices too.
pub fn sync_forgotten(media: &Media) {
    let Some(me) = account() else { return };
    let entry = json!({"t":"m","type":media.kind,"id":media.id,"addon_url":media.addon_url,"updated":now_secs(),"deleted":true});
    if let Ok(mut queued) = FORGOTTEN.lock() { queued.push(entry); }
    // Persist immediately under the session and state locks so a crash before the next sync keeps
    // the tombstone, without racing against sync_now or a sign-out in between.
    {
        let _io = SESSION_IO.lock();
        let current = SESSION.lock().ok().and_then(|s| s.as_ref().map(|x| x.account.uid.clone()));
        if current.as_deref() == Some(me.uid.as_str()) {
            let _guard = STATE_LOCK.lock();
            let st = load_state_unlocked().bound_to(&me.uid);
            save_state_unlocked(&st);
        }
    }
    sync_soon();
}

static WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sync a few seconds from now (after a change here).
pub fn sync_soon() { WANTED.store(true, std::sync::atomic::Ordering::Relaxed); }

/// While waiting after failures: 30 s, doubling to a quarter of an hour.
fn backoff_secs(failures: u32) -> u64 { (30u64 << failures.saturating_sub(1).min(5)).min(900) }

/// While signed in: sync at launch, a few seconds after each change here, and every two minutes.
/// After a failed sync the next try waits longer each time, so an outage or a refusal is not
/// asked again every few seconds.
pub fn start_sync() {
    if account().is_none() { return; }
    let Some(runtime) = RUNTIME.get() else { return };
    if STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) { return; }
    sync_soon();
    runtime.spawn(async {
        let mut idle = 0u32;
        let mut failures = 0u32;
        let mut retry_at = std::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            // Check session and clear STARTED atomically under SESSION lock so a sign-in racing
            // with sign-out can never leave STARTED=true with no background loop alive.
            let signed_in = SESSION.lock().ok().is_some_and(|slot| {
                if slot.is_none() {
                    STARTED.store(false, std::sync::atomic::Ordering::Release);
                    false
                } else {
                    true
                }
            });
            if !signed_in { return; }
            idle += 1;
            let due = WANTED.load(std::sync::atomic::Ordering::Relaxed) || idle >= 24;
            if due && std::time::Instant::now() >= retry_at {
                WANTED.store(false, std::sync::atomic::Ordering::Relaxed);
                idle = 0;
                match sync_now().await {
                    Ok((took, sent)) => {
                        failures = 0;
                        if took + sent > 0 { dev_log(&format!("sync: took {took}, sent {sent}")); }
                    }
                    Err(e) => {
                        failures += 1;
                        retry_at = std::time::Instant::now() + Duration::from_secs(backoff_secs(failures));
                        // What was wanted still is: tried again once the wait is over.
                        WANTED.store(true, std::sync::atomic::Ordering::Relaxed);
                        dev_log(&format!("sync: {e}"));
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------- encrypted settings & add-ons sync
// Your audio language, preferences and add-ons sync through your account's private
// Firestore document (`users/{uid}/config/keys`), end-to-end encrypted with your 256-bit Sync Key.

/// The device the synced settings came from, once they arrived here (shown once).
static ARRIVED: Mutex<Option<String>> = Mutex::new(None);

/// Generic platform label in sync metadata (never sends computer hostnames or device serials).
fn device_name() -> String {
    #[cfg(target_os = "android")]
    { if crate::is_tv() { "Android TV".into() } else { "Android".into() } }
    #[cfg(windows)]
    { "Windows".into() }
    #[cfg(target_os = "macos")]
    { "macOS".into() }
    #[cfg(all(not(target_os = "android"), not(windows), not(target_os = "macos")))]
    { "Linux".into() }
}

/// The settings this device can hand over during QR linking.
fn keys_here() -> Value {
    let s = load_settings();
    json!({"stream_language": s.stream_language})
}

/// The device your synced settings just came from (once).
pub fn take_keys_arrived() -> Option<String> { ARRIVED.lock().ok()?.take() }

/// One GET: its status and, when it succeeded, its answer.
async fn get_raw(token: String, path: String) -> Result<(reqwest::StatusCode, Option<Value>)> {
    on_runtime(async move {
        let r = backend().http.get(format!("https://firestore.googleapis.com/v1/{path}")).bearer_auth(token).timeout(Duration::from_secs(15)).send().await
            .map_err(|_| anyhow!("Could not reach the sync service"))?;
        let status = r.status();
        if !status.is_success() { return Ok((status, None)); }
        r.json::<Value>().await.map(|v| (status, Some(v))).map_err(|_| anyhow!("Sync service returned an unreadable response"))
    }).await?
}

/// A document, None if there is none. Refused with 401, the request is made once more with a
/// fresh token.
async fn get_doc(token: &str, path: String) -> Result<Option<Value>> {
    let (mut status, mut doc) = get_raw(token.to_owned(), path.clone()).await?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        if let Some(fresh) = token_after_401(token).await { (status, doc) = get_raw(fresh, path).await?; }
    }
    if status == reqwest::StatusCode::NOT_FOUND { return Ok(None); }
    anyhow::ensure!(status.is_success(), "Firestore answered {status}");
    Ok(doc)
}

fn keys_digest(payload: &Value) -> String {
    format!("{:x}", Sha256::digest(payload.to_string().as_bytes()))
}

fn has_any_sync_config(payload: &Value) -> bool {
    payload["addons"]["urls"].as_array().is_some_and(|a| !a.is_empty())
        || payload["addons"]["others"].as_array().is_some_and(|a| !a.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
enum KeysSyncAction { Adopt, Push, /** Both sides changed: combine them, then push the result. */ Merge, KeepSynced, Noop }

struct KeysSyncInput<'a> {
    local_has_keys: bool,
    local_hash: &'a str,
    last_synced_hash: &'a str,
    last_synced_updated: i64,
    remote_has_keys: bool,
    remote_hash: Option<&'a str>,
    remote_updated: i64,
}

/// Decide whether this device should pull remote config, push local config, combine the two, or
/// do nothing. Pushing never silently replaces a configuration another device saved since this
/// one last synced (or one this device has never seen): that is a merge.
fn decide_keys_sync(i: &KeysSyncInput<'_>) -> KeysSyncAction {
    let local_changed = !i.last_synced_hash.is_empty() && i.local_hash != i.last_synced_hash;
    // Another device saved something this one has not taken yet.
    let remote_is_newer = i.remote_has_keys
        && i.remote_updated > i.last_synced_updated
        && i.remote_hash.is_some_and(|rh| rh != i.last_synced_hash && rh != i.local_hash);
    // A newly signed-in device without keys/add-ons adopts the account's config automatically.
    if !i.local_has_keys && i.remote_has_keys && !local_changed {
        return KeysSyncAction::Adopt;
    }
    // Local settings or add-ons were edited on this device since last sync: push them, with what
    // another device saved meanwhile.
    if local_changed {
        return if remote_is_newer { KeysSyncAction::Merge } else { KeysSyncAction::Push };
    }
    // First sync on a device that already has keys/add-ons configured.
    if i.last_synced_hash.is_empty() && i.local_has_keys {
        return match i.remote_hash {
            Some(rh) if rh == i.local_hash => KeysSyncAction::KeepSynced,
            Some(_) if i.remote_has_keys => KeysSyncAction::Merge,
            _ => KeysSyncAction::Push,
        };
    }
    // Another device pushed a newer configuration.
    if let Some(rh) = i.remote_hash {
        if i.remote_updated > i.last_synced_updated && rh != i.local_hash {
            return KeysSyncAction::Adopt;
        }
        if rh == i.local_hash && i.last_synced_hash != i.local_hash {
            return KeysSyncAction::KeepSynced;
        }
    }
    KeysSyncAction::Noop
}

/// What both devices saved, combined: this device's own settings win where both have one;
/// add-ons and per-title track choices from either are kept.
fn merge_keys(local: &Value, remote: &Value) -> Value {
    let mut out = remote.clone();
    if !local["stream_language"].is_null() { out["stream_language"] = local["stream_language"].clone(); }
    let overlay = |base: &Value, top: &Value| -> Value {
        let mut merged = base.as_object().cloned().unwrap_or_default();
        for (k, v) in top.as_object().into_iter().flatten() { merged.insert(k.clone(), v.clone()); }
        Value::Object(merged)
    };
    out["prefs"] = overlay(&remote["prefs"], &local["prefs"]);
    out["track_prefs"] = overlay(&remote["track_prefs"], &local["track_prefs"]);
    let mut addons = remote["addons"].as_object().cloned().unwrap_or_default();
    for list in ["urls", "others"] {
        let mut union: Vec<Value> = remote["addons"][list].as_array().cloned().unwrap_or_default();
        for u in local["addons"][list].as_array().into_iter().flatten() { if !union.contains(u) { union.push(u.clone()); } }
        if !union.is_empty() { addons.insert(list.to_owned(), Value::Array(union)); }
    }
    out["addons"] = Value::Object(addons);
    out
}

/// Legacy v2 deterministic key derivation, kept READ-ONLY strictly to migrate pre-E2EE documents
/// into real v3 zero-knowledge encryption on first sync. Never used to encrypt new data.
fn legacy_v2_envelope_key(uid: &str, project: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"veylo-account-keys-v2\0");
    h.update(project.as_bytes());
    h.update(b"\0");
    h.update(uid.as_bytes());
    h.finalize().into()
}

fn seal_keys_payload(sync_key: &[u8; 32], payload: &Value) -> Result<String> {
    seal(sync_key, payload.to_string().as_bytes())
}

#[derive(Debug, PartialEq)]
enum KeysOpen { V3(Value), Legacy(Value), WrongKey, Empty }

/// `allow_legacy`: still accept a document from before v3 (a plaintext one, or sealed by the
/// old account-derived key). Refused once this device has verified its key against v3 data.
fn open_keys_payload(sync_key: &[u8; 32], uid: &str, project: &str, fields: &Value, allow_legacy: bool) -> KeysOpen {
    let ver = fields["v"]["integerValue"].as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(1);
    if let Some(sealed) = fields["sealed"]["stringValue"].as_str() {
        if ver >= 3 {
            return match open_sealed(sync_key, sealed).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                Some(v) => KeysOpen::V3(v),
                None => KeysOpen::WrongKey,
            };
        }
        if !allow_legacy { return KeysOpen::Empty; }
        // Legacy v2 doc: migrate cleanly to v3 E2EE.
        let legacy_key = legacy_v2_envelope_key(uid, project);
        if let Some(v) = open_sealed(&legacy_key, sealed).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            return KeysOpen::Legacy(v);
        }
        return KeysOpen::Empty;
    }
    if !allow_legacy { return KeysOpen::Empty; }
    // Legacy v1 plaintext doc: migrate cleanly to v3 E2EE.
    match fields["data"]["stringValue"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        Some(v) => KeysOpen::Legacy(v),
        None => KeysOpen::Empty,
    }
}

/// This device's settings, add-ons and track choices as they are synced.
async fn keys_snapshot() -> Value {
    let settings = load_settings();
    json!({
        "stream_language": settings.stream_language,
        "addons": crate::addons::for_handoff().await,
        "prefs": {
            "better_posters": settings.better_posters,
            "picks_hide_watched": settings.picks_hide_watched,
            "sub_scale": settings.sub_scale,
            "sub_pos": settings.sub_pos,
            "sub_bg": settings.sub_bg,
            "sub_color": settings.sub_color,
            "audio_night_mode": settings.audio_night_mode,
            "audio_passthrough": settings.audio_passthrough,
            "auto_next": settings.auto_next,
            "max_bitrate_mbps": settings.max_bitrate_mbps,
        },
        "track_prefs": export_track_prefs_value(),
    })
}

fn is_create_precondition_failure(status: u16, v: &Value) -> bool {
    status == 409
        || matches!(v["error"]["status"].as_str(), Some("FAILED_PRECONDITION" | "ALREADY_EXISTS"))
        || v["error"]["message"].as_str().is_some_and(|m| {
            m.contains("ALREADY_EXISTS") || m.contains("FAILED_PRECONDITION") || m.contains("already exists")
        })
}

/// Write the keys document (sealed) with a time later than whatever is there; returns `Some(updated)`,
/// or `None` when `create_only` lost a concurrent first-creation race on `config/keys`.
async fn try_push_keys(token: &str, doc_path: &str, sync_key: &[u8; 32], snapshot: &Value, remote_updated: i64, create_only: bool) -> Result<Option<i64>> {
    let updated = now_secs().max(remote_updated + 1);
    let sealed = seal_keys_payload(sync_key, snapshot)?;
    let mut write = json!({
        "update": {
            "name": doc_path,
            "fields": {
                "sealed": {"stringValue": sealed},
                "v": {"integerValue": "3"},
                "updated": {"integerValue": updated.to_string()},
                "device": {"stringValue": device_name()},
            }
        },
        "updateTransforms": [{"fieldPath": "synced", "setToServerValue": "REQUEST_TIME"}],
    });
    if create_only {
        write["currentDocument"] = json!({"exists": false});
    }
    let base = documents().context("Accounts aren't set up in this build")?;
    let (status, v) = post_session_raw(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes": [write]}), token).await?;
    if create_only && is_create_precondition_failure(status, &v) {
        return Ok(None);
    }
    answer(status, v)?;
    Ok(Some(updated))
}

async fn push_keys(token: &str, doc_path: &str, sync_key: &[u8; 32], snapshot: &Value, remote_updated: i64) -> Result<i64> {
    try_push_keys(token, doc_path, sync_key, snapshot, remote_updated, false).await?
        .ok_or_else(|| anyhow!("Unexpected precondition failure updating keys"))
}

/// Automatically sync audio language, preferences and add-ons through
/// `users/{uid}/config/keys` using real zero-knowledge v3 AES-256-GCM encryption.
async fn sync_keys(token: &str, uid: &str, base: &str, sync_key: &[u8; 32], st: &mut State, gen: u64, allow_legacy: bool) -> Result<()> {
    let project = config().map(|c| c.project).unwrap_or_default();
    let doc_path = format!("{base}/users/{uid}/config/keys");

    for attempt in 0..2 {
        let local = keys_snapshot().await;
        let local_hash = keys_digest(&local);
        let local_has_keys = has_any_sync_config(&local);

        let remote_doc = get_doc(token, doc_path.clone()).await?;
        still_signed_in(gen)?;
        let remote_fields = remote_doc.as_ref().map(|d| &d["fields"]);
        let (remote_data, remote_is_legacy) = match remote_fields.map(|f| open_keys_payload(sync_key, uid, project, f, allow_legacy)) {
            Some(KeysOpen::V3(v)) => (Some(v), false),
            Some(KeysOpen::Legacy(v)) => (Some(v), true),
            Some(KeysOpen::WrongKey) => {
                SYNC_KEY_NEEDED.store(true, std::sync::atomic::Ordering::Relaxed);
                anyhow::bail!("Your synced settings are encrypted with another device's Recovery Key. Enter it under Settings → Account.");
            }
            Some(KeysOpen::Empty) | None => (None, false),
        };
        let remote_updated = remote_fields
            .and_then(|f| f["updated"]["integerValue"].as_str())
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0);
        let remote_device = remote_fields
            .and_then(|f| f["device"]["stringValue"].as_str())
            .unwrap_or_default()
            .to_owned();
        let remote_hash = remote_data.as_ref().map(keys_digest);
        let remote_has_keys = remote_data.as_ref().is_some_and(has_any_sync_config);

        let mut action = decide_keys_sync(&KeysSyncInput {
            local_has_keys,
            local_hash: &local_hash,
            last_synced_hash: &st.keys_hash,
            last_synced_updated: st.keys_updated,
            remote_has_keys,
            remote_hash: remote_hash.as_deref(),
            remote_updated,
        });
        // Upgrade legacy v1/v2 documents to true v3 E2EE once local matches.
        if remote_is_legacy && matches!(action, KeysSyncAction::KeepSynced | KeysSyncAction::Noop) {
            action = KeysSyncAction::Push;
        }
        // On an empty account without `config/keys`, atomically create the key anchor so
        // concurrent first syncs can never seal items under two incompatible keys.
        if remote_doc.is_none() && action == KeysSyncAction::Noop {
            let Some(updated) = try_push_keys(token, &doc_path, sync_key, &local, remote_updated, true).await? else {
                anyhow::ensure!(attempt == 0, "Settings changed on another device while syncing; retrying.");
                continue;
            };
            st.keys_updated = updated;
            st.keys_hash = local_hash;
            return Ok(());
        }

        match action {
            KeysSyncAction::Adopt => {
                if let Some(remote) = remote_data {
                    adopt_keys(&remote).await;
                    still_signed_in(gen)?;
                    let adopted = keys_snapshot().await;
                    st.keys_hash = keys_digest(&adopted);
                    st.keys_updated = if remote_is_legacy {
                        push_keys(token, &doc_path, sync_key, &adopted, remote_updated).await?
                    } else {
                        remote_updated
                    };
                    if has_any_sync_config(&remote) {
                        if let Ok(mut a) = ARRIVED.lock() { *a = Some(remote_device); }
                    }
                    dev_log("keys: synced automatically from account");
                }
            }
            KeysSyncAction::Push => {
                let create_only = remote_doc.is_none();
                let Some(updated) = try_push_keys(token, &doc_path, sync_key, &local, remote_updated, create_only).await? else {
                    anyhow::ensure!(attempt == 0, "Settings changed on another device while syncing; retrying.");
                    continue;
                };
                st.keys_updated = updated;
                st.keys_hash = local_hash;
                dev_log("keys: saved (E2EE v3) to account");
            }
            KeysSyncAction::Merge => {
                if let Some(remote) = remote_data {
                    // Both sides have something the other lacks: nothing of either is thrown away.
                    adopt_keys(&merge_keys(&local, &remote)).await;
                    still_signed_in(gen)?;
                    let merged = keys_snapshot().await;
                    st.keys_updated = push_keys(token, &doc_path, sync_key, &merged, remote_updated).await?;
                    st.keys_hash = keys_digest(&merged);
                    dev_log("keys: merged with the account's newer settings");
                }
            }
            KeysSyncAction::KeepSynced => {
                st.keys_hash = local_hash;
                st.keys_updated = remote_updated;
            }
            KeysSyncAction::Noop => {}
        }
        return Ok(());
    }
    Ok(())
}

/// A subtitle colour as the settings keep it: `#` and six hexadecimal digits, nothing else.
fn valid_sub_color(c: &str) -> bool { c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()) }

/// Settings and add-ons from your account or another device: saved here, then add-ons adopted.
async fn adopt_keys(v: &Value) {
    let text = |k: &str| v[k].as_str().map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned);
    let language = text("stream_language");
    update_settings(|s| {
        if let Some(l) = language.clone().filter(|l| crate::addons::language(l).is_some()) { s.stream_language = Some(l); }
        if let Some(p) = v.get("prefs") {
            if let Some(b) = p["better_posters"].as_bool() { s.better_posters = b; }
            if let Some(b) = p["picks_hide_watched"].as_bool() { s.picks_hide_watched = b; }
            if let Some(f) = p["sub_scale"].as_f64() { s.sub_scale = f.clamp(0.6, 2.0); }
            if let Some(n) = p["sub_pos"].as_i64() { s.sub_pos = n.clamp(70, 100) as i32; }
            if let Some(b) = p["sub_bg"].as_bool() { s.sub_bg = b; }
            if let Some(c) = p["sub_color"].as_str().filter(|c| valid_sub_color(c)) { s.sub_color = c.to_owned(); }
            if let Some(b) = p["audio_night_mode"].as_bool() { s.audio_night_mode = b; }
            if let Some(b) = p["audio_passthrough"].as_bool() { s.audio_passthrough = b; }
            if let Some(b) = p["auto_next"].as_bool() { s.auto_next = b; }
            if p.get("max_bitrate_mbps").is_some() { s.max_bitrate_mbps = parse_max_bitrate(p); }
        }
    });
    if let Some(tp) = v.get("track_prefs") { merge_track_prefs_value(tp); }
    crate::addons::adopt(&v["addons"]).await;
}

// ---------------------------------------------------------------- linking a computer

/// The computer's half: what its QR code says, and its wait for the phone's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkOffer { pub code: String, link: String, key: Vec<u8>, token: String }

impl LinkOffer {
    /// Short 6-digit visual pairing code (`XXX-XXX`) shown on both devices so the phone user can
    /// confirm they are approving their own screen before transferring the session and E2EE key.
    pub fn check_code(&self) -> String { link_sas(&self.link, &self.key) }
}

/// A scanned QR link awaiting explicit confirmation on the phone before credentials are sent.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingLink {
    pub code: String,
    pub check_code: String,
}

fn link_sas(link: &str, key: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"veylo-link-sas-v1\0");
    h.update(link.as_bytes());
    h.update(b"\0");
    h.update(key);
    let digest = h.finalize();
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03}-{:03}", n / 1000, n % 1000)
}

fn parse_link_code(code: &str) -> Result<(String, Vec<u8>)> {
    let parts: Vec<&str> = code.trim().split(':').collect();
    let ["veylo-link", "1", link, key] = parts[..] else { anyhow::bail!("That isn't a Veylo sign-in code.") };
    anyhow::ensure!(link.len() <= 64 && link.bytes().all(|b| b.is_ascii_alphanumeric()), "That isn't a Veylo sign-in code.");
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(key).ok().filter(|k| k.len() == 32).context("That isn't a Veylo sign-in code.")?;
    Ok((link.to_owned(), key))
}

/// Validate a scanned Veylo link code and compute its 6-digit visual confirmation code.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn parse_pending_link(code: &str) -> Result<PendingLink> {
    let (link, key) = parse_link_code(code)?;
    Ok(PendingLink { code: code.trim().to_owned(), check_code: link_sas(&link, &key) })
}

/// Show a QR code to scan with a signed-in phone or tablet: this computer signs in for a moment
/// without an account and leaves a request only it (and the phone holding the code) can answer.
/// An offer nobody answers is taken down with `cancel_link`.
pub async fn offer_link() -> Result<LinkOffer> {
    let v = identity("signUp", json!({"returnSecureToken":true})).await?;
    let (token, uid) = (s(&v, "idToken"), s(&v, "localId"));
    let base = documents().context("Accounts aren't set up in this build")?;
    let link = random_text(18).replace(['-', '_'], "x");
    let mut key = vec![0u8; 32];
    getrandom::fill(&mut key).map_err(|_| anyhow!("No randomness"))?;
    let offer_written = post(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":[{
        "update":{"name":format!("{base}/links/{link}"),"fields":{"asker":{"stringValue":uid}}},
        "updateTransforms":[{"fieldPath":"created","setToServerValue":"REQUEST_TIME"}],
        "currentDocument":{"exists":false}}]}), Some(token.clone())).await;
    if let Err(e) = offer_written {
        // The passing account was made for nothing: it goes again.
        let _ = identity("delete", json!({"idToken":token})).await;
        return Err(e);
    }
    let code = format!("veylo-link:1:{link}:{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&key));
    Ok(LinkOffer { code, link, key, token })
}

/// Take down an offer that was never answered: its request, and the passing account that waited
/// with it. Best effort, and harmless after the offer was answered (both are gone by then).
pub async fn cancel_link(offer: &LinkOffer) {
    if let Some(base) = documents() {
        let _ = post(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":[{"delete":format!("{base}/links/{}", offer.link)}]}), Some(offer.token.clone())).await;
    }
    let _ = identity("delete", json!({"idToken":offer.token})).await;
}

/// `cancel_link` on its own, for a screen that is closing and cannot wait for it.
pub fn cancel_link_soon(offer: LinkOffer) {
    if let Some(runtime) = RUNTIME.get() { runtime.spawn(async move { cancel_link(&offer).await; }); }
}

/// A failure that waiting may cure (no connection, the service busy): polling for the phone's
/// answer goes on after one; any other failure ends it.
#[derive(Debug)]
pub struct Transient(String);
impl std::fmt::Display for Transient { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) } }
impl std::error::Error for Transient {}

/// Whether `take_link` failed only for now (see `Transient`).
pub fn is_transient(e: &anyhow::Error) -> bool { e.downcast_ref::<Transient>().is_some() }

/// Wait (polling) until the phone answers the link; then this computer is signed in as its
/// account. None while unanswered. An `Err` that `is_transient` is only a hiccup: ask again.
pub async fn take_link(offer: &LinkOffer) -> Result<Option<Account>> {
    let base = documents().context("Accounts aren't set up in this build")?;
    let (url, token) = (format!("https://firestore.googleapis.com/v1/{base}/links/{}", offer.link), offer.token.clone());
    let doc: Value = on_runtime(async move {
        let blip = |what: &str| anyhow::Error::new(Transient(format!("Couldn't check for the phone ({what}). Trying again…")));
        let r = backend().http.get(url).bearer_auth(token).timeout(Duration::from_secs(15)).send().await.map_err(|_| blip("no connection"))?;
        let status = r.status();
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS || status == reqwest::StatusCode::REQUEST_TIMEOUT { return Err(blip("the service is busy")); }
        // Anything else that is not an answer (the request is gone, or not ours) reads as unanswered.
        r.json::<Value>().await.map_err(|_| blip("unreadable answer"))
    }).await??;
    let Some(sealed) = doc["fields"]["sealed"]["stringValue"].as_str() else { return Ok(None) };
    let opened = open_sealed(&offer.key, sealed).context("The code didn't match. Show a new one and scan it again.")?;
    let v: Value = serde_json::from_slice(&opened)?;
    // The request goes, and so does the passing account the computer waited with.
    cancel_link(offer).await;
    let session = Session {
        account: Account { uid: s(&v, "uid"), email: s(&v, "email"), name: s(&v, "name"), provider: s(&v, "provider") },
        refresh: s(&v, "refresh"), ..Session::default()
    };
    anyhow::ensure!(!session.refresh.is_empty() && !session.account.uid.is_empty(), "The code didn't match. Show a new one and scan it again.");
    // Transfer the E2EE Sync Key before starting sync so encrypted history & add-ons open immediately.
    // The key this device had is backed up first, never overwritten.
    if let Some(code) = v["sync_key"].as_str() {
        if let Ok(k) = parse_recovery_code(code) {
            {
                let _k = SYNC_KEY_LOCK.lock();
                replace_sync_key_in(&backend().data_dir, &k, &session.account.uid)?;
            }
            SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if v["keys"].is_object() { adopt_keys(&v["keys"]).await; }
    let account = set_session(session);
    // The link has been used up either way: a refresh that only fails for now is not the code's fault.
    if let Err(e) = id_token().await { if self::account().is_none() { return Err(e); } }
    Ok(Some(account))
}

/// The phone's half: answer a computer's QR code with this account's session and E2EE Sync Key,
/// sealed with the code's ephemeral key (only that computer can open it).
pub async fn answer_link(code: &str) -> Result<()> {
    let (link, key) = parse_link_code(code)?;
    // A device that cannot open the account's data holds the wrong key; handing it on would spread it.
    anyhow::ensure!(!sync_key_needed(), "Enter this account's Recovery Key first: this device can't open its encrypted data yet.");
    let (token, _) = id_token().await?;
    let session = SESSION.lock().ok().and_then(|x| x.clone()).context("Not signed in")?;
    let sync_key = ensure_sync_key(&session.account.uid)?;
    let a = &session.account;
    // Your settings, add-ons and E2EE Sync Key travel inside the ephemeral encrypted QR handshake.
    let mut keys = keys_here();
    keys["addons"] = crate::addons::for_handoff().await;
    let plain = json!({
        "uid": a.uid, "email": a.email, "name": a.name, "provider": a.provider,
        "refresh": session.refresh,
        "sync_key": format_recovery_code(&sync_key),
        "keys": keys,
    }).to_string();
    let sealed = seal(&key, plain.as_bytes())?;
    let base = documents().context("Accounts aren't set up in this build")?;
    post_session(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":[{
        "update":{"name":format!("{base}/links/{link}"),"fields":{"sealed":{"stringValue":sealed}}},
        "updateMask":{"fieldPaths":["sealed"]},"currentDocument":{"exists":true}}]}), &token).await
        .map_err(|_| anyhow!("That code has expired. Show a new one on the computer."))?;
    Ok(())
}

/// Android: scan a computer's QR code and validate it locally without sending credentials yet;
/// the caller confirms the 6-digit verification code before calling `answer_link`.
#[cfg(target_os = "android")]
pub async fn scan_link_code() -> Result<PendingLink> {
    crate::android::scan_code()?;
    let mut found = None;
    for _ in 0..1200 {
        sleep(250).await;
        match crate::android::account_result() {
            Some(r) if r.starts_with("code:") => { found = Some(r["code:".len()..].to_owned()); break; }
            Some(r) if r.starts_with("cancel:") => anyhow::bail!("Scan cancelled."),
            Some(r) if r.starts_with("error:") => anyhow::bail!("The scanner didn't open: {}", &r["error:".len()..]),
            _ => {}
        }
    }
    let code = found.context("Scan took too long. Try again.")?;
    parse_pending_link(&code)
}

fn seal(key: &[u8], plain: &[u8]) -> Result<String> {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    let sealing = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("bad key"))?);
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|_| anyhow!("No randomness"))?;
    let mut data = plain.to_vec();
    sealing.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data).map_err(|_| anyhow!("seal"))?;
    let mut out = nonce.to_vec();
    out.extend(data);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

fn open_sealed(key: &[u8], sealed: &str) -> Option<Vec<u8>> {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    let raw = base64::engine::general_purpose::STANDARD.decode(sealed).ok()?;
    let (nonce, data) = raw.split_at_checked(12)?;
    let opening = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).ok()?);
    let mut data = data.to_vec();
    let plain = opening.open_in_place(Nonce::try_assume_unique_for_key(nonce).ok()?, Aad::empty(), &mut data).ok()?;
    Some(plain.to_vec())
}

/// A QR code as an SVG image for an <img>.
pub fn qr_svg(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let svg = code.render::<qrcode::render::svg::Color>().min_dimensions(240, 240).quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000")).light_color(qrcode::render::svg::Color("#ffffff")).build();
    Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_keys_sync_decision_covers_cross_os_flows() {
        // Fresh device on a different OS (no keys): adopts automatically from account.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: false, local_hash: "h0", last_synced_hash: "", last_synced_updated: 0,
            remote_has_keys: true, remote_hash: Some("h1"), remote_updated: 100,
        }), KeysSyncAction::Adopt);
        // First device with keys, empty account: pushes automatically.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: true, local_hash: "h1", last_synced_hash: "", last_synced_updated: 0,
            remote_has_keys: false, remote_hash: None, remote_updated: 0,
        }), KeysSyncAction::Push);
        // Local edit after syncing: pushes new keys.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: true, local_hash: "h2", last_synced_hash: "h1", last_synced_updated: 100,
            remote_has_keys: true, remote_hash: Some("h1"), remote_updated: 100,
        }), KeysSyncAction::Push);
        // Another device pushed newer keys: adopts automatically.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: true, local_hash: "h1", last_synced_hash: "h1", last_synced_updated: 100,
            remote_has_keys: true, remote_hash: Some("h2"), remote_updated: 200,
        }), KeysSyncAction::Adopt);
        // Already in sync: noop.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: true, local_hash: "h1", last_synced_hash: "h1", last_synced_updated: 100,
            remote_has_keys: true, remote_hash: Some("h1"), remote_updated: 100,
        }), KeysSyncAction::Noop);
        // Key removed locally after syncing: pushes removal.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: false, local_hash: "h_empty", last_synced_hash: "h1", last_synced_updated: 100,
            remote_has_keys: true, remote_hash: Some("h1"), remote_updated: 100,
        }), KeysSyncAction::Push);
    }

    #[test]
    fn google_callback_accepts_post_body_and_get_fallback() {
        let post = "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 31\r\n\r\nstate=abc123&id_token=tok.en_1";
        let q = parse_callback_params(post);
        assert_eq!(q.get("state").map(String::as_str), Some("abc123"));
        assert_eq!(q.get("id_token").map(String::as_str), Some("tok.en_1"));

        let get = "GET /?state=xyz&id_token=legacy HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let q2 = parse_callback_params(get);
        assert_eq!(q2.get("state").map(String::as_str), Some("xyz"));
        assert_eq!(q2.get("id_token").map(String::as_str), Some("legacy"));
    }

    #[test]
    fn a_sealed_session_opens_only_with_its_key() {
        let key = [7u8; 32];
        let sealed = seal(&key, b"{\"refresh\":\"r\"}").unwrap();
        assert_eq!(open_sealed(&key, &sealed).unwrap(), b"{\"refresh\":\"r\"}");
        assert_eq!(open_sealed(&[8u8; 32], &sealed), None);
        assert_eq!(open_sealed(&key, "bm9wZQ"), None);
    }

    #[test]
    fn e2ee_sync_key_protects_both_config_and_watch_history_items() {
        let key_a = [0x42u8; 32];
        let key_b = [0x99u8; 32];
        let code = format_recovery_code(&key_a);
        assert!(code.starts_with("VEYLO-"));
        assert_eq!(parse_recovery_code(&code).unwrap(), key_a);
        assert!(parse_recovery_code("not-a-valid-key").is_err());

        // Config/keys v3 E2EE: Firebase operator (knowing uid + project) CANNOT decrypt.
        let payload = json!({"tmdb_key": "TMDB-999", "stream_language": "italian", "addons": {"urls": ["https://a.example/manifest.json"]}});
        let sealed = seal_keys_payload(&key_a, &payload).unwrap();
        assert!(!sealed.contains("TMDB-999"), "sealed blob must not leak keys");
        let fields = json!({"sealed": {"stringValue": sealed}, "v": {"integerValue": "3"}});
        assert_eq!(open_keys_payload(&key_a, "uid-alice", "veylo-proj", &fields, true), KeysOpen::V3(payload.clone()));
        assert_eq!(open_keys_payload(&key_b, "uid-alice", "veylo-proj", &fields, true), KeysOpen::WrongKey);
        let deterministic = legacy_v2_envelope_key("uid-alice", "veylo-proj");
        assert_eq!(open_keys_payload(&deterministic, "uid-alice", "veylo-proj", &fields, true), KeysOpen::WrongKey);

        // Legacy v1 plaintext documents remain readable once for migration.
        let legacy = json!({"data": {"stringValue": payload.to_string()}});
        assert_eq!(open_keys_payload(&key_a, "uid-alice", "veylo-proj", &legacy, true), KeysOpen::Legacy(payload));

        // Watch-history item v3 E2EE: title ID & name never appear in Firestore fields.
        let item = json!({"t": "m", "type": "movie", "id": "tt1375666", "name": "Inception", "watched": true});
        let write = item_write("projects/p/databases/(default)/documents", "uid-alice", "doc1", &item, &key_a).unwrap();
        let write_str = write.to_string();
        assert!(!write_str.contains("tt1375666") && !write_str.contains("Inception"));
        let item_fields = &write["update"]["fields"];
        assert_eq!(open_item_fields(item_fields, &key_a), ItemOpen::Ok(item.clone()));
        assert_eq!(open_item_fields(item_fields, &key_b), ItemOpen::WrongKey);
    }

    #[test]
    fn rows_are_keyed_by_title_and_episode() {
        let film = json!({"type":"movie","id":"tt1","addon_url":"a","history":true,"video_id":"tt1"});
        let episode = json!({"type":"series","id":"tt2","addon_url":"a","video_id":"tt2:1:3"});
        assert_eq!(row_key(&film), "m|movie|tt1|a");
        assert_eq!(row_key(&episode), "p|series|tt2|a|tt2:1:3");
        // As stored on the server: a title says so, whatever video it was at.
        assert_eq!(row_key(&json!({"t":"m","type":"movie","id":"tt1","addon_url":"a","video_id":"tt1"})), "m|movie|tt1|a");
        assert_eq!(row_key(&json!({"t":"p","type":"series","id":"tt2","addon_url":"a","video_id":"tt2:1:3"})), "p|series|tt2|a|tt2:1:3");
        assert_ne!(doc_id(&row_key(&film)), doc_id(&row_key(&episode)));
        assert_eq!(doc_id("x").len(), 40);
    }

    #[test]
    fn synced_metadata_keeps_what_the_library_shows() {
        let meta = json!({"name":"Show","poster":"p","description":"d".repeat(900),"cast":["x"],
            "videos":[{"id":"s:1:1","season":1,"episode":1,"title":"Pilot","overview":"long","thumbnail":"t"}]});
        let slim = slim_meta(&meta);
        assert_eq!(slim["name"], "Show");
        assert!(slim.get("cast").is_none());
        assert_eq!(slim["description"].as_str().unwrap().len(), 600);
        assert_eq!(slim["videos"][0], json!({"id":"s:1:1","season":1,"episode":1}));
    }

    #[test]
    fn link_codes_are_checked_before_use() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for bad in ["hello", "veylo-link:1:abc", "veylo-link:2:abc:AAAA", "veylo-link:1:a/b:AAAA"] {
            assert!(rt.block_on(answer_link(bad)).is_err(), "{bad}");
        }
    }

    /// Against the real project (built with its values, firebase/README.md): a passing account
    /// signs up, syncs a saved film, links a "computer" through the QR handshake, and is deleted.
    /// `VEYLO_DX_DATA=<empty folder> cargo test live_account -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_account_sync_and_link() {
        assert!(accounts_available(), "built without the Firebase values");
        assert!(std::env::var_os("VEYLO_DX_DATA").is_some(), "point VEYLO_DX_DATA at an empty folder");
        init().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let email = format!("veylo-test-{}@example.com", random_text(6).to_lowercase().replace(['-', '_'], "x"));
            let me = sign_up(email.clone(), random_text(12)).await.unwrap();
            println!("signed up {} ({})", me.email, me.uid.len());
            let film = Media::from_json(&json!({"id":"tt1375666","type":"movie","name":"Inception"}), CINEMETA).unwrap();
            set_saved(&film, true).await.unwrap();
            let (took, sent) = sync_now().await.unwrap();
            println!("first sync: took {took}, sent {sent}");
            assert!(sent >= 1);
            let (token, uid) = id_token().await.unwrap();
            let docs = list_items(&token, &uid, None, None, 50).await.unwrap();
            println!("on the server: {} document(s)", docs.len());
            assert!(docs.iter().any(|(_, f)| f["sealed"]["stringValue"].is_string() && f["data"].is_null()));
            // The QR handshake: a computer asks, the phone (this session) answers, the computer takes it.
            let offer = offer_link().await.unwrap();
            assert!(take_link(&offer).await.unwrap().is_none(), "unanswered yet");
            answer_link(&offer.code).await.unwrap();
            let linked = take_link(&offer).await.unwrap().expect("answered");
            assert_eq!(linked.uid, me.uid);
            println!("linked as the same account");
            // Someone else's data stays out of reach.
            let other = list_items(&offer.token, &uid, None, None, 5).await;
            assert!(other.is_err(), "an anonymous session must not read an account's items");
            delete_account().await.unwrap();
            assert!(account().is_none());
            println!("deleted");
        });
    }

    #[test]
    fn items_query_uses_composite_cursor_when_doc_present() {
        let q = items_query(Some("2026-01-01T00:00:00Z"), Some("projects/p/databases/(default)/documents/users/u/items/a"), 300);
        assert_eq!(q["orderBy"].as_array().unwrap().len(), 2);
        assert_eq!(q["orderBy"][1]["field"]["fieldPath"], "__name__");
        assert_eq!(q["startAt"]["before"], false);
        assert_eq!(q["startAt"]["values"][0]["timestampValue"], "2026-01-01T00:00:00Z");
        assert!(q.get("where").is_none());

        let legacy = items_query(Some("2026-01-01T00:00:00Z"), None, 300);
        assert_eq!(legacy["where"]["fieldFilter"]["op"], "GREATER_THAN");
    }

    #[test]
    fn link_confirmation_code_is_deterministic_and_rejects_malformed() {
        let key = vec![7u8; 32];
        let code = format!("veylo-link:1:abc123XYZ:{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&key));
        let pending = parse_pending_link(&code).unwrap();
        let offer = LinkOffer { code: code.clone(), link: "abc123XYZ".into(), key, token: String::new() };
        assert_eq!(pending.check_code, offer.check_code());
        assert_eq!(pending.check_code.len(), 7);
        assert_eq!(pending.check_code.as_bytes()[3], b'-');
        assert!(parse_pending_link("https://evil.example/qr").is_err());
        assert!(parse_pending_link("veylo-link:1:bad-id:AAAA").is_err());
    }

    /// A computer's Google sign-in through the browser page, with your own account: it signs in,
    /// takes what your other devices synced, and signs out again (the account stays).
    /// `VEYLO_DX_DATA=<empty folder> cargo test live_google_desktop -- --ignored --nocapture`
    #[cfg(not(target_os = "android"))]
    #[test]
    #[ignore]
    fn live_google_desktop() {
        assert!(google_available(), "built without the Firebase values");
        assert!(std::env::var_os("VEYLO_DX_DATA").is_some(), "point VEYLO_DX_DATA at an empty folder");
        init().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let me = sign_in_google().await.unwrap();
            println!("signed in with Google ({} provider)", me.provider);
            let (took, _) = sync_now().await.unwrap();
            let library = library().await.unwrap();
            println!("took {took} synced rows; this computer's library now has {} title(s)", library.len());
            sign_out();
        });
    }

    // ------------------------------------------------------------ keys, state and sync safety

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("veylo-account-{tag}-{}-{}", std::process::id(), random_text(6).replace(['-', '_'], "x")));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_sync_key_is_generated_but_an_unreadable_one_is_never_overwritten() {
        let dir = temp_dir("key-read");
        let first = ensure_sync_key_in(&dir, "alice").unwrap();
        assert_eq!(ensure_sync_key_in(&dir, "alice").unwrap(), first, "the same key every time");
        let file = dir.join(SYNC_KEY_FILE);
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"uid\":\"alice\""));
        // A file that cannot be parsed is an error, and is left exactly as it was.
        for broken in ["not json at all", "{\"v\":3,\"key\":\"short\"}", "{}"] {
            std::fs::write(&file, broken).unwrap();
            assert!(ensure_sync_key_in(&dir, "alice").is_err(), "{broken}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
        }
        // An I/O error that is not "not found" (here, a directory where the file should be) is one too.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(ensure_sync_key_in(&dir, "alice").is_err());
        assert!(file.is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sign_out_keeps_the_sync_key_and_another_account_never_uses_it() {
        let dir = temp_dir("key-owner");
        std::fs::write(dir.join("account.json"), "{}").unwrap();
        std::fs::write(dir.join(STATE_FILE), "{}").unwrap();
        let alice = ensure_sync_key_in(&dir, "alice").unwrap();
        clear_session_files(&dir);
        assert!(!dir.join("account.json").exists() && !dir.join(STATE_FILE).exists());
        assert!(dir.join(SYNC_KEY_FILE).exists(), "signing out keeps the Sync Key");
        // The same account finds it again; another one starts its own and parks the first.
        assert_eq!(ensure_sync_key_in(&dir, "alice").unwrap(), alice);
        let bob = ensure_sync_key_in(&dir, "bob").unwrap();
        assert_ne!(bob, alice);
        assert!(parked_key_path(&dir, "alice").exists());
        // Back to the first account: its key returns, and bob's is parked in turn.
        assert_eq!(ensure_sync_key_in(&dir, "alice").unwrap(), alice);
        assert_eq!(ensure_sync_key_in(&dir, "bob").unwrap(), bob);
        // A file from before accounts were recorded belongs to whoever is signed in.
        let legacy = [9u8; 32];
        std::fs::write(dir.join(SYNC_KEY_FILE), json!({"v": 3, "key": format_recovery_code(&legacy)[6..]}).to_string()).unwrap();
        assert_eq!(ensure_sync_key_in(&dir, "carol").unwrap(), legacy);
        assert_eq!(read_key_file(&dir.join(SYNC_KEY_FILE)), KeyFile::Found { key: legacy, uid: Some("carol".into()) });
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_recovery_key_replaces_the_key_only_after_the_old_one_is_backed_up() {
        let dir = temp_dir("key-replace");
        let own = ensure_sync_key_in(&dir, "alice").unwrap();
        let from_phone = [5u8; 32];
        replace_sync_key_in(&dir, &from_phone, "alice").unwrap();
        assert_eq!(ensure_sync_key_in(&dir, "alice").unwrap(), from_phone);
        assert_eq!(read_key_file(&previous_key_path(&dir)), KeyFile::Found { key: own, uid: Some("alice".into()) });
        // Replacing a key that belongs to another account parks that one.
        replace_sync_key_in(&dir, &[6u8; 32], "bob").unwrap();
        assert_eq!(read_key_file(&parked_key_path(&dir, "alice")), KeyFile::Found { key: from_phone, uid: Some("alice".into()) });
        // Only deleting the account removes its keys.
        remove_sync_key_in(&dir, "alice");
        assert!(!parked_key_path(&dir, "alice").exists() && !previous_key_path(&dir).exists());
        assert!(dir.join(SYNC_KEY_FILE).exists(), "bob's active key stays");
        remove_sync_key_in(&dir, "bob");
        assert!(!dir.join(SYNC_KEY_FILE).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sync_progress_belongs_to_one_account_and_cursors_never_pass_now() {
        let now = 1_800_000_000;
        let saved = json!({"uid": "alice", "media_sent_until": now + 9_999_999, "playback_sent_until": now + 5, "sent_until": now + 7,
            "taken_until": "2026-01-01T00:00:00Z", "seen": {"m|a": 5}, "keys_hash": "h", "last": 42});
        let st = parse_state(&saved, now);
        assert_eq!((st.media_sent_until, st.playback_sent_until, st.sent_until), (now, now, now));
        assert_eq!(st.uid, "alice");
        let bound = st.bound_to("alice");
        assert_eq!(bound.last, 42);
        assert!(bound.seen.contains_key("m|a") && bound.key_verified);
        // Another account starts over; a file from before the account was recorded is taken as its own.
        let other = parse_state(&saved, now).bound_to("bob");
        assert_eq!((other.uid.as_str(), other.last, other.taken_until.clone()), ("bob", 0, None));
        assert!(other.seen.is_empty() && !other.key_verified);
        let legacy = parse_state(&json!({"last": 7}), now).bound_to("bob");
        assert_eq!((legacy.uid.as_str(), legacy.last), ("bob", 7));
        // And the uid is saved with it.
        assert_eq!(state_json(&bound, &[])["uid"], "alice");
    }

    #[test]
    fn a_sync_that_outlives_its_session_writes_nothing() {
        let dir = temp_dir("state-gen");
        let path = dir.join(STATE_FILE);
        let st = State { uid: "alice".into(), last: 5, ..State::default() };
        let gen = SESSION_GEN.load(Acquire);
        assert!(save_state_checked(&path, gen, &st));
        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();
        // Sign-out (or another sign-in) bumps the generation: the late write is refused.
        SESSION_GEN.fetch_add(1, AcqRel);
        assert!(!save_state_checked(&path, gen, &st));
        assert!(!path.exists(), "sign-out is not undone by a sync still running");
        assert!(still_signed_in(gen).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commits_are_batched_by_count_and_by_bytes() {
        let small: Vec<Value> = (0..450).map(|i| json!({"delete": format!("d{i}")})).collect();
        let batches = commit_batches(small);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [200, 200, 50]);
        // Big writes split on bytes (each about 600 KB here: three would be over the limit).
        let big: Vec<Value> = (0..7).map(|i| json!({"update": {"name": format!("n{i}"), "fields": {"sealed": {"stringValue": "x".repeat(600_000)}}}})).collect();
        let batches = commit_batches(big);
        assert!(batches.len() >= 3 && batches.iter().all(|b| b.iter().map(|w| w.to_string().len()).sum::<usize>() <= COMMIT_MAX_BYTES));
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), 7);
        // One write over the limit still goes, alone.
        let huge = commit_batches(vec![json!({"a": "x".repeat(COMMIT_MAX_BYTES + 10)}), json!({"b": 1})]);
        assert_eq!(huge.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1]);
        assert!(commit_batches(Vec::new()).is_empty());
    }

    #[test]
    fn a_row_too_big_to_sync_is_left_out_instead_of_failing_the_batch() {
        let key = [3u8; 32];
        let base = "projects/p/databases/(default)/documents";
        let normal = json!({"t": "m", "type": "movie", "id": "tt1", "meta": {"name": "x"}});
        assert!(fitting_item_write(base, "u", "d1", &normal, &key).unwrap().is_some());
        // Too big, but its synopsis and episode list can go: it syncs without them.
        let bulky = json!({"t": "m", "type": "series", "id": "tt2", "meta": {"name": "x", "description": "d".repeat(300_000), "videos": []}});
        let write = fitting_item_write(base, "u", "d2", &bulky, &key).unwrap().unwrap();
        assert!(write["update"]["fields"]["sealed"]["stringValue"].as_str().unwrap().len() <= MAX_SEALED_BYTES);
        // Nothing to trim: left out.
        let hopeless = json!({"t": "m", "type": "series", "id": "tt3", "blob": "b".repeat(400_000)});
        assert!(fitting_item_write(base, "u", "d3", &hopeless, &key).unwrap().is_none());
        assert!(item_write(base, "u", "d3", &hopeless, &key).is_err());
    }

    #[test]
    fn legacy_documents_are_refused_once_the_key_has_been_verified() {
        let key = [4u8; 32];
        let plain = json!({"t": "m", "type": "movie", "id": "tt1"});
        let legacy_item = json!({"data": {"stringValue": plain.to_string()}});
        assert_eq!(open_item_fields_with(&legacy_item, &key, true), ItemOpen::Legacy(plain.clone()));
        assert_eq!(open_item_fields_with(&legacy_item, &key, false), ItemOpen::Empty);
        let sealed = seal(&key, plain.to_string().as_bytes()).unwrap();
        let v3 = json!({"sealed": {"stringValue": sealed.clone()}, "v": {"integerValue": "3"}});
        let v2 = json!({"sealed": {"stringValue": sealed}, "v": {"integerValue": "2"}});
        assert_eq!(open_item_fields_with(&v3, &key, false), ItemOpen::Ok(plain.clone()));
        assert_eq!(open_item_fields_with(&v2, &key, false), ItemOpen::Empty);
        // The same for the settings document.
        let payload = json!({"addons": {"urls": ["https://a.example/manifest.json"]}});
        let legacy_keys = json!({"data": {"stringValue": payload.to_string()}});
        assert_eq!(open_keys_payload(&key, "u", "p", &legacy_keys, true), KeysOpen::Legacy(payload.clone()));
        assert_eq!(open_keys_payload(&key, "u", "p", &legacy_keys, false), KeysOpen::Empty);
        let sealed_v2 = seal(&legacy_v2_envelope_key("u", "p"), payload.to_string().as_bytes()).unwrap();
        let v2_keys = json!({"sealed": {"stringValue": sealed_v2}, "v": {"integerValue": "2"}});
        assert_eq!(open_keys_payload(&key, "u", "p", &v2_keys, true), KeysOpen::Legacy(payload.clone()));
        assert_eq!(open_keys_payload(&key, "u", "p", &v2_keys, false), KeysOpen::Empty);
        let v3_keys = json!({"sealed": {"stringValue": seal_keys_payload(&key, &payload).unwrap()}, "v": {"integerValue": "3"}});
        assert_eq!(open_keys_payload(&key, "u", "p", &v3_keys, false), KeysOpen::V3(payload));
    }

    #[test]
    fn a_push_never_silently_replaces_a_newer_remote_configuration() {
        // Edited here AND saved elsewhere since the last sync: merge.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: true, local_hash: "h2", last_synced_hash: "h1", last_synced_updated: 100,
            remote_has_keys: true, remote_hash: Some("h3"), remote_updated: 200,
        }), KeysSyncAction::Merge);
        // First sync of a device that has its own settings, against an account that has others: merge.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: true, local_hash: "h2", last_synced_hash: "", last_synced_updated: 0,
            remote_has_keys: true, remote_hash: Some("h3"), remote_updated: 200,
        }), KeysSyncAction::Merge);
        // Edited here, nothing new there: still a plain push.
        assert_eq!(decide_keys_sync(&KeysSyncInput {
            local_has_keys: true, local_hash: "h2", last_synced_hash: "h1", last_synced_updated: 100,
            remote_has_keys: true, remote_hash: Some("h1"), remote_updated: 100,
        }), KeysSyncAction::Push);
        let local = json!({"stream_language": "italian", "addons": {"urls": ["https://a.example/m.json"]},
            "prefs": {"auto_next": false, "sub_scale": 1.2}, "track_prefs": {"tt1": {"audio": "ita"}}});
        let remote = json!({"stream_language": "english", "addons": {"urls": ["https://b.example/m.json", "https://a.example/m.json"]},
            "prefs": {"auto_next": true, "sub_bg": true}, "track_prefs": {"tt2": {"audio": "eng"}}});
        let merged = merge_keys(&local, &remote);
        assert_eq!(merged["stream_language"], "italian");
        assert_eq!(merged["addons"]["urls"], json!(["https://b.example/m.json", "https://a.example/m.json"]), "add-ons from both, once each");
        assert_eq!(merged["prefs"], json!({"auto_next": false, "sub_bg": true, "sub_scale": 1.2}));
        assert!(merged["track_prefs"]["tt1"].is_object() && merged["track_prefs"]["tt2"].is_object());
    }

    #[test]
    fn subtitle_colours_must_be_a_hash_and_six_hex_digits() {
        for ok in ["#ffffff", "#00FF7a", "#123456"] { assert!(valid_sub_color(ok), "{ok}"); }
        for bad in ["ffffff", "#fff", "#ggg000", "#12345", "#1234567", "#12345é", "red", "", "#\u{1F600}ab"] { assert!(!valid_sub_color(bad), "{bad}"); }
    }

    #[test]
    fn credential_too_old_and_sign_out_errors_read_in_the_apps_words() {
        assert!(auth_error("CREDENTIAL_TOO_OLD_LOGIN_AGAIN").contains("sign in again"));
        assert_eq!(auth_error("CREDENTIAL_TOO_OLD_LOGIN_AGAIN : details"), auth_error("CREDENTIAL_TOO_OLD_LOGIN_AGAIN"));
        assert_eq!(auth_error("TOKEN_EXPIRED"), "You were signed out. Sign in again.");
    }

    #[test]
    fn pulled_rows_clamp_future_timestamps_and_detect_create_races() {
        let mut st = State::default();
        let horizon = 2_000_000_300;
        let mut row = json!({"t": "m", "type": "movie", "id": "tt1", "addon_url": "a", "updated": 9_999_999_999i64});
        let key = row_key(&row);
        record_pulled_row(&key, &mut row, false, horizon, &mut st);
        assert_eq!(row["updated"].as_i64(), Some(horizon));
        assert_eq!(st.seen.get(&key), Some(&horizon));
        assert!(st.known.contains(&key));

        let mut legacy = json!({"t": "m", "type": "movie", "id": "tt2", "addon_url": "a", "updated": 9_999_999_999i64});
        let lkey = row_key(&legacy);
        record_pulled_row(&lkey, &mut legacy, true, horizon, &mut st);
        assert_eq!(legacy["updated"].as_i64(), Some(horizon));
        assert!(!st.seen.contains_key(&lkey), "legacy rows stay unmarked so send upgrades them");

        assert!(is_create_precondition_failure(409, &json!({})));
        assert!(is_create_precondition_failure(400, &json!({"error": {"status": "FAILED_PRECONDITION"}})));
        assert!(is_create_precondition_failure(400, &json!({"error": {"status": "ALREADY_EXISTS"}})));
        assert!(!is_create_precondition_failure(400, &json!({"error": {"status": "INVALID_ARGUMENT", "message": "bad field"}})));
    }

    #[test]
    fn failures_back_off_and_hiccups_are_told_from_real_errors() {
        assert_eq!([1, 2, 3, 4, 5, 6, 7, 20].map(backoff_secs), [30, 60, 120, 240, 480, 900, 900, 900]);
        let hiccup = anyhow::Error::new(Transient("Couldn't check for the phone (no connection). Trying again…".into()));
        assert!(is_transient(&hiccup) && !is_transient(&anyhow!("The code didn't match.")));
        assert!(is_transient(&hiccup.context("while waiting")), "still found under context");
    }
}
