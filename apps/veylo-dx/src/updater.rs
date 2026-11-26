//! Updates from the public releases repo, on Windows, macOS and Android. At launch Veylo asks GitHub
//! for the latest release; a newer one shows a notice, and Update downloads it and installs it:
//! Windows and macOS swap the files and restart, Android hands the APK to the system installer.
//! Only public release metadata is fetched: no account, key or setting is ever sent.
use crate::icons;
use anyhow::{anyhow, Context, Result};
use dioxus::prelude::*;
use std::path::PathBuf;

/// The public repo: its releases are the builds (published by .github/workflows/release.yml).
pub const REPO: &str = "Aleinto97/veylo-releases";
/// This platform's release asset; None where releases aren't installed (Linux builds from source).
pub const ASSET: Option<&str> = if cfg!(all(target_os = "android", target_arch = "arm")) { Some("veylo-android-armv7.apk") }
    else if cfg!(target_os = "android") { Some("veylo-android-arm64.apk") }
    else if cfg!(target_os = "windows") { Some("veylo-windows-x64.zip") }
    else if cfg!(all(target_os = "macos", target_arch = "aarch64")) { Some("veylo-macos-arm64.zip") }
    else { None };

#[derive(Clone, Debug, PartialEq)]
pub struct Release { pub version: String, url: String, sha256: String }

/// Keep only safe path characters from a release version string.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos", test)), allow(dead_code))]
fn safe_version(version: &str) -> String {
    let clean: String = version.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(48).collect();
    if clean.is_empty() || clean.starts_with('.') { "update".into() } else { clean }
}

/// Extract a 64-hex SHA-256 digest from GitHub's asset `digest` field ("sha256:<hex>").
fn parse_github_digest(digest: Option<&str>) -> Option<String> {
    let hex = digest?.strip_prefix("sha256:")?.trim();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
}

/// Parse a `SHA256SUMS.txt` file ("<64-hex>  <filename>") for a specific asset name.
fn parse_sha256sums(text: &str, asset: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(hex) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        let name = name.trim_start_matches('*');
        if name == asset && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// A version's numbers: "v1.2.3-beta" is [1, 2, 3] (a "v" before and a suffix after are left out);
/// a version without a number at its start is [].
fn parse(version: &str) -> Vec<u64> {
    let v = version.trim().trim_start_matches('v');
    let end = v.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(v.len());
    v[..end].split('.').map_while(|p| p.parse().ok()).collect()
}

/// Whether `latest` is a higher version than `current`, number by number ("0.10.0" > "0.9.3").
/// Also the add-ons' update check (addons.rs).
pub fn newer(latest: &str, current: &str) -> bool {
    let (a, b) = (parse(latest), parse(current));
    !a.is_empty() && a > b
}

const MAX_UPDATE_BYTES: u64 = 256 * 1024 * 1024;

/// How the last release check ended. A definitive answer (a newer release, or none) is kept for
/// the session; a transient failure (offline, rate limit, GitHub down) is remembered only briefly,
/// so launching offline doesn't hide an update for the whole session.
#[derive(Clone)]
enum CheckState {
    Done(Option<Release>),
    Failed(std::time::Instant),
}
static CHECKED: std::sync::Mutex<Option<CheckState>> = std::sync::Mutex::new(None);
/// A check is only repeated after a transient failure once this has passed.
const RETRY_AFTER_FAILURE: std::time::Duration = std::time::Duration::from_secs(60);

fn is_allowed_release_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return false };
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    let Some(host) = parsed.host_str() else { return false };
    let host = host.to_ascii_lowercase();
    // Default https port only; the dot before the suffix keeps "evilgithubusercontent.com" out.
    parsed.port().is_none()
        && (host == "github.com" || host == "api.github.com" || host.ends_with(".githubusercontent.com"))
}

/// Release files live on GitHub and its asset CDNs only: every redirect hop must stay on those
/// hosts over https, so a hijacked redirect can't point the download (or the checksum fetch) elsewhere.
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 { return attempt.error("too many redirects"); }
        if is_allowed_release_url(attempt.url().as_str()) { attempt.follow() } else { attempt.stop() }
    })
}

