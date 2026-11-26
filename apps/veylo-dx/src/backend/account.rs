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
//! and are never logged.
use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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
static LOADED: OnceLock<()> = OnceLock::new();
static LAST_SYNCED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static SYNC_KEY_NEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STATE_LOCK: Mutex<()> = Mutex::new(());
static SYNC_KEY_LOCK: Mutex<()> = Mutex::new(());
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static FORGOTTEN: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn session_path() -> PathBuf { backend().data_dir.join("account.json") }
fn sync_key_path() -> PathBuf { backend().data_dir.join("sync-key.json") }

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

fn load_sync_key() -> Option<[u8; 32]> {
    let v: Value = std::fs::read_to_string(sync_key_path()).ok().and_then(|t| serde_json::from_str(&t).ok())?;
    parse_recovery_code(v["key"].as_str()?).ok()
}

fn save_sync_key(key: &[u8; 32]) {
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
    let data = json!({"v": 3, "key": encoded}).to_string();
    let _ = write_private_file(&sync_key_path(), data.as_bytes());
}

fn ensure_sync_key() -> Result<[u8; 32]> {
    let _guard = SYNC_KEY_LOCK.lock();
    if let Some(k) = load_sync_key() { return Ok(k); }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|_| anyhow!("Couldn't generate an encryption key"))?;
    save_sync_key(&key);
    Ok(key)
}

/// The current device's E2EE Recovery Key (generated on first use when signed in).
pub fn sync_recovery_code() -> Option<String> {
    account()?;
    ensure_sync_key().ok().map(|k| format_recovery_code(&k))
}

