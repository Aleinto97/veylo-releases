//! Firebase cross-OS diagnostics & issue reporting (strictly opt-in: disabled by default in Settings).
//! Captures crashes (`last-panic.txt`), WebView JS / layout overflow errors, playback failures,
//! frame-drop / slow-autoplay performance issues, and manual user reports—always strictly redacted
//! (never URLs, API keys, tokens, emails, hostnames, user home paths, or watch history).
//!
//! Signed-in devices write to `users/{uid}/diagnostics/{id}` (readable across all your devices
//! in Settings and in Firebase Console). Signed-out devices submit create-only anonymous reports
//! to `diagnostics/{id}` only when explicitly opted in or manually submitted.
use super::*;

#[derive(Clone, Debug, PartialEq)]
pub struct DiagReport {
    pub id: String,
    pub kind: String,
    pub os: String,
    pub arch: String,
    pub version: String,
    pub device: String,
    pub screen: String,
    pub summary: String,
    pub details: String,
    pub created_at: i64,
}

/// Canonical platform tag so issues can be triaged cleanly per OS (including Android TV).
pub fn platform_tag() -> String {
    if cfg!(target_os = "android") {
        if crate::is_tv() { "android-tv".into() } else { "android".into() }
    } else {
        std::env::consts::OS.to_owned()
    }
}

static SENT_KEYS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
const MAX_AUTO_REPORTS_PER_SESSION: usize = 15;

fn should_send_auto(kind: &str, summary: &str) -> bool {
    let Ok(mut guard) = SENT_KEYS.lock() else { return false };
    let set = guard.get_or_insert_with(HashSet::new);
    if set.len() >= MAX_AUTO_REPORTS_PER_SESSION { return false; }
    let key = format!("{kind}|{}", summary.chars().take(120).collect::<String>());
    set.insert(key)
}

fn sanitize_text(text: &str, max_chars: usize) -> String {
    let mut clean = redact_diagnostic(text);
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() { clean = clean.replace(&home, "~"); }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() { clean = clean.replace(&profile, "~"); }
    }
    clean.chars().take(max_chars).collect()
}

fn build_fields(kind: &str, summary: &str, details: &str, created: i64) -> Value {
    let settings = load_settings();
    let (screen, _) = crate::display::summary(crate::display::screen(), settings.hdr_output);
    let generic_device = format!("{} ({})", platform_tag(), std::env::consts::ARCH);
    json!({
        "kind": {"stringValue": sanitize_text(kind, 40)},
        "os": {"stringValue": platform_tag()},
        "arch": {"stringValue": std::env::consts::ARCH},
        "version": {"stringValue": env!("CARGO_PKG_VERSION")},
        "device": {"stringValue": generic_device},
        "screen": {"stringValue": sanitize_text(&screen, 80)},
        "summary": {"stringValue": sanitize_text(summary, 360)},
        "details": {"stringValue": sanitize_text(details, 7500)},
        "created": {"integerValue": created.to_string()},
    })
}

fn parse_report_doc(doc: &Value) -> Option<DiagReport> {
    let name = doc["name"].as_str()?;
    let id = name.rsplit('/').next().unwrap_or(name).to_owned();
    let f = &doc["fields"];
    let str_f = |k: &str| f[k]["stringValue"].as_str().unwrap_or_default().to_owned();
    let created_at = f["created"]["integerValue"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
    let summary = str_f("summary");
    if summary.is_empty() { return None; }
    Some(DiagReport {
        id,
        kind: str_f("kind"),
        os: str_f("os"),
        arch: str_f("arch"),
        version: str_f("version"),
        device: str_f("device"),
        screen: str_f("screen"),
        summary,
        details: str_f("details"),
        created_at,
    })
}

async fn upload_fields(fields: Value) -> Result<()> {
    let Some(base) = account::documents_pub() else { return Ok(()) };
    let doc_id = format!("{}-{}", now_secs(), account::random_id_pub(6));
    if let Ok((token, uid)) = account::id_token_pub().await {
        let doc_path = format!("{base}/users/{uid}/diagnostics/{doc_id}");
        account::commit_pub(&token, vec![json!({
            "update": {"name": doc_path, "fields": fields},
            "updateTransforms": [{"fieldPath": "synced", "setToServerValue": "REQUEST_TIME"}],
        })]).await?;
        return Ok(());
    }
    // Signed-out fallback: brief anonymous session -> create-only `diagnostics/{id}` -> clean up.
    let anon = account::anon_sign_up_pub().await?;
    let token = s(&anon, "idToken");
    anyhow::ensure!(!token.is_empty(), "No telemetry session");
    let doc_path = format!("{base}/diagnostics/{doc_id}");
    let res = account::commit_pub(&token, vec![json!({
        "update": {"name": doc_path, "fields": fields},
        "updateTransforms": [{"fieldPath": "synced", "setToServerValue": "REQUEST_TIME"}],
        "currentDocument": {"exists": false},
    })]).await;
    let _ = account::anon_delete_pub(&token).await;
    res
}

/// Fire-and-forget automatic diagnostic report (respects Settings -> telemetry_enabled,
/// deduplicates per session, and never blocks the UI or player).
pub fn report_issue_bg(kind: &'static str, summary: impl Into<String>, details: impl Into<String>) {
    if !telemetry_enabled() || !accounts_available() { return; }
    let summary = summary.into();
    if summary.trim().is_empty() || !should_send_auto(kind, &summary) { return; }
    let details = details.into();
    let Some(runtime) = RUNTIME.get() else { return };
    runtime.spawn(async move {
        let fields = build_fields(kind, &summary, &details, now_secs());
        if let Err(e) = upload_fields(fields).await {
            dev_log(&format!("telemetry ({kind}): couldn't send ({e})"));
        } else {
            dev_log(&format!("telemetry ({kind}): reported"));
        }
    });
}

static PANIC_UPLOAD_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Upload an un-sent crash backtrace from a previous run (`last-panic.txt`) once on launch.
pub fn upload_pending_panic() {
    if !telemetry_enabled() || !accounts_available() { return; }
    if PANIC_UPLOAD_STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) { return; }
    let path = backend().data_dir.join("last-panic.txt");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    if text.trim().is_empty() { return; }
    let first = text.lines().next().unwrap_or("Application panic").to_owned();
    let summary = format!("Crash: {first}");
    if !should_send_auto("crash", &summary) { return; }
    let sent_path = backend().data_dir.join("last-panic-sent.txt");
    let Some(runtime) = RUNTIME.get() else { return };
    runtime.spawn(async move {
        let fields = build_fields("crash", &summary, &text, now_secs());
        match upload_fields(fields).await {
            Ok(()) => {
                // Only archive after upload succeeds so an offline launch retries next time.
                let _ = std::fs::rename(&path, &sent_path);
                dev_log("telemetry (crash): reported");
            }
            Err(e) => {
                dev_log(&format!("telemetry (crash): couldn't send ({e})"));
            }
        }
    });
}