fn client() -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .user_agent("veylo-dx")
        .redirect(redirect_policy())
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(30));
    #[cfg(not(test))]
    let builder = builder.https_only(true);
    builder.build().unwrap_or_default()
}

/// The latest release when it is newer than this build and has this platform's file; None offline or up to date.
/// A definitive answer is cached for the session so navigating overlays never burns through GitHub's
/// unauthenticated rate limit; an offline or rate-limited check is retried after a minute.
pub async fn check() -> Option<Release> {
    let asset = ASSET?;
    #[cfg(target_os = "android")]
    prune_old_android_apks();
    match CHECKED.lock().ok().and_then(|g| g.clone()) {
        Some(CheckState::Done(found)) => return found,
        Some(CheckState::Failed(at)) if at.elapsed() < RETRY_AFTER_FAILURE => return None,
        _ => {}
    }
    let outcome = crate::backend::on_runtime(async move { fetch_latest(asset).await }).await.unwrap_or(Err(()));
    let state = match &outcome {
        Ok(found) => CheckState::Done(found.clone()),
        Err(()) => CheckState::Failed(std::time::Instant::now()),
    };
    if let Ok(mut g) = CHECKED.lock() { *g = Some(state); }
    outcome.ok().flatten()
}

/// One release check: `Ok(Some)` a newer release with a verified checksum, `Ok(None)` up to date or
/// not installable here (definitive), `Err` a transient failure worth retrying.
async fn fetch_latest(asset: &str) -> std::result::Result<Option<Release>, ()> {
    let http = client();
    let body = http.get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send().await.map_err(|_| ())?
        .error_for_status().map_err(|_| ())?
        .bytes().await.map_err(|_| ())?;
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else { return Err(()) };
    let Some(tag) = v["tag_name"].as_str() else { return Ok(None) };
    let version = tag.trim_start_matches('v').to_owned();
    if !newer(&version, env!("CARGO_PKG_VERSION")) { return Ok(None); }
    let Some(assets) = v["assets"].as_array() else { return Ok(None) };
    let Some(entry) = assets.iter().find(|a| a["name"] == asset) else { return Ok(None) };
    let Some(url) = entry["browser_download_url"].as_str().map(str::to_owned) else { return Ok(None) };
    if !is_allowed_release_url(&url) { return Ok(None); }
    let mut sha256 = parse_github_digest(entry["digest"].as_str());
    if sha256.is_none() {
        if let Some(sums_url) = assets.iter().find(|a| a["name"] == "SHA256SUMS.txt")
            .and_then(|a| a["browser_download_url"].as_str())
            .filter(|u| is_allowed_release_url(u))
        {
            let resp = http.get(sums_url).send().await.map_err(|_| ())?.error_for_status().map_err(|_| ())?;
            let text = resp.text().await.map_err(|_| ())?;
            sha256 = parse_sha256sums(&text, asset);
        }
    }
    // Fail closed: never offer an update unless a valid 64-hex SHA-256 digest was verified.
    Ok(sha256.map(|sha256| Release { version, url, sha256 }))
}

#[cfg_attr(not(any(target_os = "android", target_os = "windows", target_os = "macos")), allow(dead_code))]
/// Stream the release file to `to` (an APK is ~150 MB: never held in memory) and verify SHA-256.
async fn download(url: String, to: PathBuf, expected_sha256: String) -> Result<()> {
    crate::backend::on_runtime(verify_download_to(url, to, expected_sha256)).await?
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
fn file_matches_sha256(path: &std::path::Path, expected_sha256: &str) -> bool {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) { return false; }
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    format!("{:x}", hasher.finalize()) == expected
}

