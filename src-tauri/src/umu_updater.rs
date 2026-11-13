//! A managed umu-run for machines without one: the port of src/main/services/umu-updater.ts,
//! umu-release.ts and umu-update-schedule.ts. Upstream bundles a umu-run and updates it;
//! Chimaera bundles none, so a system `umu-run` on PATH is used first and the managed
//! zipapp (from umu-launcher's GitHub releases) only when there is none.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const RELEASE_API_URL: &str = "https://api.github.com/repos/Open-Wine-Components/umu-launcher/releases/latest";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_DOWNLOAD_BYTES: usize = 20 * 1024 * 1024;
const ZIPAPP_ENTRY: &str = "umu/umu-run";
const CHECK_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
const RETRY_BASE_DELAY_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub download_url: String,
    pub sha256: String,
}

/// state.json, in upstream's camelCase shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub version: Option<String>,
    pub checked_at: u64,
    pub failure_count: u32,
    pub retry_at: Option<u64>,
}

/// parseUmuUpdateState: no valid `checkedAt` means no state.
fn parse_state(value: &Value) -> Option<State> {
    Some(State {
        version: value["version"].as_str().filter(|v| is_valid_version(v)).map(str::to_string),
        checked_at: value["checkedAt"].as_f64().filter(|n| n.is_finite() && *n >= 0.0)? as u64,
        failure_count: value["failureCount"].as_f64().filter(|n| n.is_finite() && *n > 0.0).map_or(0, |n| n.floor() as u32),
        retry_at: value["retryAt"].as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64),
    })
}

fn is_valid_version(v: &str) -> bool {
    !v.is_empty() && v.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// compareUmuVersions: numeric segments, missing ones count as 0.
pub fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let parts = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (l, r) = (parts(left), parts(right));
    (0..l.len().max(r.len())).map(|i| l.get(i).unwrap_or(&0).cmp(r.get(i).unwrap_or(&0))).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal)
}

/// parseUmuRelease: the zipapp asset of a final release, with a GitHub URL and a sha256 digest.
pub fn parse_release(payload: &Value) -> Option<Release> {
    if payload["draft"] == true || payload["prerelease"].as_bool().unwrap_or(false) {
        return None;
    }
    for asset in payload["assets"].as_array()? {
        let Some(version) = asset["name"].as_str().and_then(|n| n.strip_prefix("umu-launcher-")).and_then(|n| n.strip_suffix("-zipapp.tar")).filter(|v| is_valid_version(v)) else { continue };
        let sha256 = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()));
        let url = asset["browser_download_url"].as_str().filter(|u| u.starts_with("https://github.com/"));
        let (Some(sha256), Some(url)) = (sha256, url) else { return None };
        return Some(Release { version: version.to_string(), download_url: url.to_string(), sha256: sha256.to_lowercase() });
    }
    None
}

pub fn is_check_due(state: Option<&State>, now: u64) -> bool {
    match state {
        None => true,
        Some(State { retry_at: Some(at), .. }) => now >= *at,
        Some(s) => now.saturating_sub(s.checked_at) >= CHECK_INTERVAL_MS,
    }
}

fn retry_delay(failure_count: u32) -> u64 {
    RETRY_BASE_DELAY_MS.saturating_mul(1u64 << failure_count.saturating_sub(1).min(16)).min(CHECK_INTERVAL_MS)
}

pub fn record_success(version: Option<String>, now: u64) -> State {
    State { version, checked_at: now, failure_count: 0, retry_at: None }
}

pub fn record_failure(state: Option<&State>, installed: Option<String>, now: u64) -> State {
    let failure_count = state.map_or(0, |s| s.failure_count) + 1;
    State { version: installed, checked_at: state.map_or(0, |s| s.checked_at), failure_count, retry_at: Some(now + retry_delay(failure_count)) }
}

fn managed_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("umu")
}

fn version_binary(data_dir: &Path, version: &str) -> PathBuf {
    managed_dir(data_dir).join(version).join("umu-run")
}

fn read_state(data_dir: &Path) -> Option<State> {
    parse_state(&serde_json::from_str(&std::fs::read_to_string(managed_dir(data_dir).join("state.json")).ok()?).ok()?)
}

fn write_state(data_dir: &Path, state: &State) -> Result<(), String> {
    let dir = managed_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!("state.json.{}.tmp", uuid::Uuid::new_v4()));
    let body = json!({ "version": state.version, "checkedAt": state.checked_at, "failureCount": state.failure_count, "retryAt": state.retry_at });
    std::fs::write(&temp, body.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, dir.join("state.json")).map_err(|e| e.to_string())
}

