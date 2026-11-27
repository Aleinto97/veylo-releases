//! Sentry crash & error reporting for the Flutter bridge.
//!
//! One client lives here, in Rust: backend panics are caught by Sentry's panic hook, failed
//! bridge calls are reported per op, and Dart/Flutter errors are forwarded through the
//! `report_error` op. Nothing leaves the device unless Settings → "Opt-in diagnostics"
//! (`telemetry_enabled`, off by default) is on, and every string is passed through
//! `backend::redact_diagnostic` first (add-on URLs, keys, tokens, emails, home paths stripped).
//!
//! The DSN comes from `VEYLO_SENTRY_DSN` at build time (or at run time, for local testing). It is
//! never committed; without one, reporting is compiled in but inert.

use crate::backend;
use sentry::protocol::{Event, Level, Value as SentryValue};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

static GUARD: OnceLock<Option<sentry::ClientInitGuard>> = OnceLock::new();
/// Last time each op's failure was sent: one report per op every 10 minutes keeps quota sane.
static LAST_SENT: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
const OP_REPORT_EVERY: Duration = Duration::from_secs(600);

fn dsn() -> Option<String> {
    std::env::var("VEYLO_SENTRY_DSN").ok()
        .or_else(|| option_env!("VEYLO_SENTRY_DSN").map(str::to_owned))
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty())
}

/// Start Sentry once. Call after `backend::init()` so Sentry's panic hook chains the backend's
/// redacting hook (which still writes `last-panic.txt`) instead of being replaced by it.
pub fn init() {
    GUARD.get_or_init(|| {
        let dsn = dsn()?;
        let mut options = sentry::ClientOptions::default();
        options.release = Some(concat!("veylo-flutter@", env!("CARGO_PKG_VERSION")).into());
        options.environment = Some(if cfg!(debug_assertions) { "debug" } else { "release" }.into());
        options.send_default_pii = false;
        options.server_name = None;
        options.attach_stacktrace = false;
        options.max_breadcrumbs = 40;
        options.before_send = Some(Arc::new(|event| {
            if !reports_on() { return None; }
            Some(scrub(event))
        }));
        options.before_breadcrumb = Some(Arc::new(|mut b| {
            if !reports_on() { return None; }
            b.message = b.message.map(|m| backend::redact_diagnostic(&m));
            b.data.clear();
            Some(b)
        }));
        // Logs: redacted, and dropped like events when diagnostics are off.
        options.before_send_log = Some(Arc::new(|mut log| {
            if !reports_on() { return None; }
            log.body = backend::redact_diagnostic(&log.body);
            for attr in log.attributes.values_mut() { redact_value(&mut attr.0); }
            Some(log)
        }));
        // Tracing: every bridge op is a transaction (op name + duration + status, no data).
        // All of them locally/debug, 10% in release builds.
        let options = options.traces_sample_rate(if cfg!(debug_assertions) { 1.0 } else { 0.1 });
        let guard = sentry::init((dsn, options));
        sentry::configure_scope(|scope| {
            scope.set_tag("os", std::env::consts::OS);
            scope.set_tag("arch", std::env::consts::ARCH);
            scope.set_tag("shell", "flutter");
            if let Ok(t) = std::env::var("VEYLO_TEST_TARGET") { scope.set_tag("test_target", t); }
        });
        Some(guard)
    });
}

pub fn active() -> bool {
    GUARD.get().is_some_and(Option::is_some)
}

/// Set by the `force_reports` op: CI runtime tests report without touching the user's setting.
static FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn force_reports() { FORCED.store(true, std::sync::atomic::Ordering::Relaxed); }

/// Opt-in diagnostics, a forced test run, or `VEYLO_SENTRY_TEST` in the environment.
fn reports_on() -> bool {
    backend::telemetry_enabled()
        || FORCED.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var_os("VEYLO_SENTRY_TEST").is_some()
}

fn sending() -> bool { active() && reports_on() }

/// A performance transaction for one bridge op, only when reports are on (no data, just timing).
pub fn op_transaction(op: &str) -> Option<sentry::TransactionOrSpan> {
    if !sending() || op == "report_error" || op == "flush_reports" { return None; }
    let ctx = sentry::TransactionContext::new(&format!("bridge.{op}"), "bridge.op");
    Some(sentry::start_transaction(ctx).into())
}