#[cfg_attr(not(any(target_os = "android", target_os = "windows", target_os = "macos", test)), allow(dead_code))]
async fn verify_download_to(url: String, to: PathBuf, expected_sha256: String) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let expected = expected_sha256.trim().to_ascii_lowercase();
    anyhow::ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "Update refused: missing verified SHA-256 checksum."
    );
    let mut response = client().get(&url).send().await.and_then(|r| r.error_for_status())
        .map_err(|_| anyhow!("Couldn't download the update"))?;
    // A redirect off GitHub's hosts stops the client at the 3xx answer, which is no update file.
    anyhow::ensure!(!response.status().is_redirection(), "Couldn't download the update");
    if let Some(len) = response.content_length() {
        anyhow::ensure!(len <= MAX_UPDATE_BYTES, "The update file is unexpectedly large.");
    }
    if let Some(dir) = to.parent() { std::fs::create_dir_all(dir).context("Couldn't save the update")?; }
    let part = to.with_extension(format!("{}.part", to.extension().and_then(|e| e.to_str()).unwrap_or("bin")));
    let _ = std::fs::remove_file(&part);
    let mut file = std::fs::File::create(&part).context("Couldn't save the update")?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    while let Some(chunk) = response.chunk().await.map_err(|_| { let _ = std::fs::remove_file(&part); anyhow!("The download was interrupted") })? {
        written = written.saturating_add(chunk.len() as u64);
        if written > MAX_UPDATE_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("The update file is unexpectedly large.");
        }
        hasher.update(&chunk);
        if let Err(e) = file.write_all(&chunk) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(e).context("Couldn't save the update");
        }
    }
    file.sync_all().context("Couldn't save the update")?;
    drop(file);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!("The downloaded update failed verification. Try again.");
    }
    if let Err(e) = std::fs::rename(&part, &to) {
        let _ = std::fs::remove_file(&part);
        return Err(e).context("Couldn't save the update");
    }
    Ok(())
}

/// Create a private, unpredictable staging directory under Veylo's own data folder (never shared /tmp).
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn update_work_dir(version: &str) -> Result<PathBuf> {
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(|_| anyhow!("Couldn't generate an update folder name"))?;
    let suffix: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let updates = crate::backend::backend_data_dir().join("updates");
    // Staging folders kept by a failed swap (or never cleaned) go after a week.
    if let Ok(entries) = std::fs::read_dir(&updates) {
        let week = std::time::Duration::from_secs(7 * 24 * 3600);
        for entry in entries.flatten() {
            let old = entry.metadata().and_then(|m| m.modified()).ok()
                .and_then(|t| t.elapsed().ok()).is_some_and(|age| age > week);
            if old && entry.file_name().to_str().is_some_and(|n| n.starts_with("veylo-")) { let _ = std::fs::remove_dir_all(entry.path()); }
        }
    }
    let dir = updates.join(format!("veylo-{}-{suffix}", safe_version(version)));
    std::fs::create_dir_all(&dir).context("Couldn't prepare the update folder")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// Remove cached Android APKs from completed upgrades (version <= running build) so ~150 MB isn't stranded.
#[cfg(target_os = "android")]
fn prune_old_android_apks() {
    let Ok(dir) = crate::android::files_dir().map(|d| d.join("updates")) else { return };
    let Some(asset) = ASSET else { return };
    let apk = dir.join(asset);
    let ver_file = apk.with_extension("version");
    // A download that was interrupted (or killed mid-way) leaves its `.apk.part`: never reused, ~150 MB each.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(|n| n.ends_with(".part")) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    if let Ok(ver) = std::fs::read_to_string(&ver_file) {
        if !newer(ver.trim(), env!("CARGO_PKG_VERSION")) {
            let _ = std::fs::remove_file(&apk);
            let _ = std::fs::remove_file(&ver_file);
        }
    }
}

/// What Install led to, when it returns at all (Windows quits to swap its files).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))] // Windows and macOS restart instead of returning
pub enum Outcome { Installing, NeedsPermission }

