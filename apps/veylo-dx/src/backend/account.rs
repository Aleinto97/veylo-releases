//! Your Veylo account and the sync between your devices, through Firebase (Authentication and
//! Firestore, called over REST). Every sign-in happens in the app:
//! - email and password;
//! - Google: Android's own account sheet (VeyloAccount.kt) on Android, the browser on a computer;
//! - a computer linked from a signed-in phone or tablet: the computer shows a QR code, the phone
//!   scans it and hands the computer its session, sealed with the key in that code.
//!
//! What syncs: titles with history (saved, watched, rated, progress) and each episode's record,
//! as the core lists them (`sync_changes`), newest change winning (`sync_apply`). Each document
//! carries the server's time, so a device whose clock is off still sees every change.
//! The session (its refresh token) is kept in the app's private folder, like settings.json; it
//! is never logged.
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
    /// Google's "web" client: Android's account sheet asks for a token for it.
    pub web_client: Option<&'static str>,
    /// Google's "desktop" client: the browser sign-in on a computer.
    pub desktop_client: Option<(&'static str, &'static str)>,
}

pub fn config() -> Option<Config> {
    let set = |v: Option<&'static str>| v.filter(|v| !v.trim().is_empty());
    Some(Config {
        api_key: set(option_env!("VEYLO_FIREBASE_API_KEY"))?,
        project: set(option_env!("VEYLO_FIREBASE_PROJECT"))?,
        web_client: set(option_env!("VEYLO_GOOGLE_WEB_CLIENT")),
        desktop_client: set(option_env!("VEYLO_GOOGLE_DESKTOP_CLIENT")).zip(set(option_env!("VEYLO_GOOGLE_DESKTOP_SECRET"))),
    })
}

/// Whether this build can sign in at all.
pub fn accounts_available() -> bool { config().is_some() }

/// Google sign-in this device offers: Android's account sheet, or the browser on a computer.
pub fn google_available() -> bool {
    config().is_some_and(|c| if cfg!(target_os = "android") { c.web_client.is_some() } else { c.desktop_client.is_some() })
}

// ---------------------------------------------------------------- session

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Account { pub uid: String, pub email: String, pub name: String, pub provider: String }

#[derive(Clone, Default)]
struct Session { account: Account, refresh: String, id_token: String, expires: i64 }

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static LOADED: OnceLock<()> = OnceLock::new();

fn session_path() -> PathBuf { backend().data_dir.join("account.json") }

