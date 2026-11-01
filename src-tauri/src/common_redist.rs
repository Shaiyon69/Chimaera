//! Common redistributables (VC++, DirectX, OpenAL, .NET 4, XNA): downloaded once, installed from
//! settings or before a game's first launch. Ported from src/main/services/common-redist-manager.ts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::launch::LAUNCHER_LABEL;
use crate::store::Store;

const FILES: &[&str] = &["dotNetFx40_Full_setup.exe", "directx_Jun2010_redist.exe", "oalinst.exe", "install.bat", "xnafx40_redist.msi", "VisualCppRedist_AIO_x86_x64.exe"];
const SOURCE: &str = "https://github.com/hydralauncher/hydra-common-redist/raw/refs/heads/main";
const PASSED_KEY: &str = "commonRedistPassed";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DONE_MESSAGE: &str = "Installation complete";

/// commonRedistPath (Electron's folder when only that one exists, so its downloads are reused).
pub fn dir(app: &AppHandle) -> Result<PathBuf, String> {
    crate::save_backups::data_dir(app, "CommonRedist")
}

/// install.bat writes its progress to %TEMP%\common_redist_install.log.
fn log_path() -> PathBuf {
    std::env::temp_dir().join("common_redist_install.log")
}

/// canInstallCommonRedist: every installer file is present.
pub fn can_install(app: &AppHandle) -> bool {
    dir(app).is_ok_and(|dir| FILES.iter().all(|f| dir.join(f).exists()))
}

pub fn reset_preflight(store: &Store) -> Result<(), String> {
    store.del("", PASSED_KEY)
}

/// downloadCommonRedist: fetches missing files; install.bat is always refreshed.
pub async fn download(app: &AppHandle) -> Result<(), String> {
    let dir = dir(app)?;
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let client = reqwest::Client::new();
    for file in FILES {
        let path = dir.join(file);
        if path.exists() && *file != "install.bat" {
            continue;
        }
        let response = client.get(format!("{SOURCE}/{file}")).send().await.and_then(|r| r.error_for_status()).map_err(|e| format!("{file}: {e}"))?;
        let bytes = response.bytes().await.map_err(|e| format!("{file}: {e}"))?;
        // Written beside, then renamed, so a cut-off download never passes the presence check.
        let partial = dir.join(format!("{file}.part"));
        tokio::fs::write(&partial, &bytes).await.map_err(|e| e.to_string())?;
        tokio::fs::rename(&partial, &path).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// checkSystemRedistributables: the names of runtimes missing from this Windows install.
fn missing_on_system(windows: &Path) -> Vec<&'static str> {
    let system32 = windows.join("System32");
    let checks: [(&str, bool); 5] = [
        ("Visual C++ Runtime", system32.join("vcruntime140.dll").exists() && system32.join("msvcp140.dll").exists()),
        ("DirectX June 2010", system32.join("d3dx9_43.dll").exists()),
        ("OpenAL", system32.join("OpenAL32.dll").exists()),
        (".NET Framework 4.0", windows.join(r"Microsoft.NET\Framework\v4.0.30319\clr.dll").exists()),
        (
            "XNA Framework 4.0",
            windows.join(r"Microsoft.NET\assembly\GAC_32\Microsoft.Xna.Framework").exists() || windows.join(r"Microsoft.NET\assembly\GAC_MSIL\Microsoft.Xna.Framework").exists(),
        ),
    ];
    checks.iter().filter(|(_, ok)| !ok).map(|(name, _)| *name).collect()
}

fn windows_dir() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
}

/// The line before the trailing newline, as Electron reads the log tail.
fn log_tail(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    lines.len().checked_sub(2).map(|i| lines[i].trim().to_string()).filter(|l| !l.is_empty())
}

/// Runs install.bat (it elevates itself) and follows its log until "Installation complete",
/// calling `progress` with each new tail. Returns whether it completed within 5 minutes.
async fn run_installer(app: &AppHandle, mut progress: impl FnMut(&str, bool)) -> bool {
    let Ok(dir) = dir(app) else { return false };
    let log = log_path();
    if !log.exists() {
        let _ = std::fs::write(&log, "");
    }
    let mut command = std::process::Command::new("cmd.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.raw_arg(format!("/d /s /c \"\"{}\"\"", dir.join("install.bat").display())).creation_flags(CREATE_NO_WINDOW);
    }
    if let Err(error) = command.current_dir(&dir).spawn() {
        eprintln!("[common-redist] could not run install.bat: {error}");
        return false;
    }
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < INSTALL_TIMEOUT {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Some(tail) = std::fs::read_to_string(&log).ok().as_deref().and_then(log_tail) else { continue };
        if tail != last {
            let done = tail.contains(DONE_MESSAGE);
            progress(&tail, done);
            if done {
                return true;
            }
            last = tail;
        }
    }
    eprintln!("[common-redist] installation timed out");
    false
}

/// `installCommonRedist` from settings: progress goes to the main window as common-redist-progress.
pub fn install(app: &AppHandle) -> Result<(), String> {
    if !can_install(app) {
        return Ok(());
    }
    reset_preflight(&app.state::<Store>())?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let emitter = app.clone();
        let completed = run_installer(&app, move |log, complete| {
            let _ = emitter.emit("common-redist-progress", [json!({ "log": log, "complete": complete })]);
        })
        .await;
        if !completed {
            let _ = app.emit("common-redist-progress", [json!({ "log": "Installation timed out", "complete": false })]);
        }
    });
    Ok(())
}

fn preflight_progress(app: &AppHandle, status: &str, detail: Option<&str>) {
    let _ = app.emit_to(LAUNCHER_LABEL, "preflight-progress", [json!({ "status": status, "detail": detail })]);
}

/// runPreflight before a launch: download, check the system, install what is missing once.
/// Never fails the launch; the result only decides whether it runs again next time.
pub async fn preflight(app: &AppHandle) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let store = app.state::<Store>();
    if store.get("", PASSED_KEY).ok() == Some(Value::Bool(true)) {
        preflight_progress(app, "complete", None);
        return true;
    }
    preflight_progress(app, "checking", None);
    if !can_install(app) {
        preflight_progress(app, "downloading", None);
        if let Err(error) = download(app).await {
            eprintln!("[common-redist] download failed: {error}");
            preflight_progress(app, "error", Some("download_failed"));
            return false;
        }
    }
    let missing = missing_on_system(&windows_dir());
    if !missing.is_empty() {
        eprintln!("[common-redist] missing on this system: {missing:?}");
        preflight_progress(app, "installing", None);
        let launcher = app.clone();
        if !run_installer(app, move |log, _| preflight_progress(&launcher, "installing", Some(log))).await {
            preflight_progress(app, "error", Some("install_failed"));
            return false;
        }
    }
    let _ = store.put("", PASSED_KEY, &json!(true));
    preflight_progress(app, "complete", None);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_log_tail_like_electron() {
        assert_eq!(log_tail("\r\nInstalling DirectX\r\nInstallation complete\r\n").as_deref(), Some("Installation complete"));
        assert_eq!(log_tail("Installing OpenAL"), None);
        assert_eq!(log_tail(""), None);
    }

    #[test]
    fn reports_every_runtime_missing_from_an_empty_windows_folder() {
        let empty = std::env::temp_dir().join("chimaera-no-windows");
        assert_eq!(missing_on_system(&empty).len(), 5);
    }
}