#[cfg(target_os = "android")]
pub async fn install(release: Release) -> Result<Outcome> {
    let apk = crate::android::files_dir()?.join("updates").join(ASSET.context("No update for this platform")?);
    // Already downloaded (the viewer went to allow installs and came back): reuse only if SHA-256 still matches.
    let apk_check = apk.clone();
    let expected = release.sha256.clone();
    let fresh = std::fs::read_to_string(apk.with_extension("version")).ok().as_deref() == Some(release.version.as_str())
        && crate::backend::on_runtime(async move {
            tokio::task::spawn_blocking(move || file_matches_sha256(&apk_check, &expected)).await.unwrap_or(false)
        }).await.unwrap_or(false);
    if !fresh {
        download(release.url.clone(), apk.clone(), release.sha256.clone()).await?;
        let _ = crate::backend::write_private_file(&apk.with_extension("version"), release.version.as_bytes());
    }
    let apk_install = apk.clone();
    let status = crate::backend::on_runtime(async move {
        tokio::task::spawn_blocking(move || crate::android::install_update(&apk_install))
            .await
            .map_err(|_| anyhow!("Installer task failed"))?
    }).await??;
    match status.as_str() {
        "permission" => Ok(Outcome::NeedsPermission),
        _ => Ok(Outcome::Installing),
    }
}

/// Waits for Veylo to close, stages the zip into a fresh folder, swaps atomically with backup/rollback, and relaunches.
#[cfg(target_os = "windows")]
const APPLY: &str = r#"param([int]$ProcessId, [string]$Zip, [string]$Dest)
$ErrorActionPreference = 'Stop'
$work = Split-Path -Parent $Zip
$stage = Join-Path $work 'new'
$backup = "$Dest.old"
$exe = Join-Path $Dest 'veylo-dx.exe'
$swapped = $false
# Put the previous install back: whatever half-copied folder sits at $Dest goes, the backup returns.
function Restore-Backup {
    if (-not (Test-Path (Join-Path $backup 'veylo-dx.exe'))) { return $false }
    try {
        if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest -ErrorAction Stop }
        Move-Item -Path $backup -Destination $Dest -ErrorAction Stop
        return $true
    } catch { return $false }
}
try {
    try { Wait-Process -Id $ProcessId -Timeout 30 -ErrorAction SilentlyContinue } catch {}
    Start-Sleep -Milliseconds 500
    # An earlier swap was cut off after the old install moved aside: that backup IS the install.
    if (-not (Test-Path $exe)) { [void](Restore-Backup) }
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    Expand-Archive -Force -Path $Zip -DestinationPath $stage
    if (-not (Test-Path (Join-Path $stage 'veylo-dx.exe'))) { throw 'Update archive missing veylo-dx.exe' }
    # Only replace the entire parent directory when it exclusively holds Veylo's own bundle.
    # If veylo-dx.exe lives in a shared folder (e.g. Downloads), replace only Veylo's staged entries in-place.
    $allowed = @('veylo-dx.exe', 'libmpv-2.dll', 'picks-ai', 'licenses')
    $hasForeign = $false
    if (Test-Path $Dest) {
        foreach ($item in (Get-ChildItem -LiteralPath $Dest -Force)) {
            $name = $item.Name.ToLowerInvariant()
            if (($allowed -notcontains $name) -or (($name -eq 'licenses') -and -not (Test-Path (Join-Path $item.FullName 'THIRD_PARTY_LICENSES.md')))) {
                $hasForeign = $true; break
            }
        }
    }
    if ($hasForeign) {
        $itemBackup = Join-Path $work 'prev-items'
        for ($i = 0; $i -lt 10 -and -not $swapped; $i++) {
            $backed = @()
            try {
                if (Test-Path $itemBackup) { Remove-Item -Recurse -Force $itemBackup }
                New-Item -ItemType Directory -Path $itemBackup | Out-Null
                foreach ($entry in (Get-ChildItem -LiteralPath $stage -Force)) {
                    $dstItem = Join-Path $Dest $entry.Name
                    if (($entry.Name -ieq 'licenses') -and (Test-Path $dstItem) -and -not (Test-Path (Join-Path $dstItem 'THIRD_PARTY_LICENSES.md'))) {
                        continue
                    }
                    if (Test-Path $dstItem) {
                        Move-Item -LiteralPath $dstItem -Destination (Join-Path $itemBackup $entry.Name) -ErrorAction Stop
                        $backed += $entry.Name
                    }
                    Copy-Item -LiteralPath $entry.FullName -Destination $dstItem -Recurse -Force -ErrorAction Stop
                }
                if (-not (Test-Path $exe)) { throw 'New install is missing veylo-dx.exe' }
                $swapped = $true
            } catch {
                foreach ($name in $backed) {
                    $dstItem = Join-Path $Dest $name
                    $bakItem = Join-Path $itemBackup $name
                    if (Test-Path $bakItem) {
                        if (Test-Path $dstItem) { Remove-Item -Recurse -Force $dstItem -ErrorAction SilentlyContinue }
                        Move-Item -LiteralPath $bakItem -Destination $dstItem -ErrorAction SilentlyContinue
                    }
                }
                Start-Sleep -Seconds 1
            }
        }
    } else {
        for ($i = 0; $i -lt 10 -and -not $swapped; $i++) {
            $moved = $false
            try {
                # Only remove an existing backup that is genuinely an earlier Veylo bundle and while the current install is healthy.
                if ((Test-Path (Join-Path $backup 'veylo-dx.exe')) -and (Test-Path $exe)) { Remove-Item -Recurse -Force $backup }
                Move-Item -Path $Dest -Destination $backup -ErrorAction Stop
                $moved = $true
                Move-Item -Path $stage -Destination $Dest -ErrorAction Stop
                if (-not (Test-Path $exe)) { throw 'New install is missing veylo-dx.exe' }
                $swapped = $true
            } catch {
                if ($moved) { [void](Restore-Backup) }
                Start-Sleep -Seconds 1
            }
        }
        # The backup is deleted only once the new veylo-dx.exe is verifiably in place.
        if ($swapped) {
            if (Test-Path $exe) {
                Remove-Item -Recurse -Force $backup -ErrorAction SilentlyContinue
            } else {
                $swapped = $false
                [void](Restore-Backup)
            }
        }
    }
} finally {
    # Last resort: nothing runnable at $Dest but a backup exists.
    if (-not (Test-Path $exe)) { [void](Restore-Backup) }
    if (Test-Path $exe) { Start-Process $exe }
    # A failed swap keeps its staging folder (the downloaded zip) for inspection or a retry.
    if ($swapped) { Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue }
}
"#;