pub fn finish_transaction(tx: Option<sentry::TransactionOrSpan>, status: sentry::protocol::SpanStatus) {
    if let Some(tx) = tx { tx.set_status(status); tx.finish(); }
}

fn redact(s: &str) -> String { backend::redact_diagnostic(s) }

fn redact_value(v: &mut SentryValue) {
    match v {
        SentryValue::String(s) => *s = redact(s),
        SentryValue::Array(a) => a.iter_mut().for_each(redact_value),
        SentryValue::Object(o) => o.values_mut().for_each(redact_value),
        _ => {}
    }
}

/// Field-by-field redaction (the event id and frame addresses are hex and must survive).
fn scrub(mut e: Event<'static>) -> Event<'static> {
    e.server_name = None;
    e.user = None;
    e.request = None;
    e.message = e.message.map(|m| redact(&m));
    if let Some(ref mut l) = e.logentry {
        l.message = redact(&l.message);
        l.params.iter_mut().for_each(redact_value);
    }
    e.extra.values_mut().for_each(redact_value);
    for b in e.breadcrumbs.values.iter_mut() {
        b.message = b.message.as_deref().map(redact);
        b.data.clear();
    }
    let scrub_frames = |st: &mut sentry::protocol::Stacktrace| {
        for f in st.frames.iter_mut() {
            f.abs_path = f.abs_path.as_deref().map(redact);
            f.filename = f.filename.as_deref().map(redact);
            f.package = f.package.as_deref().map(redact);
            f.vars.clear();
        }
    };
    for ex in e.exception.values.iter_mut() {
        ex.value = ex.value.as_deref().map(redact);
        if let Some(ref mut st) = ex.stacktrace { scrub_frames(st); }
    }
    for t in e.threads.values.iter_mut() {
        if let Some(ref mut st) = t.stacktrace { scrub_frames(st); }
    }
    // Loaded-library paths (`/home/<user>/…/libveylo_bridge.so`): Sentry matches images by debug
    // id, so the path can be redacted without losing symbolication.
    for img in e.debug_meta.to_mut().images.iter_mut() {
        if let sentry::protocol::DebugImage::Symbolic(ref mut i) = img { i.name = redact(&i.name); }
    }
    e
}

fn due(key: &str) -> bool {
    let mut g = LAST_SENT.lock().unwrap_or_else(|p| p.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    match map.get(key) {
        Some(at) if now.duration_since(*at) < OP_REPORT_EVERY => false,
        _ => { map.insert(key.to_owned(), now); true }
    }
}

fn op_event(op: &str, level: Level, message: String, elapsed: Duration, kind: &str) -> Event<'static> {
    let mut e = Event { level, message: Some(message), ..Default::default() };
    e.tags.insert("op".into(), op.to_owned());
    e.tags.insert("kind".into(), kind.to_owned());
    e.extra.insert("elapsed_ms".into(), SentryValue::from(elapsed.as_millis() as u64));
    // Group by op + kind, not by the (redacted, still variable) error text.
    e.fingerprint = vec![kind.to_owned().into(), op.to_owned().into()].into();
    e
}

/// A bridge op returned an error: always logged locally (redacted), sent at most every 10 min per op.
pub fn op_failed(op: &str, elapsed: Duration, err: &anyhow::Error) {
    let clean = redact(&format!("{err:#}"));
    backend::record_diag_line(&format!("bridge op {op} failed after {}ms: {clean}", elapsed.as_millis()));
    sentry::add_breadcrumb(sentry::Breadcrumb {
        category: Some("bridge".into()),
        message: Some(format!("{op} failed: {clean}")),
        level: Level::Warning,
        ..Default::default()
    });
    if sending() { sentry::logger_warn!(op = op, elapsed_ms = elapsed.as_millis() as u64, "bridge op failed: {}", clean.clone()); }
    if sending() && due(&format!("err:{op}")) {
        sentry::capture_event(op_event(op, Level::Warning, format!("{op}: {clean}"), elapsed, "op-error"));
    }
}

/// A bridge op panicked: the panic itself was already captured by Sentry's panic hook (with its
/// backtrace); this records which op it was so the Dart side can show a real error.
pub fn op_panicked(op: &str, elapsed: Duration) {
    backend::record_diag_line(&format!("bridge op {op} panicked after {}ms", elapsed.as_millis()));
    if sending() { sentry::logger_fatal!(op = op, "bridge op panicked"); }
    if sending() && due(&format!("panic:{op}")) {
        sentry::capture_event(op_event(op, Level::Fatal, format!("{op} panicked"), elapsed, "op-panic"));
    }
}

/// Successful ops leave a breadcrumb only when slow, so a later report shows what was dragging.
pub fn op_slow(op: &str, elapsed: Duration) {
    if elapsed >= Duration::from_secs(4) {
        backend::record_diag_line(&format!("bridge op {op} slow: {}ms", elapsed.as_millis()));
        sentry::add_breadcrumb(sentry::Breadcrumb {
            category: Some("bridge".into()),
            message: Some(format!("{op} took {}ms", elapsed.as_millis())),
            level: Level::Info,
            ..Default::default()
        });
        if sending() { sentry::logger_info!(op = op, elapsed_ms = elapsed.as_millis() as u64, "bridge op slow"); }
    }
}

/// A Dart/Flutter error forwarded by the UI (`report_error` op).
pub fn dart_error(source: &str, error: &str, stack: &str, fatal: bool) {
    let clean = redact(error);
    backend::record_diag_line(&format!("dart {source}: {clean}"));
    if !sending() { return; }
    sentry::logger_error!(source = source, "dart error: {}", clean.clone());
    let first_line = clean.lines().next().unwrap_or("").chars().take(160).collect::<String>();
    if !due(&format!("dart:{source}:{first_line}")) { return; }
    let mut e = Event {
        level: if fatal { Level::Fatal } else { Level::Error },
        message: Some(clean.clone()),
        platform: "dart".into(),
        ..Default::default()
    };
    e.tags.insert("kind".into(), "dart".into());
    e.tags.insert("source".into(), source.to_owned());
    // Dart stack traces are kept as text (redacted, trimmed): they name files and lines, not data.
    let stack = redact(&stack.lines().take(60).collect::<Vec<_>>().join("\n"));
    if !stack.is_empty() { e.extra.insert("dart_stack".into(), SentryValue::from(stack)); }
    e.fingerprint = vec!["dart".into(), source.to_owned().into(), first_line.into()].into();
    sentry::capture_event(e);
}

/// Flush queued reports before the process exits.
pub fn flush() {
    if let Some(client) = sentry::Hub::current().client() { client.flush(Some(Duration::from_secs(2))); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_strips_urls_keys_and_host_but_keeps_ids() {
        let mut e = Event { message: Some("GET https://addon.example/abc123/manifest.json?apikey=SECRET failed".into()), ..Default::default() };
        e.server_name = Some("my-laptop".into());
        e.extra.insert("x".into(), SentryValue::from("token=hunter2 at /home/alice/x"));
        e.debug_meta = std::borrow::Cow::Owned(sentry::protocol::DebugMeta {
            images: vec![sentry::protocol::DebugImage::Symbolic(sentry::protocol::SymbolicDebugImage {
                name: "/home/alice/app/lib/libveylo_bridge.so".into(),
                arch: None, image_addr: 0.into(), image_size: 0, image_vmaddr: 0.into(),
                id: Default::default(), code_id: None, debug_file: None,
            })],
            ..Default::default()
        });
        let id = e.event_id;
        let out = scrub(e);
        let msg = out.message.unwrap();
        assert!(!msg.contains("addon.example") && !msg.contains("SECRET"), "{msg}");
        assert!(out.server_name.is_none());
        let extra = out.extra["x"].as_str().unwrap().to_owned();
        assert!(!extra.contains("hunter2") && !extra.contains("alice"), "{extra}");
        assert_eq!(out.event_id, id);
        let sentry::protocol::DebugImage::Symbolic(ref img) = out.debug_meta.images[0] else { panic!() };
        assert!(!img.name.contains("alice"), "{}", img.name);
    }

    #[test]
    fn op_reports_are_rate_limited() {
        assert!(due("test:op"));
        assert!(!due("test:op"));
        assert!(due("test:other"));
    }
}