fn load_session() {
    LOADED.get_or_init(|| {
        let v: Value = std::fs::read_to_string(session_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let refresh = s(&v, "refresh");
        if !refresh.is_empty() && config().is_some() {
            let account = Account { uid: s(&v, "uid"), email: s(&v, "email"), name: s(&v, "name"), provider: s(&v, "provider") };
            if let Ok(mut slot) = SESSION.lock() { *slot = Some(Session { account, refresh, ..Session::default() }); }
        }
    });
}

fn save_session(session: Option<&Session>) {
    let path = session_path();
    match session {
        Some(x) => {
            let a = &x.account;
            let _ = std::fs::write(&path, json!({"uid":a.uid,"email":a.email,"name":a.name,"provider":a.provider,"refresh":x.refresh}).to_string());
        }
        None => { let _ = std::fs::remove_file(&path); let _ = std::fs::remove_file(state_path()); }
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
    let v = identity("signInWithIdp", json!({"postBody":format!("id_token={id_token}&providerId=google.com"),
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
    loop {
        sleep(250).await;
        match crate::android::account_result() {
            Some(r) if r.starts_with("token:") => return Ok(r["token:".len()..].to_owned()),
            Some(r) if r.starts_with("cancel:") => anyhow::bail!("Sign-in cancelled."),
            Some(r) if r.starts_with("error:") => anyhow::bail!("Google sign-in didn't work: {}", &r["error:".len()..]),
            _ => {}
        }
    }
}

/// The installed-app flow: Google's page in the browser, its answer to a one-off local address
/// (127.0.0.1), the code exchanged with a PKCE verifier.
#[cfg(not(target_os = "android"))]
async fn google_id_token() -> Result<String> {
    let (client, secret) = config().and_then(|c| c.desktop_client).context("Google sign-in isn't set up in this build")?;
    on_runtime(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let verifier = random_text(48);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_text(16);
        let url = url::Url::parse_with_params("https://accounts.google.com/o/oauth2/v2/auth", &[("client_id", client), ("redirect_uri", &redirect),
            ("response_type", "code"), ("scope", "openid email profile"), ("code_challenge", &challenge), ("code_challenge_method", "S256"),
            ("state", &state), ("prompt", "select_account")])?;
        crate::open_external(url.as_str());
        let code = tokio::time::timeout(Duration::from_secs(300), async {
            loop {
                let (mut stream, _) = listener.accept().await?;
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let Ok(answer) = url::Url::parse(&format!("http://127.0.0.1{path}")) else { continue };
                let q: HashMap<String, String> = answer.query_pairs().into_owned().collect();
                if q.get("state") != Some(&state) { continue; }
                let page = if q.contains_key("code") { "You're signed in. You can close this tab and go back to Veylo." } else { "Sign-in was cancelled. You can close this tab." };
                let body = format!("<!doctype html><meta charset=utf-8><title>Veylo</title><body style=\"font:18px system-ui;background:#111;color:#eee;display:grid;place-items:center;height:90vh\">{page}</body>");
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                return q.get("code").cloned().context("Sign-in cancelled.");
            }
        }).await.map_err(|_| anyhow!("Sign-in took too long. Try again."))??;
        let v: Value = backend().http.post("https://oauth2.googleapis.com/token")
            .form(&[("code", code.as_str()), ("client_id", client), ("client_secret", secret), ("redirect_uri", &redirect),
                ("grant_type", "authorization_code"), ("code_verifier", &verifier)])
            .send().await?.json().await?;
        Ok::<_, anyhow::Error>(s(&v, "id_token")).and_then(|t| if t.is_empty() { Err(anyhow!("Google didn't sign you in. Try again.")) } else { Ok(t) })
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
    let c = config().context("Accounts aren't set up in this build")?;
    let refresh = current.refresh.clone();
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
            // Refused for good (the account was deleted, or signed out everywhere): signed out here.
            let code = e.to_string();
            if ["TOKEN_EXPIRED", "USER_NOT_FOUND", "INVALID_REFRESH_TOKEN", "USER_DISABLED"].iter().any(|c| code.starts_with(c)) { forget_session(); }
            anyhow::bail!(auth_error(&code));
        }
    };
    let mut next = current;
    next.id_token = s(&v, "id_token");
    next.refresh = Some(s(&v, "refresh_token")).filter(|r| !r.is_empty()).unwrap_or(next.refresh);
    next.expires = now_secs() + v["expires_in"].as_str().and_then(|x| x.parse().ok()).unwrap_or(3600) - 60;
    let out = (next.id_token.clone(), next.account.uid.clone());
    save_session(Some(&next));
    if let Ok(mut slot) = SESSION.lock() { *slot = Some(next); }
    Ok(out)
}

fn forget_session() {
    if let Ok(mut slot) = SESSION.lock() { *slot = None; }
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
    identity("delete", json!({"idToken":token})).await?;
    forget_session();
    Ok(())
}

// ---------------------------------------------------------------- Firestore

fn documents() -> Option<String> {
    config().map(|c| format!("projects/{}/databases/(default)/documents", c.project))
}

async fn commit(token: &str, writes: Vec<Value>) -> Result<()> {
    let base = documents().context("Accounts aren't set up in this build")?;
    for chunk in writes.chunks(400) {
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

/// A synced row as a document write: its data as JSON text, and the server's time.
fn item_write(base: &str, uid: &str, id: &str, data: &Value) -> Value {
    json!({"update":{"name":format!("{base}/users/{uid}/items/{id}"),"fields":{"data":{"stringValue":data.to_string()}}},
        "updateTransforms":[{"fieldPath":"synced","setToServerValue":"REQUEST_TIME"}]})
}

fn doc_id(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes()))[..40].to_owned() }

// ---------------------------------------------------------------- sync

/// Where sync is up to: the last local change sent (seconds, this device's clock), the last
/// server change taken, and the (key, time) of rows exchanged lately so none is sent back.
#[derive(Default)]
struct State { sent_until: i64, taken_until: Option<String>, seen: BTreeMap<String, i64>, known: std::collections::BTreeSet<String>, forgotten: Vec<Value>, last: i64 }

fn state_path() -> PathBuf { backend().data_dir.join("sync-state.json") }

fn load_state() -> State {
    let v: Value = std::fs::read_to_string(state_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    State {
        sent_until: v["sent_until"].as_i64().unwrap_or(0),
        taken_until: v["taken_until"].as_str().map(str::to_owned),
        seen: v["seen"].as_object().map(|o| o.iter().filter_map(|(k, t)| Some((k.clone(), t.as_i64()?))).collect()).unwrap_or_default(),
        known: v["known"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        forgotten: v["forgotten"].as_array().cloned().unwrap_or_default(),
        last: v["last"].as_i64().unwrap_or(0),
    }
}

fn save_state(st: &State) {
    // Only the newest exchanged rows are worth remembering.
    let mut seen: Vec<(&String, &i64)> = st.seen.iter().collect();
    seen.sort_by_key(|(_, t)| std::cmp::Reverse(**t));
    let seen: serde_json::Map<String, Value> = seen.into_iter().take(5000).map(|(k, t)| (k.clone(), json!(t))).collect();
    let _ = std::fs::write(state_path(), json!({"sent_until":st.sent_until,"taken_until":st.taken_until,"seen":seen,
        "known":st.known,"forgotten":st.forgotten,"last":st.last}).to_string());
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
pub fn last_synced() -> i64 { if account().is_some() { load_state().last } else { 0 } }

/// Take the other devices' changes, then send this one's. Returns how many rows came and went.
pub async fn sync_now() -> Result<(usize, usize)> {
    let _one = SYNCING.lock().await;
    let (token, uid) = id_token().await?;
    let base = documents().context("Accounts aren't set up in this build")?;
    let mut st = load_state();
    // Take: every document the server changed since the last time, in pages.
    let mut taken = 0;
    loop {
        let docs = list_items(&token, &uid, st.taken_until.as_deref(), 300).await?;
        if docs.is_empty() { break; }
        let (mut media, mut playback) = (Vec::new(), Vec::new());
        for (_, fields) in &docs {
            let Some(row) = fields["data"]["stringValue"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()) else { continue };
            let key = row_key(&row);
            st.known.insert(key.clone());
            if let Some(t) = row["updated"].as_i64() { st.seen.insert(key.clone(), t); }
            if key.starts_with("p|") { playback.push(row) } else { media.push(row) }
        }
        taken += docs.len();
        exec(json!({"op":"sync_apply","media":media,"playback":playback})).await?;
        st.taken_until = docs.last().and_then(|(_, f)| f["synced"]["timestampValue"].as_str().map(str::to_owned));
        save_state(&st);
        if docs.len() < 300 { break; }
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
        writes.push(item_write(&base, &uid, &doc_id(&key), &data));
        st.seen.insert(key.clone(), updated);
        st.known.insert(key);
    }
    for gone in std::mem::take(&mut st.forgotten) {
        let key = row_key(&gone);
        writes.push(item_write(&base, &uid, &doc_id(&key), &gone));
    }
    let sent = writes.len();
    commit(&token, writes).await?;
    // A second's changes may still be coming: the next sync looks at that second again.
    st.sent_until = (newest - 1).max(st.sent_until);
    st.last = now_secs();
    save_state(&st);
    Ok((taken, sent))
}

/// A title forgotten here is forgotten on the other devices too.
pub fn sync_forgotten(media: &Media) {
    if account().is_none() { return; }
    let mut st = load_state();
    st.forgotten.push(json!({"t":"m","type":media.kind,"id":media.id,"addon_url":media.addon_url,"updated":now_secs(),"deleted":true}));
    save_state(&st);
    sync_soon();
}

static WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sync a few seconds from now (after a change here).
pub fn sync_soon() { WANTED.store(true, std::sync::atomic::Ordering::Relaxed); }

/// While signed in: sync at launch, a few seconds after each change here, and every two minutes.
pub fn start_sync() {
    if account().is_none() || STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) { return; }
    sync_soon();
    let Some(runtime) = RUNTIME.get() else { return };
    runtime.spawn(async {
        let mut idle = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if account().is_none() { STARTED.store(false, std::sync::atomic::Ordering::Release); return; }
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
    let account = set_session(session);
    id_token().await?;
    Ok(Some(account))
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
/// The phone's half: answer a computer's QR code with this account's session, sealed with the
/// code's key (only that computer can open it).
pub async fn answer_link(code: &str) -> Result<()> {
    let parts: Vec<&str> = code.trim().split(':').collect();
    let ["veylo-link", "1", link, key] = parts[..] else { anyhow::bail!("That isn't a Veylo sign-in code.") };
    anyhow::ensure!(link.len() <= 64 && link.bytes().all(|b| b.is_ascii_alphanumeric()), "That isn't a Veylo sign-in code.");
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(key).ok().filter(|k| k.len() == 32).context("That isn't a Veylo sign-in code.")?;
    let (token, _) = id_token().await?;
    let session = SESSION.lock().ok().and_then(|x| x.clone()).context("Not signed in")?;
    let a = &session.account;
    let plain = json!({"uid":a.uid,"email":a.email,"name":a.name,"provider":a.provider,"refresh":session.refresh}).to_string();
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
    let code = loop {
        sleep(250).await;
        match crate::android::account_result() {
            Some(r) if r.starts_with("code:") => break r["code:".len()..].to_owned(),
            Some(r) if r.starts_with("cancel:") => anyhow::bail!("Scan cancelled."),
            Some(r) if r.starts_with("error:") => anyhow::bail!("The scanner didn't open: {}", &r["error:".len()..]),
            _ => {}
        }
    };
    answer_link(&code).await
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
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
    fn a_sealed_session_opens_only_with_its_key() {
        let key = [7u8; 32];
        let sealed = seal(&key, b"{\"refresh\":\"r\"}").unwrap();
        assert_eq!(open_sealed(&key, &sealed).unwrap(), b"{\"refresh\":\"r\"}");
        assert_eq!(open_sealed(&[8u8; 32], &sealed), None);
        assert_eq!(open_sealed(&key, "bm9wZQ"), None);
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
}