/// Download the release, hand it to the helper and quit so the files can be replaced.
#[cfg(target_os = "windows")]
pub async fn install(release: Release) -> Result<Outcome> {
    let exe = std::env::current_exe().context("Couldn't find Veylo's folder")?;
    let dest: &std::path::Path = exe.parent().context("Couldn't find Veylo's folder")?;
    let work = update_work_dir(&release.version)?;
    let zip = work.join(ASSET.context("No update for this platform")?);
    let script = work.join("apply.ps1");
    download(release.url.clone(), zip.clone(), release.sha256.clone()).await?;
    std::fs::write(&script, APPLY).context("Couldn't save the update")?;
    let ps = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|r| r.join("System32\\WindowsPowerShell\\v1.0\\powershell.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let mut helper = std::process::Command::new(ps);
    helper.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"]).arg(&script)
        .arg("-ProcessId").arg(std::process::id().to_string()).arg("-Zip").arg(&zip).arg("-Dest").arg(dest);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        helper.creation_flags(CREATE_NO_WINDOW);
    }
    // Playback progress still queued is written first: the app quits right after the helper starts.
    crate::backend::flush_progress().await;
    crate::backend::flush_writes();
    helper.spawn().context("Couldn't start the updater")?;
    std::process::exit(0);
}

/// macOS: waits for Veylo to close, stages on the same volume, swaps Veylo.app with rollback on failure, and relaunches.
#[cfg(target_os = "macos")]
const APPLY_MAC: &str = r#"pid="$1"; zip="$2"; app="$3"
work=$(dirname "$zip")
parent=$(dirname "$app")
stage="$parent/.Veylo.app.new.$$"
cleanup() { rm -rf "$work" "$stage"; }
trap cleanup EXIT
for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
mkdir -p "$stage" && ditto -x -k "$zip" "$stage" || { open "$app"; exit 1; }
[ -d "$stage/Veylo.app" ] && [ ! -L "$stage/Veylo.app" ] || { open "$app"; exit 1; }
rm -rf "$app.old"
if mv "$app" "$app.old"; then
    if mv "$stage/Veylo.app" "$app"; then
        rm -rf "$app.old"
        xattr -dr com.apple.quarantine "$app" 2>/dev/null
    else
        mv "$app.old" "$app" 2>/dev/null
    fi
