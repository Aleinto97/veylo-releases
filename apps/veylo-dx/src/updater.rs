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
pub const ASSET: Option<&str> = if cfg!(target_os = "android") { Some("veylo-android-arm64.apk") }
    else if cfg!(target_os = "windows") { Some("veylo-windows-x64.zip") }
    else if cfg!(all(target_os = "macos", target_arch = "aarch64")) { Some("veylo-macos-arm64.zip") }
    else { None };

#[derive(Clone, Debug, PartialEq)]
pub struct Release { pub version: String, url: String }

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

fn client() -> reqwest::Client {
    reqwest::Client::builder().user_agent("veylo-dx").build().unwrap_or_default()
}

/// The latest release when it is newer than this build and has this platform's file; None offline or up to date.
pub async fn check() -> Option<Release> {
    let asset = ASSET?;
    crate::backend::on_runtime(async move {
        let body = client().get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
            .header("Accept", "application/vnd.github+json")
            .send().await.ok()?.error_for_status().ok()?.bytes().await.ok()?;
        let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
        let version = v["tag_name"].as_str()?.trim_start_matches('v').to_owned();
        if !newer(&version, env!("CARGO_PKG_VERSION")) { return None; }
        let url = v["assets"].as_array()?.iter().find(|a| a["name"] == asset)?["browser_download_url"].as_str()?.to_owned();
        Some(Release { version, url })
    }).await.ok().flatten()
}

#[cfg_attr(not(any(target_os = "android", target_os = "windows", target_os = "macos")), allow(dead_code))]
/// Stream the release file to `to` (an APK is ~150 MB: never held in memory).
async fn download(url: String, to: PathBuf) -> Result<()> {
    crate::backend::on_runtime(async move {
        use std::io::Write;
        let mut response = client().get(&url).send().await.and_then(|r| r.error_for_status())
            .map_err(|_| anyhow!("Couldn't download the update"))?;
        if let Some(dir) = to.parent() { std::fs::create_dir_all(dir).context("Couldn't save the update")?; }
        let mut file = std::fs::File::create(&to).context("Couldn't save the update")?;
        while let Some(chunk) = response.chunk().await.map_err(|_| anyhow!("The download was interrupted"))? {
            file.write_all(&chunk).context("Couldn't save the update")?;
        }
        file.sync_all().context("Couldn't save the update")
    }).await?
}

/// What Install led to, when it returns at all (Windows quits to swap its files).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))] // Windows and macOS restart instead of returning
pub enum Outcome { Installing, NeedsPermission }

#[cfg(target_os = "android")]
pub async fn install(release: Release) -> Result<Outcome> {
    let apk = crate::android::files_dir()?.join("updates").join(ASSET.context("No update for this platform")?);
    // Already downloaded (the viewer went to allow installs and came back): use it.
    let fresh = std::fs::metadata(&apk).map(|m| m.len() > 0).unwrap_or(false)
        && std::fs::read_to_string(apk.with_extension("version")).ok().as_deref() == Some(release.version.as_str());
    if !fresh {
        download(release.url.clone(), apk.clone()).await?;
        std::fs::write(apk.with_extension("version"), &release.version).context("Couldn't save the update")?;
    }
    match crate::android::install_update(&apk)?.as_str() {
        "permission" => Ok(Outcome::NeedsPermission),
        _ => Ok(Outcome::Installing),
    }
}

/// Waits for Veylo to close, unpacks the new build over the old one and starts it again.
#[cfg(target_os = "windows")]
const APPLY: &str = r#"param([int]$ProcessId, [string]$Zip, [string]$Dest)
try { Wait-Process -Id $ProcessId -Timeout 30 -ErrorAction SilentlyContinue } catch {}
Start-Sleep -Milliseconds 500
for ($i = 0; $i -lt 10; $i++) {
    try { Expand-Archive -Force -Path $Zip -DestinationPath $Dest -ErrorAction Stop; break } catch { Start-Sleep -Seconds 1 }
}
Start-Process (Join-Path $Dest 'veylo-dx.exe')
Remove-Item -Recurse -Force (Split-Path $Zip) -ErrorAction SilentlyContinue
"#;

/// Download the release, hand it to the helper and quit so the files can be replaced.
#[cfg(target_os = "windows")]
pub async fn install(release: Release) -> Result<Outcome> {
    let exe = std::env::current_exe().context("Couldn't find Veylo's folder")?;
    let dest: &std::path::Path = exe.parent().context("Couldn't find Veylo's folder")?;
    let work = std::env::temp_dir().join(format!("veylo-update-{}", release.version));
    let zip = work.join(ASSET.context("No update for this platform")?);
    let script = work.join("apply.ps1");
    download(release.url.clone(), zip.clone()).await?;
    std::fs::write(&script, APPLY).context("Couldn't save the update")?;
    let mut helper = std::process::Command::new("powershell");
    helper.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"]).arg(&script)
        .arg("-ProcessId").arg(std::process::id().to_string()).arg("-Zip").arg(&zip).arg("-Dest").arg(dest);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        helper.creation_flags(CREATE_NO_WINDOW);
    }
    helper.spawn().context("Couldn't start the updater")?;
    std::process::exit(0);
}

/// macOS: waits for Veylo to close, swaps Veylo.app for the new one and opens it again.
#[cfg(target_os = "macos")]
const APPLY_MAC: &str = r#"pid="$1"; zip="$2"; app="$3"
for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
work=$(dirname "$zip")
rm -rf "$work/new" && mkdir -p "$work/new" && ditto -x -k "$zip" "$work/new" || exit 1
[ -d "$work/new/Veylo.app" ] || exit 1
rm -rf "$app.old" && mv "$app" "$app.old" && mv "$work/new/Veylo.app" "$app" && rm -rf "$app.old"
xattr -dr com.apple.quarantine "$app" 2>/dev/null
open "$app"
rm -rf "$work"
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
    let work = std::env::temp_dir().join(format!("veylo-update-{}", release.version));
    let zip = work.join(ASSET.context("No update for this platform")?);
    let script = work.join("apply.sh");
    download(release.url.clone(), zip.clone()).await?;
    std::fs::write(&script, APPLY_MAC).context("Couldn't save the update")?;
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
                    "{action}"
                }
                if !working { icons::IconButton { label: "Later", onclick: move |_| step.set(Step::Later), icons::Close {} } }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::newer;
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
}