/// Explicit user bug/UI/performance report from Settings, including the redacted recent log.
pub async fn send_manual_report(category: &str, message: &str) -> Result<()> {
    let msg = message.trim();
    anyhow::ensure!(!msg.is_empty(), "Describe the issue first.");
    anyhow::ensure!(accounts_available(), "Firebase isn't configured in this build.");
    let log = diagnostic_report();
    let details = format!("User report ({category}):\n{msg}\n\n{log}");
    let fields = build_fields(category, msg, &details, now_secs());
    upload_fields(fields).await
}

/// Fetch recent diagnostic reports across all of your devices (signed-in account only).
pub async fn list_remote_reports(os_filter: Option<&str>) -> Result<Vec<DiagReport>> {
    let (token, uid) = account::id_token_pub().await.context("Sign in to view reports across your devices.")?;
    let base = account::documents_pub().context("Firebase isn't configured in this build.")?;
    let doc = account::get_doc_pub(&token, format!("{base}/users/{uid}/diagnostics?pageSize=100")).await?.unwrap_or_default();
    let mut list: Vec<DiagReport> = doc["documents"].as_array().into_iter().flatten()
        .filter_map(parse_report_doc)
        .filter(|r| os_filter.is_none_or(|want| want.is_empty() || r.os.eq_ignore_ascii_case(want)))
        .collect();
    list.sort_by_key(|r| std::cmp::Reverse(r.created_at));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_redacts_and_roundtrips_firestore_docs() {
        let s = sanitize_text("Failed at https://real-debrid.com/d/SECRET token=abc123", 200);
        assert!(!s.contains("SECRET") && !s.contains("abc123"), "{s}");
        assert!(s.contains("[URL]") && s.contains("[REDACTED]"), "{s}");

        let doc = json!({
            "name": "projects/p/databases/(default)/documents/users/u/diagnostics/1700-abc",
            "fields": {
                "kind": {"stringValue": "ui_error"},
                "os": {"stringValue": "android"},
                "arch": {"stringValue": "aarch64"},
                "version": {"stringValue": "0.10.0"},
                "device": {"stringValue": "YLE_W09"},
                "screen": {"stringValue": "HDR10+"},
                "summary": {"stringValue": "Horizontal overflow on .bottom-sheet"},
                "details": {"stringValue": "width=2100 viewport=1200"},
                "created": {"integerValue": "1700000000"},
            }
        });
        let parsed = parse_report_doc(&doc).unwrap();
        assert_eq!(parsed.id, "1700-abc");
        assert_eq!(parsed.kind, "ui_error");
        assert_eq!(parsed.os, "android");
        assert_eq!(parsed.device, "YLE_W09");
        assert_eq!(parsed.created_at, 1700000000);
    }

    #[test]
    fn auto_telemetry_deduplicates_identical_issues() {
        assert!(should_send_auto("ui_error", "unique-test-bug-alpha"));
        assert!(!should_send_auto("ui_error", "unique-test-bug-alpha"));
        assert!(should_send_auto("performance", "unique-test-bug-alpha"));
    }
}