fi
open "$app"
"#;

/// The Veylo.app this copy runs from (Veylo.app/Contents/MacOS/veylo-dx).
#[cfg(target_os = "macos")]
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.ancestors().nth(3)?;
    app.extension().is_some_and(|e| e == "app").then(|| app.to_path_buf())
}

#[cfg(target_os = "macos")]
pub async fn install(release: Release) -> Result<Outcome> {
    let app = app_bundle().context("Only an installed Veylo.app can update itself: run the install command again")?;
    let parent = app.parent().context("Couldn't locate Veylo.app's folder")?;
    // Verify the target folder is writable before quitting so we don't fail silently after exit.
    let probe = parent.join(format!(".veylo-update-probe-{}", std::process::id()));
    std::fs::File::create(&probe).context("Veylo doesn't have permission to update its Applications folder")?;
    let _ = std::fs::remove_file(&probe);
    let work = update_work_dir(&release.version)?;
    let zip = work.join(ASSET.context("No update for this platform")?);
    let script = work.join("apply.sh");
    download(release.url.clone(), zip.clone(), release.sha256.clone()).await?;
    std::fs::write(&script, APPLY_MAC).context("Couldn't save the update")?;
    crate::backend::flush_progress().await;
    crate::backend::flush_writes();
    std::process::Command::new("/bin/sh").arg(&script).arg(std::process::id().to_string()).arg(&zip).arg(&app)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().context("Couldn't start the updater")?;
    std::process::exit(0);
}

/// Linux builds from source: nothing to install (the notice never shows there).
#[cfg(not(any(target_os = "android", target_os = "windows", target_os = "macos")))]
pub async fn install(_release: Release) -> Result<Outcome> { Err(anyhow!("Update Veylo by building it again")) }

#[derive(Clone, PartialEq)]
enum Step { Offer, Later, Working, Permission, Failed(String) }