fn is_usable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn installed_version(data_dir: &Path, state: Option<&State>) -> Option<String> {
    state?.version.clone().filter(|v| is_usable(&version_binary(data_dir, v)))
}

/// UmuUpdater.getManagedBinaryPath.
pub fn managed_binary(data_dir: &Path) -> Option<PathBuf> {
    installed_version(data_dir, read_state(data_dir).as_ref()).map(|v| version_binary(data_dir, &v))
}

/// getCompatiblePythonPath: umu's zipapp needs Python 3.10 or newer.
fn compatible_python() -> Option<String> {
    ["/usr/bin/python3", "python3"].into_iter().find_map(|candidate| {
        let mut command = std::process::Command::new(candidate);
        crate::proton::use_host_env(&mut command).args(["-c", "import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let out = command.output().ok().filter(|o| o.status.success())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (major, minor) = text.trim().split_once('.')?;
        let (major, minor): (u32, u32) = (major.parse().ok()?, minor.parse().ok()?);
        (major > 3 || (major == 3 && minor >= 10)).then(|| candidate.to_string())
    })
}

/// The command that runs umu-run: the system one, else the managed zipapp (through Python when it can).
pub fn umu_program(data_dir: &Path) -> Option<Vec<String>> {
    if crate::proton::on_path("umu-run") {
        return Some(vec!["umu-run".into()]);
    }
    let managed = managed_binary(data_dir)?.to_string_lossy().into_owned();
    Some(match compatible_python() {
        Some(python) => vec![python, managed],
        None => vec![managed],
    })
}

fn extract_zipapp(archive: &[u8], target: &Path) -> Result<(), String> {
    let mut tar = tar::Archive::new(archive);
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if entry.path().map_err(|e| e.to_string())?.to_string_lossy() == ZIPAPP_ENTRY {
            let mut file = std::fs::File::create(target).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    Err(format!("archive is missing {ZIPAPP_ENTRY}"))
}

async fn install_release(data_dir: &Path, client: &reqwest::Client, release: &Release) -> Result<(), String> {
    let response = client.get(&release.download_url).timeout(DOWNLOAD_TIMEOUT).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
    if response.content_length().is_some_and(|n| n as usize > MAX_DOWNLOAD_BYTES) {
        return Err(format!("umu {} archive is too large", release.version));
    }
    let archive = response.bytes().await.map_err(|e| e.to_string())?;
    if archive.len() > MAX_DOWNLOAD_BYTES {
        return Err(format!("umu {} archive is too large", release.version));
    }
    let digest: String = Sha256::digest(&archive).iter().map(|b| format!("{b:02x}")).collect();
    if digest != release.sha256 {
        return Err(format!("umu {} archive digest mismatch", release.version));
    }
    let work = managed_dir(data_dir).join(format!(".download-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let extracted = work.join("umu-run");
    let result = extract_zipapp(&archive, &extracted).and_then(|()| {
        if !is_usable(&extracted) {
            return Err(format!("umu {} archive is missing {ZIPAPP_ENTRY}", release.version));
        }
        let target = version_binary(data_dir, &release.version);
        let dir = target.parent().unwrap();
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::rename(&extracted, &target).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn remove_stale_versions(data_dir: &Path, keep: &[&str]) {
    let Ok(entries) = std::fs::read_dir(managed_dir(data_dir)) else { return };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && !keep.contains(&name.as_str()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

static CHECKING: AtomicBool = AtomicBool::new(false);

/// UmuUpdater.checkForUpdates: Linux only, once a day (backing off after failures), and only
/// when there is no system umu-run to use instead.
pub async fn check_for_updates(data_dir: PathBuf) {
    if !cfg!(target_os = "linux") || crate::proton::on_path("umu-run") || CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    let state = read_state(&data_dir);
    if is_check_due(state.as_ref(), crate::library::now_ms()) {
        let installed = installed_version(&data_dir, state.as_ref());
        if let Err(error) = run_check(&data_dir, installed.clone()).await {
            eprintln!("[umu] update failed: {error}");
            let _ = write_state(&data_dir, &record_failure(state.as_ref(), installed, crate::library::now_ms()));
        }
    }
    CHECKING.store(false, Ordering::SeqCst);
}

async fn run_check(data_dir: &Path, installed: Option<String>) -> Result<(), String> {
    // GitHub's API refuses requests without a User-Agent.
    let client = reqwest::Client::builder().user_agent("Chimaera").build().map_err(|e| e.to_string())?;
    let payload: Value = client.get(RELEASE_API_URL).header("Accept", "application/vnd.github+json").timeout(REQUEST_TIMEOUT).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let release = parse_release(&payload).filter(|r| installed.as_deref().is_none_or(|v| compare_versions(&r.version, v).is_gt()));
    let Some(release) = release else {
        return write_state(data_dir, &record_success(installed, crate::library::now_ms()));
    };
    eprintln!("[umu] installing umu-launcher {}", release.version);
    install_release(data_dir, &client, &release).await?;
    write_state(data_dir, &record_success(Some(release.version.clone()), crate::library::now_ms()))?;
    let previous = installed.unwrap_or_else(|| release.version.clone());
    remove_stale_versions(data_dir, &[&release.version, &previous]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    fn zipapp() -> Value {
        json!({
            "name": "umu-launcher-1.4.4-zipapp.tar",
            "browser_download_url": "https://github.com/Open-Wine-Components/umu-launcher/releases/download/1.4.4/umu-launcher-1.4.4-zipapp.tar",
            "digest": "sha256:eb590691841f7fad3fc3ad8fd5db4ccb87849fe7948e62b28ece7a4ee48cc851",
        })
    }

    #[test]
    fn reads_the_zipapp_asset() {
        let release = parse_release(&json!({ "draft": false, "prerelease": false, "assets": [{ "name": "umu-launcher-1.4.4.fc43.x86_64.rpm", "digest": "sha256:00" }, zipapp()] }));
        assert_eq!(release, Some(Release { version: "1.4.4".into(), download_url: zipapp()["browser_download_url"].as_str().unwrap().into(), sha256: "eb590691841f7fad3fc3ad8fd5db4ccb87849fe7948e62b28ece7a4ee48cc851".into() }));
    }

    #[test]
    fn rejects_unverifiable_or_unfinished_releases() {
        let mut no_digest = zipapp();
        no_digest["digest"] = Value::Null;
        let mut elsewhere = zipapp();
        elsewhere["browser_download_url"] = json!("https://example.com/umu.tar");
        assert_eq!(parse_release(&json!({ "assets": [no_digest] })), None);
        assert_eq!(parse_release(&json!({ "assets": [elsewhere] })), None);
        assert_eq!(parse_release(&json!({ "prerelease": true, "assets": [zipapp()] })), None);
        assert_eq!(parse_release(&json!({ "assets": [] })), None);
        assert_eq!(parse_release(&Value::Null), None);
    }

    #[test]
    fn state_needs_checked_at() {
        assert_eq!(parse_state(&json!({ "version": "1.4.4", "checkedAt": 5, "failureCount": 2.7, "retryAt": null })), Some(State { version: Some("1.4.4".into()), checked_at: 5, failure_count: 2, retry_at: None }));
        assert_eq!(parse_state(&json!({ "version": "../x", "checkedAt": 1 })).unwrap().version, None);
        assert_eq!(parse_state(&json!({ "version": "1.4.4" })), None);
    }

    #[test]
    fn compares_numeric_segments() {
        assert_eq!(compare_versions("1.4.4", "1.3.0"), Greater);
        assert_eq!(compare_versions("1.4.10", "1.4.9"), Greater);
        assert_eq!(compare_versions("1.4", "1.4.0"), Equal);
        assert_eq!(compare_versions("1.3.0", "1.4.4"), Less);
    }

    #[test]
    fn schedule_backs_off_and_caps_at_a_day() {
        assert!(is_check_due(None, 0));
        let ok = record_success(Some("1.4.4".into()), 1_000);
        assert!(!is_check_due(Some(&ok), 1_000 + CHECK_INTERVAL_MS - 1));
        assert!(is_check_due(Some(&ok), 1_000 + CHECK_INTERVAL_MS));
        let failed = record_failure(Some(&ok), Some("1.4.4".into()), 5_000);
        assert_eq!((failed.failure_count, failed.retry_at, failed.checked_at), (1, Some(5_000 + RETRY_BASE_DELAY_MS), 1_000));
        let again = record_failure(Some(&failed), None, 5_000);
        assert_eq!(again.retry_at, Some(5_000 + 2 * RETRY_BASE_DELAY_MS));
        assert_eq!(retry_delay(30), CHECK_INTERVAL_MS);
    }

    #[test]
    fn extracts_only_the_zipapp() {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in [("umu/README", &b"x"[..]), (ZIPAPP_ENTRY, &b"#!/usr/bin/env python3"[..])] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_cksum();
            builder.append_data(&mut header, path, body).unwrap();
        }
        let archive = builder.into_inner().unwrap();
        let target = std::env::temp_dir().join(format!("chimaera-umu-zipapp-{}", uuid::Uuid::new_v4()));
        extract_zipapp(&archive, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"#!/usr/bin/env python3");
        let _ = std::fs::remove_file(&target);
        assert!(extract_zipapp(&tar::Builder::new(Vec::new()).into_inner().unwrap(), &target).is_err());
    }
}