/// True when the account has v3 encrypted cloud data that this device's current key couldn't open.
pub fn sync_key_needed() -> bool {
    SYNC_KEY_NEEDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Replace this device's local Sync Key with a Recovery Key from another device and trigger sync.
pub async fn set_sync_recovery_code(code: &str) -> Result<(usize, usize)> {
    let key = parse_recovery_code(code)?;
    let _one = SYNCING.lock().await;
    {
        let _k = SYNC_KEY_LOCK.lock();
        save_sync_key(&key);
    }
    SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
    // Reset sync cursors atomically while holding SYNCING so encrypted cloud items and keys are re-read with the new key.
    {
        let _guard = STATE_LOCK.lock();
        let mut st = load_state_unlocked();
        st.taken_until = None;
        st.keys_hash.clear();
        st.keys_updated = 0;
        save_state_unlocked(&st);
    }
    sync_now_locked().await
}

fn load_session() {
    LOADED.get_or_init(|| {
        let v: Value = std::fs::read_to_string(session_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let refresh = s(&v, "refresh");
        if !refresh.is_empty() && config().is_some() {
            let account = Account { uid: s(&v, "uid"), email: s(&v, "email"), name: s(&v, "name"), provider: s(&v, "provider") };
            if let Ok(mut slot) = SESSION.lock() { *slot = Some(Session { account, refresh, ..Session::default() }); }
            LAST_SYNCED.store(load_state().last, std::sync::atomic::Ordering::Relaxed);
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
        None => {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(state_path());
            let _ = std::fs::remove_file(sync_key_path());
        }
    }
}

/// The signed-in account, if any.
pub fn account() -> Option<Account> {
    load_session();
    SESSION.lock().ok()?.as_ref().map(|x| x.account.clone())
}

fn set_session(session: Session) -> Account {
    save_session(Some(&session));
    let account = session.account.clone();
    if let Ok(mut slot) = SESSION.lock() { *slot = Some(session); }
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
        "OPERATION_NOT_ALLOWED" | "CONFIGURATION_NOT_FOUND" => "This sign-in method isn't turned on for Veylo's accounts yet.",
        _ => "Couldn't reach Veylo's accounts. Check your connection and try again.",
    }.to_owned()
}

async fn post(url: String, body: Value, token: Option<String>) -> Result<Value> {
    on_runtime(async move {
        let mut req = backend().http.post(url).json(&body).timeout(Duration::from_secs(20));
        if let Some(t) = token { req = req.bearer_auth(t); }
        let response = req.send().await.map_err(|_| anyhow!(auth_error("")))?;
        let ok = response.status().is_success();
        let v: Value = response.json().await.unwrap_or_default();
        if ok { Ok(v) } else { Err(anyhow!("{}", v["error"]["message"].as_str().or(v[0]["error"]["message"].as_str()).unwrap_or("").to_owned())) }
    }).await?
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
            .form(&[("grant_type", "refresh_token"), ("refresh_token", refresh.as_str())]).timeout(Duration::from_secs(20)).send().await?;
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
                let same_session = SESSION.lock().ok().and_then(|s| s.as_ref().map(|x| x.account.uid == expected_uid && x.refresh == current.refresh)).unwrap_or(false);
                if same_session { forget_session(); }
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
    let updated = if let Ok(mut slot) = SESSION.lock() {
        let still_same = slot.as_ref().is_some_and(|s| s.account.uid == expected_uid && s.refresh == current.refresh);
        if still_same { *slot = Some(next.clone()); true } else { false }
    } else { false };
    anyhow::ensure!(updated, "Not signed in");
    save_session(Some(&next));
    Ok(out)
}

fn forget_session() {
    if let Ok(mut slot) = SESSION.lock() { *slot = None; }
    if let Ok(mut f) = FORGOTTEN.lock() { f.clear(); }
    LAST_SYNCED.store(0, std::sync::atomic::Ordering::Relaxed);
    SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
    save_session(None);
}

/// Sign out on this device (the account and the other devices stay).
pub fn sign_out() { forget_session(); }

/// Delete the account and everything synced to it, then sign out. This device's library stays.
pub async fn delete_account() -> Result<()> {
    let (token, uid) = id_token().await?;
    // The synced titles first: a deleted account can't reach them any more.
    loop {
        let docs = list_items(&token, &uid, None, 300).await?;
        if docs.is_empty() { break; }
        let writes: Vec<Value> = docs.iter().map(|(name, _)| json!({"delete": name})).collect();
        commit(&token, writes).await?;
    }
    if let Some(base) = documents() {
        // Synced keys & add-ons config document.
        let _ = commit(&token, vec![json!({"delete": format!("{base}/users/{uid}/config/keys")})]).await;
        // Any legacy key handoff requests and diagnostic reports, paginated to completion.
        for coll in ["handoff", "diagnostics"] {
            loop {
                let list = get_doc(&token, format!("{base}/users/{uid}/{coll}?pageSize=300")).await.ok().flatten().unwrap_or_default();
                let writes: Vec<Value> = list["documents"].as_array().into_iter().flatten().filter_map(|d| d["name"].as_str()).map(|n| json!({"delete": n})).collect();
                if writes.is_empty() { break; }
                commit(&token, writes).await?;
            }
        }
    }
    identity("delete", json!({"idToken":token})).await?;
    forget_session();
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

async fn commit(token: &str, writes: Vec<Value>) -> Result<()> {
    let base = documents().context("Accounts aren't set up in this build")?;
    // Keep batches strictly below list_items' 300-doc page size so a batch sharing one server
    // REQUEST_TIME never spans across a GREATER_THAN pagination boundary.
    for chunk in writes.chunks(200) {
        post(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":chunk}), Some(token.to_owned())).await?;
    }
    Ok(())
}

/// The account's synced documents changed after `since` (the server's time), oldest first:
/// (document name, its fields).
async fn list_items(token: &str, uid: &str, since: Option<&str>, limit: usize) -> Result<Vec<(String, Value)>> {
    let base = documents().context("Accounts aren't set up in this build")?;
    let mut query = json!({"from":[{"collectionId":"items"}],"orderBy":[{"field":{"fieldPath":"synced"}}],"limit":limit});
    if let Some(t) = since {
        query["where"] = json!({"fieldFilter":{"field":{"fieldPath":"synced"},"op":"GREATER_THAN","value":{"timestampValue":t}}});
    }
    let v = post(format!("https://firestore.googleapis.com/v1/{base}/users/{uid}:runQuery"), json!({"structuredQuery":query}), Some(token.to_owned())).await?;
    Ok(v.as_array().into_iter().flatten().filter_map(|r| {
        let doc = &r["document"];
        Some((doc["name"].as_str()?.to_owned(), doc["fields"].clone()))
    }).collect())
}

/// A synced row as an end-to-end encrypted document write (AES-256-GCM with the user's 256-bit
/// Sync Key), plus the server's timestamp.
fn item_write(base: &str, uid: &str, id: &str, data: &Value, sync_key: &[u8; 32]) -> Result<Value> {
    let sealed = seal(sync_key, data.to_string().as_bytes())?;
    Ok(json!({
        "update": {
            "name": format!("{base}/users/{uid}/items/{id}"),
            "fields": {
                "sealed": {"stringValue": sealed},
                "v": {"integerValue": "3"},
            }
        },
        "updateTransforms": [{"fieldPath": "synced", "setToServerValue": "REQUEST_TIME"}]
    }))
}

#[derive(Debug, PartialEq)]
enum ItemOpen { Ok(Value), WrongKey, Empty }

fn open_item_fields(fields: &Value, sync_key: &[u8; 32]) -> ItemOpen {
    if let Some(sealed) = fields["sealed"]["stringValue"].as_str() {
        return match open_sealed(sync_key, sealed).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            Some(v) => ItemOpen::Ok(v),
            None => ItemOpen::WrongKey,
        };
    }
    // Seamless migration fallback for legacy plaintext item docs.
    match fields["data"]["stringValue"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        Some(v) => ItemOpen::Ok(v),
        None => ItemOpen::Empty,
    }
}

fn doc_id(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes()))[..40].to_owned() }

// ---------------------------------------------------------------- sync

/// Where sync is up to: the last local change sent (seconds, this device's clock), the last
/// server change taken, the (key, time) of rows exchanged lately so none is sent back, and the
/// hash/timestamp of the last keys & add-ons snapshot synced with the account.
#[derive(Default)]
struct State {
    sent_until: i64,
    taken_until: Option<String>,
    seen: BTreeMap<String, i64>,
    known: std::collections::BTreeSet<String>,
    forgotten: Vec<Value>,
    last: i64,
    keys_hash: String,
    keys_updated: i64,
}

fn state_path() -> PathBuf { backend().data_dir.join("sync-state.json") }

fn load_state_unlocked() -> State {
    let v: Value = std::fs::read_to_string(state_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let last = v["last"].as_i64().unwrap_or(0);
    LAST_SYNCED.store(last, std::sync::atomic::Ordering::Relaxed);
    State {
        sent_until: v["sent_until"].as_i64().unwrap_or(0),
        taken_until: v["taken_until"].as_str().map(str::to_owned),
        seen: v["seen"].as_object().map(|o| o.iter().filter_map(|(k, t)| Some((k.clone(), t.as_i64()?))).collect()).unwrap_or_default(),
        known: v["known"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        forgotten: v["forgotten"].as_array().cloned().unwrap_or_default(),
        last,
        keys_hash: s(&v, "keys_hash"),
        keys_updated: v["keys_updated"].as_i64().unwrap_or(0),
    }
}

fn load_state() -> State {
    let _guard = STATE_LOCK.lock();
    load_state_unlocked()
}

fn save_state_unlocked(st: &State) {
    // Only the newest exchanged rows are worth remembering.
    let mut seen: Vec<(&String, &i64)> = st.seen.iter().collect();
    seen.sort_by_key(|(_, t)| std::cmp::Reverse(**t));
    let seen: serde_json::Map<String, Value> = seen.into_iter().take(5000).map(|(k, t)| (k.clone(), json!(t))).collect();
    // Merge any tombstones queued while a sync was in flight so they are never clobbered.
    let mut forgotten = st.forgotten.clone();
    if let Ok(pending) = FORGOTTEN.lock() {
        for item in pending.iter() {
            if !forgotten.contains(item) { forgotten.push(item.clone()); }
        }
    }
    LAST_SYNCED.store(st.last, std::sync::atomic::Ordering::Relaxed);
    let data = json!({
        "sent_until": st.sent_until,
        "taken_until": st.taken_until,
        "seen": seen,
        "known": st.known,
        "forgotten": forgotten,
        "last": st.last,
        "keys_hash": st.keys_hash,
        "keys_updated": st.keys_updated,
    }).to_string();
    let _ = write_private_file(&state_path(), data.as_bytes());
}

fn save_state(st: &State) {
    let _guard = STATE_LOCK.lock();
    save_state_unlocked(st);
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

async fn sync_now_locked() -> Result<(usize, usize)> {
    let (token, uid) = id_token().await?;
    let base = documents().context("Accounts aren't set up in this build")?;
    let sync_key = ensure_sync_key()?;
    let mut st = load_state();
    // Take: every document the server changed since the last time, in pages.
    let mut taken = 0;
    let mut wrong_key_seen = false;
    loop {
        let docs = list_items(&token, &uid, st.taken_until.as_deref(), 300).await?;
        if docs.is_empty() { break; }
        let (mut media, mut playback) = (Vec::new(), Vec::new());
        for (_, fields) in &docs {
            let row = match open_item_fields(fields, &sync_key) {
                ItemOpen::Ok(row) => row,
                ItemOpen::WrongKey => { wrong_key_seen = true; continue; }
                ItemOpen::Empty => continue,
            };
            let key = row_key(&row);
            st.known.insert(key.clone());
            if let Some(t) = row["updated"].as_i64() { st.seen.insert(key.clone(), t); }
            if key.starts_with("p|") { playback.push(row) } else { media.push(row) }
        }
        if wrong_key_seen {
            SYNC_KEY_NEEDED.store(true, std::sync::atomic::Ordering::Relaxed);
            anyhow::bail!("Your synced data is encrypted with another device's Recovery Key. Enter it under Settings → Account.");
        }
        taken += docs.len();
        exec(json!({"op":"sync_apply","media":media,"playback":playback})).await?;
        st.taken_until = docs.last().and_then(|(_, f)| f["synced"]["timestampValue"].as_str().map(str::to_owned));
        save_state(&st);
        if docs.len() < 300 { break; }
    }
    // Sync encrypted keys & add-ons before pushing so we detect a mismatched key before overwriting.
    // On a first unverified sync (empty keys_hash), abort rather than pushing items encrypted under
    // an unverified local key.
    if let Err(e) = sync_keys(&token, &uid, &base, &sync_key, &mut st).await {
        if sync_key_needed() || st.keys_hash.is_empty() { return Err(e); }
        dev_log(&format!("keys: {e}"));
    } else {
        SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    // Send: what changed here since the last time, but not what just came from elsewhere.
    let changes = exec(json!({"op":"sync_changes","since":st.sent_until})).await?;
    let mut writes = Vec::new();
    let mut newest = st.sent_until;
    for row in changes["media"].as_array().into_iter().flatten().chain(changes["playback"].as_array().into_iter().flatten()) {
        let key = row_key(row);
        let updated = row["updated"].as_i64().unwrap_or(0);
        newest = newest.max(updated);
        if st.seen.get(&key) == Some(&updated) { continue; }
        // A catalogue row with no history goes only where the account has it (it was unsaved).
        if row["history"] == false && !st.known.contains(&key) { continue; }
        let mut data = row.clone();
        if let Some(o) = data.as_object_mut() { o.remove("key"); o.remove("history"); }
        data["t"] = json!(if key.starts_with("p|") { "p" } else { "m" });
        if data.get("meta").is_some() { data["meta"] = slim_meta(&row["meta"]); }
        writes.push(item_write(&base, &uid, &doc_id(&key), &data, &sync_key)?);
        st.seen.insert(key.clone(), updated);
        st.known.insert(key);
    }
    // Drain both persisted and concurrently queued tombstones; restore on commit failure.
    let mut pending_forgotten = std::mem::take(&mut st.forgotten);
    if let Ok(mut queued) = FORGOTTEN.lock() {
        for item in queued.drain(..) {
            if !pending_forgotten.contains(&item) { pending_forgotten.push(item); }
        }
    }
    for gone in &pending_forgotten {
        let key = row_key(gone);
        writes.push(item_write(&base, &uid, &doc_id(&key), gone, &sync_key)?);
    }
    let sent = writes.len();
    if let Err(e) = commit(&token, writes).await {
        if let Ok(mut queued) = FORGOTTEN.lock() {
            for item in pending_forgotten {
                if !queued.contains(&item) { queued.push(item); }
            }
        }
        return Err(e);
    }
    // A second's changes may still be coming: the next sync looks at that second again.
    st.sent_until = (newest - 1).max(st.sent_until);
    st.last = now_secs();
    save_state(&st);
    Ok((taken, sent))
}

/// A title forgotten here is forgotten on the other devices too.
pub fn sync_forgotten(media: &Media) {
    if account().is_none() { return; }
    let entry = json!({"t":"m","type":media.kind,"id":media.id,"addon_url":media.addon_url,"updated":now_secs(),"deleted":true});
    if let Ok(mut queued) = FORGOTTEN.lock() { queued.push(entry); }
    // Persist immediately under STATE_LOCK so a crash before the next sync keeps the tombstone
    // without racing against load/save_state in sync_now.
    {
        let _guard = STATE_LOCK.lock();
        let st = load_state_unlocked();
        save_state_unlocked(&st);
    }
    sync_soon();
}

static WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sync a few seconds from now (after a change here).
pub fn sync_soon() { WANTED.store(true, std::sync::atomic::Ordering::Relaxed); }

/// While signed in: sync at launch, a few seconds after each change here, and every two minutes.
pub fn start_sync() {
    if account().is_none() { return; }
    let Some(runtime) = RUNTIME.get() else { return };
    if STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) { return; }
    sync_soon();
    runtime.spawn(async {
        let mut idle = 0u32;
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
            if WANTED.swap(false, std::sync::atomic::Ordering::Relaxed) || idle >= 24 {
                idle = 0;
                match sync_now().await {
                    Ok((took, sent)) if took + sent > 0 => dev_log(&format!("sync: took {took}, sent {sent}")),
                    Ok(_) => {}
                    Err(e) => dev_log(&format!("sync: {e}")),
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

async fn get_doc(token: &str, path: String) -> Result<Option<Value>> {
    let token = token.to_owned();
    on_runtime(async move {
        let r = backend().http.get(format!("https://firestore.googleapis.com/v1/{path}")).bearer_auth(token).timeout(Duration::from_secs(15)).send().await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND { return Ok(None) }
        anyhow::ensure!(r.status().is_success(), "Firestore answered {}", r.status());
        Ok(Some(r.json::<Value>().await?))
    }).await?
}

fn keys_digest(payload: &Value) -> String {
    format!("{:x}", Sha256::digest(payload.to_string().as_bytes()))
}

fn has_any_sync_config(payload: &Value) -> bool {
    payload["addons"]["urls"].as_array().is_some_and(|a| !a.is_empty())
        || payload["addons"]["others"].as_array().is_some_and(|a| !a.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
enum KeysSyncAction { Adopt, Push, KeepSynced, Noop }

struct KeysSyncInput<'a> {
    local_has_keys: bool,
    local_hash: &'a str,
    last_synced_hash: &'a str,
    last_synced_updated: i64,
    remote_has_keys: bool,
    remote_hash: Option<&'a str>,
    remote_updated: i64,
}

/// Decide whether this device should pull remote config, push local config, or do nothing.
fn decide_keys_sync(i: &KeysSyncInput<'_>) -> KeysSyncAction {
    let local_changed = !i.last_synced_hash.is_empty() && i.local_hash != i.last_synced_hash;
    // A newly signed-in device without keys/add-ons adopts the account's config automatically.
    if !i.local_has_keys && i.remote_has_keys && !local_changed {
        return KeysSyncAction::Adopt;
    }
    // Local settings or add-ons were edited on this device since last sync: push them.
    if local_changed {
        return KeysSyncAction::Push;
    }
    // First sync on a device that already has keys/add-ons configured.
    if i.last_synced_hash.is_empty() && i.local_has_keys {
        return match i.remote_hash {
            Some(rh) if rh == i.local_hash => KeysSyncAction::KeepSynced,
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

fn open_keys_payload(sync_key: &[u8; 32], uid: &str, project: &str, fields: &Value) -> KeysOpen {
    let ver = fields["v"]["integerValue"].as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(1);
    if let Some(sealed) = fields["sealed"]["stringValue"].as_str() {
        if ver >= 3 {
            return match open_sealed(sync_key, sealed).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                Some(v) => KeysOpen::V3(v),
                None => KeysOpen::WrongKey,
            };
        }
        // Legacy v2 doc: migrate cleanly to v3 E2EE.
        let legacy_key = legacy_v2_envelope_key(uid, project);
        if let Some(v) = open_sealed(&legacy_key, sealed).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            return KeysOpen::Legacy(v);
        }
        return KeysOpen::Empty;
    }
    // Legacy v1 plaintext doc: migrate cleanly to v3 E2EE.
    match fields["data"]["stringValue"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        Some(v) => KeysOpen::Legacy(v),
        None => KeysOpen::Empty,
    }
}

/// Automatically sync audio language, preferences and add-ons through
/// `users/{uid}/config/keys` using real zero-knowledge v3 AES-256-GCM encryption.
async fn sync_keys(token: &str, uid: &str, base: &str, sync_key: &[u8; 32], st: &mut State) -> Result<()> {
    let project = config().map(|c| c.project).unwrap_or_default();
    let settings = load_settings();
    let local_addons = crate::addons::for_handoff().await;
    let local = json!({
        "stream_language": settings.stream_language,
        "addons": local_addons,
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
    });
    let local_hash = keys_digest(&local);
    let local_has_keys = has_any_sync_config(&local);

    let doc_path = format!("{base}/users/{uid}/config/keys");
    let remote_doc = get_doc(token, doc_path.clone()).await?;
    let remote_fields = remote_doc.as_ref().map(|d| &d["fields"]);
    let (remote_data, remote_is_legacy) = match remote_fields.map(|f| open_keys_payload(sync_key, uid, project, f)) {
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

    match action {
        KeysSyncAction::Adopt => {
            if let Some(remote) = remote_data {
                adopt_keys(&remote).await;
                let after = load_settings();
                let adopted = json!({
                    "stream_language": after.stream_language,
                    "addons": crate::addons::for_handoff().await,
                    "prefs": {
                        "better_posters": after.better_posters,
                        "picks_hide_watched": after.picks_hide_watched,
                        "sub_scale": after.sub_scale,
                        "sub_pos": after.sub_pos,
                        "sub_bg": after.sub_bg,
                        "sub_color": after.sub_color,
                        "audio_night_mode": after.audio_night_mode,
                        "audio_passthrough": after.audio_passthrough,
                        "auto_next": after.auto_next,
                        "max_bitrate_mbps": after.max_bitrate_mbps,
                    },
                    "track_prefs": export_track_prefs_value(),
                });
                st.keys_hash = keys_digest(&adopted);
                st.keys_updated = remote_updated;
                if has_any_sync_config(&remote) {
                    if let Ok(mut a) = ARRIVED.lock() { *a = Some(remote_device); }
                }
                dev_log("keys: synced automatically from account");
            }
        }
        KeysSyncAction::Push => {
            let updated = now_secs().max(remote_updated + 1);
            let sealed = seal_keys_payload(sync_key, &local)?;
            commit(token, vec![json!({
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
            })]).await?;
            st.keys_hash = local_hash;
            st.keys_updated = updated;
            dev_log("keys: saved (E2EE v3) to account");
        }
        KeysSyncAction::KeepSynced => {
            st.keys_hash = local_hash;
            st.keys_updated = remote_updated;
        }
        KeysSyncAction::Noop => {}
    }
    Ok(())
}

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
            if let Some(c) = p["sub_color"].as_str().filter(|c| c.starts_with('#') && c.len() == 7) { s.sub_color = c.to_owned(); }
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

/// Show a QR code to scan with a signed-in phone or tablet: this computer signs in for a moment
/// without an account and leaves a request only it (and the phone holding the code) can answer.
pub async fn offer_link() -> Result<LinkOffer> {
    let v = identity("signUp", json!({"returnSecureToken":true})).await?;
    let (token, uid) = (s(&v, "idToken"), s(&v, "localId"));
    let base = documents().context("Accounts aren't set up in this build")?;
    let link = random_text(18).replace(['-', '_'], "x");
    let mut key = vec![0u8; 32];
    getrandom::fill(&mut key).map_err(|_| anyhow!("No randomness"))?;
    post(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":[{
        "update":{"name":format!("{base}/links/{link}"),"fields":{"asker":{"stringValue":uid}}},
        "updateTransforms":[{"fieldPath":"created","setToServerValue":"REQUEST_TIME"}],
        "currentDocument":{"exists":false}}]}), Some(token.clone())).await?;
    let code = format!("veylo-link:1:{link}:{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&key));
    Ok(LinkOffer { code, link, key, token })
}

/// Wait (polling) until the phone answers the link; then this computer is signed in as its
/// account. None while unanswered.
pub async fn take_link(offer: &LinkOffer) -> Result<Option<Account>> {
    let base = documents().context("Accounts aren't set up in this build")?;
    let (url, token) = (format!("https://firestore.googleapis.com/v1/{base}/links/{}", offer.link), offer.token.clone());
    let doc: Value = on_runtime(async move {
        backend().http.get(url).bearer_auth(token).timeout(Duration::from_secs(15)).send().await?.json::<Value>().await.map_err(anyhow::Error::from)
    }).await??;
    let Some(sealed) = doc["fields"]["sealed"]["stringValue"].as_str() else { return Ok(None) };
    let opened = open_sealed(&offer.key, sealed).context("The code didn't match. Show a new one and scan it again.")?;
    let v: Value = serde_json::from_slice(&opened)?;
    // The request goes, and so does the passing account the computer waited with.
    let _ = post(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":[{"delete":format!("{base}/links/{}", offer.link)}]}), Some(offer.token.clone())).await;
    let _ = identity("delete", json!({"idToken":offer.token})).await;
    let session = Session {
        account: Account { uid: s(&v, "uid"), email: s(&v, "email"), name: s(&v, "name"), provider: s(&v, "provider") },
        refresh: s(&v, "refresh"), ..Session::default()
    };
    anyhow::ensure!(!session.refresh.is_empty(), "The code didn't match. Show a new one and scan it again.");
    // Transfer the E2EE Sync Key before starting sync so encrypted history & add-ons open immediately.
    if let Some(code) = v["sync_key"].as_str() {
        if let Ok(k) = parse_recovery_code(code) {
            save_sync_key(&k);
            SYNC_KEY_NEEDED.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if v["keys"].is_object() { adopt_keys(&v["keys"]).await; }
    let account = set_session(session);
    id_token().await?;
    Ok(Some(account))
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
/// The phone's half: answer a computer's QR code with this account's session and E2EE Sync Key,
/// sealed with the code's ephemeral key (only that computer can open it).
pub async fn answer_link(code: &str) -> Result<()> {
    let parts: Vec<&str> = code.trim().split(':').collect();
    let ["veylo-link", "1", link, key] = parts[..] else { anyhow::bail!("That isn't a Veylo sign-in code.") };
    anyhow::ensure!(link.len() <= 64 && link.bytes().all(|b| b.is_ascii_alphanumeric()), "That isn't a Veylo sign-in code.");
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(key).ok().filter(|k| k.len() == 32).context("That isn't a Veylo sign-in code.")?;
    let (token, _) = id_token().await?;
    let session = SESSION.lock().ok().and_then(|x| x.clone()).context("Not signed in")?;
    let sync_key = ensure_sync_key()?;
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
    post(format!("https://firestore.googleapis.com/v1/{base}:commit"), json!({"writes":[{
        "update":{"name":format!("{base}/links/{link}"),"fields":{"sealed":{"stringValue":sealed}}},
        "updateMask":{"fieldPaths":["sealed"]},"currentDocument":{"exists":true}}]}), Some(token)).await
        .map_err(|_| anyhow!("That code has expired. Show a new one on the computer."))?;
    Ok(())
}

/// Android: scan a computer's QR code with Google's scanner and answer it.
#[cfg(target_os = "android")]
pub async fn scan_and_link() -> Result<()> {
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
    answer_link(&code).await
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
        assert_eq!(open_keys_payload(&key_a, "uid-alice", "veylo-proj", &fields), KeysOpen::V3(payload.clone()));
        assert_eq!(open_keys_payload(&key_b, "uid-alice", "veylo-proj", &fields), KeysOpen::WrongKey);
        let deterministic = legacy_v2_envelope_key("uid-alice", "veylo-proj");
        assert_eq!(open_keys_payload(&deterministic, "uid-alice", "veylo-proj", &fields), KeysOpen::WrongKey);

        // Legacy v1 plaintext documents remain readable once for migration.
        let legacy = json!({"data": {"stringValue": payload.to_string()}});
        assert_eq!(open_keys_payload(&key_a, "uid-alice", "veylo-proj", &legacy), KeysOpen::Legacy(payload));

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
            let docs = list_items(&token, &uid, None, 50).await.unwrap();
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
            let other = list_items(&offer.token, &uid, None, 5).await;
            assert!(other.is_err(), "an anonymous session must not read an account's items");
            delete_account().await.unwrap();
            assert!(account().is_none());
            println!("deleted");
        });
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
}