/// "Veylo x.y is available" with Update, above the player's status toast.
#[component]
pub fn UpdateNotice() -> Element {
    let latest = use_resource(|| async { check().await });
    let mut step = use_signal(|| Step::Offer);
    let Some(Some(release)) = latest() else { return rsx! {} };
    let working = step() == Step::Working;
    let (icon_warn, line, action) = match step() {
        Step::Failed(error) => (true, error, "Try again"),
        Step::Permission => (false, "Allow Veylo to install updates, then press Update again".to_owned(), "Update"),
        Step::Working => (false, format!("Downloading Veylo {}…", release.version), "Updating…"),
        _ => (false, format!("Veylo {} is available", release.version), "Update"),
    };
    rsx! {
        if step() != Step::Later {
            div { class: "toast glass update-toast", role: "status",
                span { class: if icon_warn { "warn-icon" } else { "lamp" }, if icon_warn { icons::Alert {} } else { icons::Refresh {} } }
                span { class: "toast-title", "{line}" }
                button { class: "pill accent", disabled: working,
                    onclick: move |_| {
                        let release = release.clone();
                        step.set(Step::Working);
                        spawn(async move {
                            match install(release).await {
                                Ok(Outcome::NeedsPermission) => step.set(Step::Permission),
                                // Android's installer takes over the screen; the notice steps aside.
                                Ok(Outcome::Installing) => step.set(Step::Later),
                                Err(e) => step.set(Step::Failed(format!("{e}"))),
                            }
                        });
                    },
                    if working { span { class: "spinner", aria_hidden: "true" } }
                    span { "{action}" }
                }
                if !working { icons::IconButton { label: "Later", onclick: move |_| step.set(Step::Later), icons::Close {} } }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{newer, parse_github_digest, parse_sha256sums, safe_version};
    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("v0.10.0", "0.9.3"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("latest", "0.1.0"));
        assert!(newer("2.40.0-beta.1", "2.38.1"));
        assert!(!newer("1.9", "1.10"));
        assert!(!newer("0.8.0-rc1", "0.8.0"));
    }

    #[test]
    fn release_urls_stay_on_github_over_https() {
        use super::is_allowed_release_url as ok;
        assert!(ok("https://github.com/Aleinto97/veylo-releases/releases/download/v1/a.zip"));
        assert!(ok("https://objects.githubusercontent.com/x"));
        assert!(ok("https://release-assets.githubusercontent.com/x"));
        assert!(!ok("http://github.com/x"));
        assert!(!ok("https://evilgithubusercontent.com/x"));
        assert!(!ok("https://github.com.evil.example/x"));
        assert!(!ok("https://user:pw@github.com/x"));
        assert!(!ok("https://github.com:8443/x"));
        assert!(!ok("https://127.0.0.1/x"));
    }

    #[test]
    fn sanitizes_versions_and_parses_checksums() {
        assert_eq!(safe_version("0.10.1"), "0.10.1");
        assert_eq!(safe_version("../../etc/passwd"), "update");
        assert_eq!(safe_version("0.10.1/..\\bad"), "0.10.1..bad");
        let hex = "a".repeat(64);
        assert_eq!(parse_github_digest(Some(&format!("sha256:{hex}"))), Some(hex.clone()));
        assert_eq!(parse_github_digest(Some("sha256:short")), None);
        let sums = format!("{hex}  veylo-windows-x64.zip\n");
        assert_eq!(parse_sha256sums(&sums, "veylo-windows-x64.zip"), Some(hex));
        assert_eq!(parse_sha256sums(&sums, "other.zip"), None);
    }

    #[test]
    fn download_verifies_sha256_and_deletes_corrupt_files() {
        use sha2::{Digest, Sha256};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let payload = b"veylo release payload bytes for integrity check";
        let good_hash = format!("{:x}", Sha256::digest(payload));
        let bad_hash = "0".repeat(64);
        let dir = std::env::temp_dir().join(format!("veylo-updater-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let mut req = [0u8; 1024];
                    let _ = sock.read(&mut req).await;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.write_all(payload).await;
                    let _ = sock.shutdown().await;
                }
            });
            let url = format!("http://127.0.0.1:{port}/release.zip");
            let ok_path = dir.join("good.zip");
            super::verify_download_to(url.clone(), ok_path.clone(), good_hash.clone()).await.expect("matching SHA-256 must pass");
            assert_eq!(std::fs::read(&ok_path).unwrap(), payload);
            assert!(super::file_matches_sha256(&ok_path, &good_hash));
            assert!(!super::file_matches_sha256(&ok_path, &bad_hash));

            let bad_path = dir.join("bad.zip");
            let err = super::verify_download_to(url.clone(), bad_path.clone(), bad_hash).await.unwrap_err();
            assert!(err.to_string().contains("verification"), "{err}");
            assert!(!bad_path.exists(), "corrupted download must be deleted");

            // Fail closed: empty/missing checksum must be rejected before writing anything.
            let missing_path = dir.join("missing.zip");
            let err2 = super::verify_download_to(url, missing_path.clone(), String::new()).await.unwrap_err();
            assert!(err2.to_string().contains("checksum"), "{err2}");
            assert!(!missing_path.exists());
            server.await.unwrap();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
