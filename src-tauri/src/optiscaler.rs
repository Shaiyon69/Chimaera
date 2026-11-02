//! OptiScaler per game (port of `src/main/services/optiscaler/` and its events):
//! release download, FSR4 override DLLs, install with backup and rollback,
//! the `OptiScaler.ini` allowlist, and the startup auto-update.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::sync::{Mutex, OnceCell};

use crate::games::game_key;
use crate::store::Store;

pub(crate) const MARKER: &str = "optiscaler_manager.json";
const BACKUP: &str = ".optiscaler_backup";
const CLEANUP: &str = ".optiscaler_cleanup";
// Files OptiScaler writes or ships that a game never has on its own.
const LEFTOVER_NAMES: &[&str] = &["OptiScaler.ini", "OptiScaler.log", "fakenvapi.ini", "fakenvapi.log"];
const FSR4_DLL: &str = "amd_fidelityfx_upscaler_dx12.dll";
const PROXIES: &[&str] = &["dxgi.dll", "winmm.dll", "version.dll", "dbghelp.dll", "d3d12.dll", "wininet.dll", "winhttp.dll"];
const SKIP: &[&str] = &["unins", "crash", "launcher", "redist", "unitycrash", "vc_redist", "dxsetup", "eac", "easyanticheat", "benchmark"];
const AC_NAMES: &[&str] = &["easyanticheat", "beservice", "battleye", "vgk", "mhyprot", "equ8", "xigncode", "nprotect"];
const SHIPPING_SUFFIX: &str = "-win64-shipping.exe";
const RELEASES_API: &str = "https://api.github.com/repos/optiscaler/OptiScaler/releases";
// 100, not Electron's 30: the repo posts several timing-kit releases a day, which push the dll- tag off a short page.
const FSR4_API: &str = "https://api.github.com/repos/zgauthier2000/fsr4-shader-performance-overrides/releases?per_page=100";
const FSR4_TAG_PREFIX: &str = "dll-";
const TAG_TTL: Duration = Duration::from_secs(60 * 60);
const OPTIPATCHER_API: &str = "https://api.github.com/repos/optiscaler/OptiPatcher/releases/latest";
// OptiDllPath defaults to the exe folder, so OptiScaler looks for ASI files in `<exe dir>/plugins`.
const OPTIPATCHER_REL: &str = "plugins/OptiPatcher.asi";

// ---------- detection ----------

pub(crate) fn walk(root: &Path, max_depth: usize, visit: &mut dyn FnMut(&Path, &fs::DirEntry) -> bool) {
    fn rec(dir: &Path, depth: usize, max: usize, visit: &mut dyn FnMut(&Path, &fs::DirEntry) -> bool) -> bool {
        let Ok(entries) = fs::read_dir(dir) else { return true };
        for e in entries.flatten() {
            if !visit(dir, &e) {
                return false;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            if is_dir && !e.file_name().to_string_lossy().starts_with('.') && depth + 1 < max && !rec(&e.path(), depth + 1, max, visit) {
                return false;
            }
        }
        true
    }
    rec(root, 0, max_depth, visit);
}

/// Largest non-launcher exe up to 5 levels deep; prefers Unreal `*-Win64-Shipping.exe`.
fn find_main_exe(game_dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    walk(game_dir, 5, &mut |_, e| {
        let low = e.file_name().to_string_lossy().to_lowercase();
        if e.file_type().is_ok_and(|t| t.is_file()) && low.ends_with(".exe") && !SKIP.iter().any(|s| low.contains(s)) {
            if let Ok(meta) = e.metadata() {
                let size = meta.len() + if low.ends_with(SHIPPING_SUFFIX) { 1 << 40 } else { 0 };
                if best.as_ref().is_none_or(|(b, _)| size > *b) {
                    best = Some((size, e.path()));
                }
            }
        }
        true
    });
    best.map(|(_, p)| p)
}

/// Heuristic graphics API from the DLL names embedded in the exe.
/// dx10/dx9 come after vulkan so Vulkan games that also name d3d9 keep their OptiScaler default.
pub(crate) fn detect_api(exe: &Path) -> &'static str {
    let Ok(bytes) = fs::read(exe) else { return "unknown" };
    let lower = bytes.to_ascii_lowercase();
    let has = |name: &str| {
        let ascii = name.as_bytes().to_vec();
        let wide: Vec<u8> = name.bytes().flat_map(|b| [b, 0]).collect();
        [ascii, wide].iter().any(|n| lower.windows(n.len()).any(|w| w == n.as_slice()))
    };
    [("d3d12", "dx12"), ("d3d11", "dx11"), ("vulkan-1", "vulkan"), ("d3d10", "dx10"), ("d3d9", "dx9")].into_iter().find(|(n, _)| has(n)).map_or("unknown", |(_, a)| a)
}

/// Name of the first anti-cheat file or folder within 4 levels.
pub(crate) fn detect_anticheat(game_dir: &Path) -> Option<String> {
    let mut hit = None;
    walk(game_dir, 4, &mut |_, e| {
        let name = e.file_name().to_string_lossy().into_owned();
        if AC_NAMES.iter().any(|a| name.to_lowercase().contains(a)) {
            hit = Some(name);
            return false;
        }
        true
    });
    hit
}

/// Unreal puts the exe in `<root>/<Game>/Binaries/Win64`; anti-cheat lives at the root.
pub(crate) fn game_root_from_exe_dir(exe_dir: &Path) -> PathBuf {
    let parts: Vec<_> = exe_dir.components().collect();
    match parts.iter().position(|p| p.as_os_str().to_string_lossy().eq_ignore_ascii_case("binaries")) {
        Some(i) if i > 1 => parts[..i - 1].iter().collect(),
        _ => exe_dir.to_path_buf(),
    }
}

// ---------- GPU ----------

/// `RX` + optional spaces + 4 digits whose first matches `first`.
fn has_rx(lower: &str, first: impl Fn(char) -> bool) -> bool {
    lower.match_indices("rx").any(|(i, _)| {
        let rest = lower[i + 2..].trim_start();
        let digits: Vec<char> = rest.chars().take(4).collect();
        digits.len() == 4 && digits.iter().all(char::is_ascii_digit) && first(digits[0])
    })
}

/// `Arc A770`-style names: `arc`, spaces, A or B, 2-3 digits, then a word boundary.
fn has_arc(lower: &str) -> bool {
    lower.match_indices("arc").any(|(i, _)| {
        let rest = lower[i + 3..].trim_start();
        let mut chars = rest.chars();
        if !matches!(chars.next(), Some('a' | 'b')) {
            return false;
        }
        let tail: String = chars.collect();
        let n = tail.chars().take_while(char::is_ascii_digit).count();
        (2..=3).contains(&n) && !tail[n..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
    })
}

/// "native": RDNA4 runs AMD's own FSR4. "desktop"/"igpu" pick the override zip; the best card wins.
fn classify_gpus(names: &[String]) -> &'static str {
    let lows: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    if lows.iter().any(|n| has_rx(n, |d| d == '9') || n.contains("r9700") || n.contains("radeon ai pro")) {
        return "native";
    }
    let desktop = |n: &String| has_rx(n, |d| ('5'..='8').contains(&d)) || ["geforce", "rtx", "gtx", "quadro"].iter().any(|k| n.contains(k)) || has_arc(n);
    if lows.iter().any(desktop) {
        return "desktop";
    }
    // Unknown or undetected falls back to desktop, the pre-detection behavior.
    if names.is_empty() { "desktop" } else { "igpu" }
}

async fn gpu_names() -> &'static [String] {
    static NAMES: OnceCell<Vec<String>> = OnceCell::const_new();
    NAMES
        .get_or_init(|| async {
            tauri::async_runtime::spawn_blocking(|| {
                let mut command = std::process::Command::new("powershell");
                command.args(["-NoProfile", "-Command", "(Get-CimInstance Win32_VideoController).Name"]);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                    command.creation_flags(CREATE_NO_WINDOW);
                }
                command.output().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()).unwrap_or_default()
            })
            .await
            .unwrap_or_default()
        })
        .await
}

async fn detect_gpu() -> &'static str {
    classify_gpus(gpu_names().await)
}

fn prefs(app: &AppHandle) -> Value {
    app.state::<Store>().get("", "userPreferences").unwrap_or(Value::Null)
}

fn nightly(prefs: &Value) -> bool {
    prefs["optiscalerNightly"].as_bool().unwrap_or(false)
}

fn variant(prefs: &Value) -> &'static str {
    if prefs["optiscalerFsr4Variant"] == "lossy" { "lossy" } else { "exact" }
}

/// Manual override from settings, else the detected GPU.
async fn resolve_gpu(prefs: &Value) -> String {
    match prefs["optiscalerFsr4Gpu"].as_str() {
        Some(chosen @ ("native" | "desktop" | "igpu")) => chosen.to_string(),
        _ => detect_gpu().await.to_string(),
    }
}

// ---------- downloads ----------

fn cache_root(app: &AppHandle) -> Result<PathBuf, String> {
    crate::save_backups::data_dir(app, "optiscaler")
}

fn github() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent("Chimaera").build().map_err(|e| e.to_string())
}

pub(crate) async fn github_json(url: &str, timeout: u64) -> Result<Value, String> {
    github()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(timeout))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn download(url: &str, dest: &Path, timeout: u64) -> Result<(), String> {
    let bytes = github()?.get(url).timeout(Duration::from_secs(timeout)).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?.bytes().await.map_err(|e| e.to_string())?;
    fs::write(dest, bytes).map_err(|e| e.to_string())
}

async fn extract(app: &AppHandle, archive: &Path, dest: &Path) -> Result<(), String> {
    let binary = crate::extraction::seven_zip(app)?;
    let (archive, dest) = (archive.to_path_buf(), dest.to_path_buf());
    tauri::async_runtime::spawn_blocking(move || crate::extraction::run_7z(&binary, &archive, &dest, &mut |_| {})).await.map_err(|e| e.to_string())?
}

fn release_url(nightly: bool) -> String {
    if nightly { format!("{RELEASES_API}?per_page=1") } else { format!("{RELEASES_API}/latest") }
}

fn release_of(data: &Value, nightly: bool) -> &Value {
    if nightly { &data[0] } else { data }
}

fn find_payload(root: &Path) -> Option<PathBuf> {
    let mut hit = None;
    walk(root, usize::MAX, &mut |dir, e| {
        if e.file_name() == "OptiScaler.dll" && e.file_type().is_ok_and(|t| t.is_file()) {
            hit = Some(dir.to_path_buf());
            return false;
        }
        true
    });
    hit
}

/// Newest release tag without downloading; null offline. Cached an hour (GitHub allows 60 calls/h).
async fn latest_tag(nightly: bool) -> Option<String> {
    static CACHE: Mutex<Option<HashMap<bool, (String, Instant)>>> = Mutex::const_new(None);
    let mut cache = CACHE.lock().await;
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some((tag, at)) = map.get(&nightly).filter(|(_, at)| at.elapsed() < TAG_TTL) {
        let _ = at;
        return Some(tag.clone());
    }
    let data = github_json(&release_url(nightly), 10).await.ok()?;
    let tag = release_of(&data, nightly)["tag_name"].as_str()?.to_string();
    map.insert(nightly, (tag.clone(), Instant::now()));
    Some(tag)
}

/// Release tag plus extracted payload folder; the newest cached release when offline.
/// The lock makes status warm-up and install share one download.
async fn fetch_latest(app: &AppHandle, nightly: bool) -> Result<(String, PathBuf), String> {
    static RESOLVED: Mutex<Option<HashMap<bool, (String, PathBuf, Instant)>>> = Mutex::const_new(None);
    let mut resolved = RESOLVED.lock().await;
    let map = resolved.get_or_insert_with(HashMap::new);
    if let Some((tag, payload, _)) = map.get(&nightly).filter(|(_, p, at)| at.elapsed() < TAG_TTL && find_payload(p).is_some()) {
        return Ok((tag.clone(), payload.clone()));
    }
    let cache = cache_root(app)?.join("cache");
    let result = match download_latest(app, &cache, nightly).await {
        Ok(found) => found,
        Err(err) => newest_cached_release(&cache).ok_or(err)?,
    };
    map.insert(nightly, (result.0.clone(), result.1.clone(), Instant::now()));
    Ok(result)
}

async fn download_latest(app: &AppHandle, cache: &Path, nightly: bool) -> Result<(String, PathBuf), String> {
    let data = github_json(&release_url(nightly), 30).await?;
    let release = release_of(&data, nightly);
    let tag = release["tag_name"].as_str().ok_or("No OptiScaler release found")?.to_string();
    let dest = cache.join(format!("OptiScaler_{tag}"));
    if let Some(payload) = find_payload(&dest) {
        return Ok((tag, payload));
    }
    let asset = release["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str().is_some_and(|n| n.to_lowercase().ends_with(".7z"))).ok_or(format!("No .7z asset in release {tag}"))?;
    let (Some(name), Some(url)) = (asset["name"].as_str(), asset["browser_download_url"].as_str()) else { return Err("Bad release asset".into()) };
    fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let archive = cache.join(name);
    download(url, &archive, 60).await?;
    let _ = fs::remove_dir_all(&dest);
    let extracted = extract(app, &archive, &dest).await;
    let _ = fs::remove_file(&archive);
    extracted.map_err(|_| format!("Failed to extract OptiScaler {tag}"))?;
    let payload = find_payload(&dest).ok_or("OptiScaler.dll not found in extracted release")?;
    Ok((tag, payload))
}

/// Newest `OptiScaler_<tag>` folder by natural order.
fn newest_cached_release(cache: &Path) -> Option<(String, PathBuf)> {
    let mut tags: Vec<String> = fs::read_dir(cache).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|d| d.starts_with("OptiScaler_")).collect();
    tags.sort_by(|a, b| natural_cmp(b, a));
    tags.into_iter().find_map(|d| find_payload(&cache.join(&d)).map(|p| (d["OptiScaler_".len()..].to_string(), p)))
}

/// localeCompare with `numeric: true`, enough for version tags.
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match out.last_mut() {
                Some((d, text)) if *d == digit => text.push(c),
                _ => out.push((digit, c.to_string())),
            }
        }
        out
    }
    for (x, y) in chunks(a).iter().zip(chunks(b).iter()) {
        let ord = match (x, y) {
            ((true, x), (true, y)) => x.trim_start_matches('0').len().cmp(&y.trim_start_matches('0').len()).then_with(|| x.trim_start_matches('0').cmp(y.trim_start_matches('0'))),
            ((_, x), (_, y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        };
        if ord.is_ne() {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

// ---------- FSR4 override DLLs ----------

fn fsr4_archive(family: &str) -> &'static str {
    if family == "igpu" { "fsr4.1.1-igpu-cyboman.zip" } else { "fsr4.1.1-cyboman.zip" }
}

fn fsr4_dll_path(root: &Path, tag: &str, family: &str, variant: &str) -> PathBuf {
    root.join(tag).join(family).join(variant).join(FSR4_DLL)
}

fn has_family(root: &Path, tag: &str, family: &str) -> bool {
    ["exact", "lossy"].iter().all(|v| fsr4_dll_path(root, tag, family, v).is_file())
}

fn newest_fsr4_tag(root: &Path, family: &str) -> Option<String> {
    let mut tags: Vec<String> = fs::read_dir(root).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|t| t.starts_with(FSR4_TAG_PREFIX) && has_family(root, t, family)).collect();
    tags.sort_by(|a, b| natural_cmp(a, b));
    tags.pop()
}

/// Downloaded DLL for a variant, without touching the network.
fn cached_fsr4_dll(app: &AppHandle, family: &str, variant: &str) -> Option<PathBuf> {
    let root = cache_root(app).ok()?.join("fsr4");
    newest_fsr4_tag(&root, family).map(|tag| fsr4_dll_path(&root, &tag, family, variant))
}

/// `sha256sum` output as relative path -> hash.
fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (hash, rest) = l.split_once(char::is_whitespace)?;
            let name = rest.trim_start().trim_start_matches('*').trim();
            (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !name.is_empty()).then(|| (name.to_string(), hash.to_lowercase()))
        })
        .collect()
}

fn sha256_file(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

/// Fetches the newest FSR4 DLL release for a GPU family; the newest cached one when offline.
async fn ensure_fsr4_dlls(app: &AppHandle, family: &str) -> Option<String> {
    static LOCK: Mutex<()> = Mutex::const_new(());
    let _guard = LOCK.lock().await;
    let root = cache_root(app).ok()?.join("fsr4");
    match fetch_fsr4_dlls(app, &root, family).await {
        Ok(tag) => Some(tag),
        Err(err) => {
            eprintln!("[optiscaler] failed to fetch FSR4 DLLs: {err}");
            newest_fsr4_tag(&root, family)
        }
    }
}

async fn fetch_fsr4_dlls(app: &AppHandle, root: &Path, family: &str) -> Result<String, String> {
    let archive_name = fsr4_archive(family);
    let releases = github_json(FSR4_API, 30).await?;
    let release = releases.as_array().into_iter().flatten().find(|r| r["tag_name"].as_str().is_some_and(|t| t.starts_with(FSR4_TAG_PREFIX))).ok_or("No FSR4 DLL release found")?;
    let tag = release["tag_name"].as_str().unwrap_or_default().to_string();
    if has_family(root, &tag, family) {
        return Ok(tag);
    }
    let asset = |name: &str| release["assets"].as_array().into_iter().flatten().find(|a| a["name"] == name).and_then(|a| a["browser_download_url"].as_str()).map(str::to_string);
    let (Some(archive_url), Some(sums_url)) = (asset(archive_name), asset("SHA256SUMS")) else {
        return Err(format!("Release {tag} is missing {archive_name} or SHA256SUMS"));
    };
    // Staged per family so desktop and igpu archives can share one tag folder.
    let tmp = root.join(format!("{tag}.{family}.tmp"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let archive = tmp.join(archive_name);
    download(&archive_url, &archive, 120).await?;
    let sums_text = github()?.get(&sums_url).timeout(Duration::from_secs(30)).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
    let sums = parse_sums(&sums_text);
    if sha256_file(&archive).as_ref() != sums.get(archive_name) {
        return Err(format!("Checksum mismatch for {archive_name}"));
    }
    extract(app, &archive, &tmp).await.map_err(|_| format!("Failed to extract {archive_name}"))?;
    let _ = fs::remove_file(&archive);
    let mut found = Vec::new();
    for variant in ["exact", "lossy"] {
        let key = format!("{family}/{variant}/{FSR4_DLL}");
        // Archives hold either <family>/<variant>/ or <variant>/ at the root.
        let file = [tmp.join(family).join(variant).join(FSR4_DLL), tmp.join(variant).join(FSR4_DLL)].into_iter().find(|f| f.is_file());
        match file {
            Some(f) if sha256_file(&f).as_ref() == sums.get(&key) => found.push((variant, f)),
            _ => return Err(format!("Bad or missing {key} in {archive_name}")),
        }
    }
    let _ = fs::remove_dir_all(root.join(&tag).join(family));
    for (variant, file) in found {
        let target = fsr4_dll_path(root, &tag, family, variant);
        fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::rename(file, target).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_dir_all(&tmp);
    Ok(tag)
}

/// The user's chosen DLL if it still exists, else the downloaded one. Native GPUs need none.
fn resolve_fsr4_dll(app: &AppHandle, prefs: &Value, gpu: &str) -> Option<PathBuf> {
    if gpu == "native" {
        return None;
    }
    let v = variant(prefs);
    let chosen = if v == "lossy" { prefs["optiscalerFsr4LossyPath"].as_str() } else { prefs["optiscalerFsr4ExactPath"].as_str().or(prefs["optiscalerFsr4DllPath"].as_str()) };
    chosen.map(PathBuf::from).filter(|p| p.exists()).or_else(|| cached_fsr4_dll(app, gpu, v))
}

// ---------- OptiPatcher ----------

/// Newest OptiPatcher.asi (a rolling release, so the asset id is the version); the newest cached one offline.
async fn ensure_optipatcher(app: &AppHandle) -> Result<PathBuf, String> {
    static LOCK: Mutex<()> = Mutex::const_new(());
    let _guard = LOCK.lock().await;
    let root = cache_root(app)?.join("optipatcher");
    match fetch_optipatcher(&root).await {
        Ok(file) => Ok(file),
        Err(err) => {
            let mut ids: Vec<String> = fs::read_dir(&root).ok().into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|d| root.join(d).join("OptiPatcher.asi").is_file()).collect();
            ids.sort_by(|a, b| natural_cmp(b, a));
            ids.first().map(|d| root.join(d).join("OptiPatcher.asi")).ok_or(err)
        }
    }
}

async fn fetch_optipatcher(root: &Path) -> Result<PathBuf, String> {
    let release = github_json(OPTIPATCHER_API, 15).await?;
    let asset = release["assets"].as_array().into_iter().flatten().find(|a| a["name"] == "OptiPatcher.asi").ok_or("No OptiPatcher.asi in the release")?;
    let (Some(id), Some(url)) = (asset["id"].as_u64(), asset["browser_download_url"].as_str()) else { return Err("Bad OptiPatcher asset".into()) };
    let file = root.join(id.to_string()).join("OptiPatcher.asi");
    if file.is_file() {
        return Ok(file);
    }
    let tmp = root.join(format!("{id}.tmp"));
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let staged = tmp.join("OptiPatcher.asi");
    download(url, &staged, 60).await?;
    // GitHub publishes "sha256:<hex>" for each asset; older assets may lack it.
    if let Some(want) = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")) {
        if sha256_file(&staged).as_deref() != Some(want.to_lowercase().as_str()) {
            let _ = fs::remove_dir_all(&tmp);
            return Err("Checksum mismatch for OptiPatcher.asi".into());
        }
    }
    fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::rename(&staged, &file).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(file)
}

/// Adds one file to an existing install, backing up a foreign file it replaces.
fn add_tracked_file(exe_dir: &Path, rel: &str, src: &Path) -> Result<(), String> {
    let marker = read_marker(exe_dir).ok_or("OptiScaler is not installed")?;
    let (mut files, mut backed) = (strings(&marker["files"]), strings(&marker["backed_up"]));
    let dst = inside(exe_dir, rel).ok_or("Bad path")?;
    fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
    if dst.exists() && !files.iter().any(|f| f == rel) {
        let b = exe_dir.join(BACKUP).join(rel);
        fs::create_dir_all(b.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::rename(&dst, &b).map_err(|e| e.to_string())?;
        backed.push(rel.into());
    }
    fs::copy(src, &dst).map_err(|e| e.to_string())?;
    if !files.iter().any(|f| f == rel) {
        files.push(rel.into());
    }
    write_marker(exe_dir, marker["version"].as_str().unwrap_or_default(), marker["proxy"].as_str().unwrap_or_default(), &files, &backed)
}

/// Removes one tracked file and restores what it replaced.
fn remove_tracked_file(exe_dir: &Path, rel: &str) -> Result<(), String> {
    let marker = read_marker(exe_dir).ok_or("OptiScaler is not installed")?;
    let (mut files, mut backed) = (strings(&marker["files"]), strings(&marker["backed_up"]));
    if let Some(p) = inside(exe_dir, rel) {
        let _ = fs::remove_file(&p);
        if backed.iter().any(|b| b == rel) {
            if let Some(src) = inside(&exe_dir.join(BACKUP), rel) {
                fs::rename(src, &p).map_err(|e| e.to_string())?;
            }
        }
    }
    files.retain(|f| f != rel);
    backed.retain(|b| b != rel);
    write_marker(exe_dir, marker["version"].as_str().unwrap_or_default(), marker["proxy"].as_str().unwrap_or_default(), &files, &backed)
}

fn has_tracked_file(exe_dir: &Path, rel: &str) -> bool {
    read_marker(exe_dir).is_some_and(|m| strings(&m["files"]).iter().any(|f| f == rel))
}

/// `setOptiPatcher`: adds or removes OptiPatcher.asi; adding also turns on ASI loading.
pub async fn set_optipatcher(app: &AppHandle, shop: &str, object_id: &str, enabled: bool) -> Result<(), String> {
    let dir = context(app, shop, object_id)?.1;
    read_marker(&dir).ok_or("OptiScaler is not installed")?;
    if !enabled {
        return remove_tracked_file(&dir, OPTIPATCHER_REL);
    }
    let asi = ensure_optipatcher(app).await?;
    add_tracked_file(&dir, OPTIPATCHER_REL, &asi)?;
    let ini = dir.join("OptiScaler.ini");
    if ini_get_set(&ini, "LoadAsiPlugins", None).is_some() {
        ini_get_set(&ini, "LoadAsiPlugins", Some("true"));
    }
    Ok(())
}

// ---------- cleanup ----------

/// Cheap hint for the UI: no marker of ours, but OptiScaler's own files are there.
fn has_leftovers(exe_dir: &Path) -> bool {
    read_marker(exe_dir).is_none() && (LEFTOVER_NAMES[..2].iter().any(|n| exe_dir.join(n).is_file()) || exe_dir.join("D3D12_Optiscaler").is_dir())
}

/// Files in `exe_dir` byte-identical to a file in one of `releases` (OptiScaler.dll may sit
/// under any proxy name), plus OptiScaler's own ini/log files. Relative paths with `/`.
fn find_leftovers(exe_dir: &Path, releases: &[PathBuf]) -> Vec<String> {
    let mut known: HashMap<u64, Vec<(PathBuf, Option<String>)>> = HashMap::new();
    let mut names: Vec<(String, u64)> = Vec::new();
    for release in releases {
        for rel in list_files(release) {
            let src = release.join(&rel);
            let Ok(size) = fs::metadata(&src).map(|m| m.len()) else { continue };
            known.entry(size).or_default().push((src, None));
            let rel = if rel.eq_ignore_ascii_case("optiscaler.dll") { String::new() } else { rel };
            names.push((rel, size));
        }
    }
    let mut candidates: Vec<String> = names.iter().filter(|(r, _)| !r.is_empty()).map(|(r, _)| r.clone()).collect();
    candidates.extend(PROXIES.iter().map(|p| p.to_string()));
    candidates.sort();
    candidates.dedup();
    let mut out: Vec<String> = LEFTOVER_NAMES.iter().filter(|n| exe_dir.join(n).is_file()).map(|n| n.to_string()).collect();
    for rel in candidates {
        let Some(path) = inside(exe_dir, &rel).filter(|p| p.is_file()) else { continue };
        let Ok(size) = fs::metadata(&path).map(|m| m.len()) else { continue };
        let Some(same_size) = known.get_mut(&size) else { continue };
        // Hash only files whose size matches something we shipped.
        let Some(hash) = sha256_file(&path) else { continue };
        if same_size.iter_mut().any(|(src, h)| h.get_or_insert_with(|| sha256_file(src).unwrap_or_default()) == &hash) && !out.contains(&rel) {
            out.push(rel);
        }
    }
    out
}

/// Moves leftovers into `.optiscaler_cleanup/`, so a file the game needed can be put back.
fn move_leftovers(exe_dir: &Path, files: &[String]) -> Result<(), String> {
    let target = exe_dir.join(CLEANUP);
    for rel in files {
        let (Some(src), Some(dst)) = (inside(exe_dir, rel), inside(&target, rel)) else { continue };
        fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_dir(exe_dir.join("D3D12_Optiscaler"));
    Ok(())
}

/// `cleanupOptiscaler`: lists leftovers, or moves them when `apply` is true.
pub async fn cleanup(app: &AppHandle, shop: &str, object_id: &str, apply: bool) -> Result<Value, String> {
    let dir = context(app, shop, object_id)?.1;
    if read_marker(&dir).is_some() {
        return Err("OptiScaler is installed by Chimaera here; use Uninstall instead".into());
    }
    let mut releases: Vec<PathBuf> = fs::read_dir(cache_root(app)?.join("cache")).into_iter().flatten().flatten().map(|e| e.path()).filter_map(|p| find_payload(&p)).collect();
    let fsr4 = cache_root(app)?.join("fsr4");
    if fsr4.is_dir() {
        releases.push(fsr4);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let files = find_leftovers(&dir, &releases);
        if apply {
            move_leftovers(&dir, &files)?;
        }
        Ok(json!({ "files": files }))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- install ----------

pub(crate) fn read_marker(exe_dir: &Path) -> Option<Value> {
    let marker: Value = serde_json::from_str(&fs::read_to_string(exe_dir.join(MARKER)).ok()?).ok()?;
    // Other tools (the Bloodborne port) write a marker of the same name.
    marker["proxy"].as_str().filter(|p| PROXIES.contains(p))?;
    Some(marker)
}

fn write_marker(exe_dir: &Path, tag: &str, proxy: &str, files: &[String], backed: &[String]) -> Result<(), String> {
    let marker = json!({ "version": tag, "proxy": proxy, "files": files, "backed_up": backed });
    fs::write(exe_dir.join(MARKER), serde_json::to_string_pretty(&marker).unwrap()).map_err(|e| e.to_string())
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

/// Files under `root`, relative with `/`.
fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(root, usize::MAX, &mut |_, e| {
        if e.file_type().is_ok_and(|t| t.is_file()) {
            if let Ok(rel) = e.path().strip_prefix(root) {
                out.push(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
            }
        }
        true
    });
    out
}

/// A marker entry must stay inside the game folder; the marker file is not trusted blindly.
pub(crate) fn inside(exe_dir: &Path, rel: &str) -> Option<PathBuf> {
    let path = Path::new(rel);
    path.components().all(|c| matches!(c, std::path::Component::Normal(_))).then(|| exe_dir.join(path))
}

fn uninstall(exe_dir: &Path, keep_ini: bool) -> Result<(), String> {
    let marker = read_marker(exe_dir).ok_or("No install marker found")?;
    let files = strings(&marker["files"]);
    for rel in &files {
        if keep_ini && rel.eq_ignore_ascii_case("optiscaler.ini") {
            continue;
        }
        if let Some(p) = inside(exe_dir, rel) {
            let _ = fs::remove_file(p);
        }
    }
    let backup = exe_dir.join(BACKUP);
    for rel in strings(&marker["backed_up"]) {
        let (Some(src), Some(dst)) = (inside(&backup, &rel), inside(exe_dir, &rel)) else { continue };
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::rename(&src, &dst).map_err(|e| e.to_string())?;
        }
    }
    let _ = fs::remove_dir_all(&backup);
    let _ = fs::remove_file(exe_dir.join(MARKER));
    // Prune the empty folders the install created.
    let mut sorted = files;
    sorted.sort_by_key(|f| std::cmp::Reverse(f.len()));
    for rel in sorted {
        let Some(mut dir) = inside(exe_dir, &rel).and_then(|p| p.parent().map(Path::to_path_buf)) else { continue };
        while dir != exe_dir && dir.starts_with(exe_dir) && fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none()) {
            let _ = fs::remove_dir(&dir);
            dir = match dir.parent() {
                Some(p) => p.to_path_buf(),
                None => break,
            };
        }
    }
    Ok(())
}

/// Copies the payload into `exe_dir`, backing up what it replaces. The marker is written
/// first, so a failure rolls back through `uninstall` to the game's original files.
fn install(payload: &Path, exe_dir: &Path, proxy: &str, tag: &str, fsr4_dll: Option<&Path>, fsr4_mode: Option<&str>, api: &str) -> Result<Vec<(String, String)>, String> {
    let old = read_marker(exe_dir);
    if old.is_none() && exe_dir.join(MARKER).exists() {
        return Err("Another tool's optiscaler_manager.json is in this folder".into());
    }
    if old.is_some() {
        // Restore originals first so the backups stay the true originals.
        uninstall(exe_dir, true)?;
    }
    let old_files = old.as_ref().map(|m| strings(&m["files"])).unwrap_or_default();
    let backup = exe_dir.join(BACKUP);
    let (mut copied, mut backed): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    write_marker(exe_dir, tag, proxy, &copied, &backed)?;

    let mut run = || -> Result<Vec<(String, String)>, String> {
        let back_up = |dst: &Path, rel: &str, backed: &mut Vec<String>| -> Result<(), String> {
            let b = backup.join(rel);
            fs::create_dir_all(b.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::rename(dst, &b).map_err(|e| e.to_string())?;
            backed.push(rel.to_string());
            Ok(())
        };
        for rel in list_files(payload) {
            if rel.starts_with("!!") || rel.to_lowercase().starts_with("setup_") {
                continue;
            }
            let src = payload.join(&rel);
            let rel = if rel.eq_ignore_ascii_case("optiscaler.dll") { proxy.to_string() } else { rel };
            let dst = exe_dir.join(&rel);
            fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
            if dst.exists() {
                if rel.eq_ignore_ascii_case("optiscaler.ini") {
                    // Still ours from the previous install: keep the user's settings.
                    if old_files.iter().any(|f| f == "OptiScaler.ini") {
                        copied.push(rel);
                    }
                    continue;
                }
                back_up(&dst, &rel, &mut backed)?;
            }
            fs::copy(&src, &dst).map_err(|e| e.to_string())?;
            copied.push(rel);
            write_marker(exe_dir, tag, proxy, &copied, &backed)?;
        }
        if let Some(dll) = fsr4_dll {
            let dst = exe_dir.join(FSR4_DLL);
            let ours = copied.iter().any(|f| f == FSR4_DLL);
            if dst.exists() && !ours {
                back_up(&dst, FSR4_DLL, &mut backed)?;
            }
            fs::copy(dll, &dst).map_err(|e| e.to_string())?;
            if !ours {
                copied.push(FSR4_DLL.into());
            }
        }
        write_marker(exe_dir, tag, proxy, &copied, &backed)?;
        Ok(configure_ini(&exe_dir.join("OptiScaler.ini"), api, fsr4_mode))
    };
    run().inspect_err(|_| {
        // Rollback is best effort; the original error matters more.
        let _ = uninstall(exe_dir, false);
    })
}

// ---------- OptiScaler.ini ----------

/// Reads a key. `Section.Key` matches only inside `[Section]` (OptiScaler reuses names like
/// `Enabled`); a plain key matches the first one in any section. With `value`, sets it,
/// appending when missing: at the end of its section, or the file for a plain key.
fn ini_get_set(ini: &Path, key: &str, value: Option<&str>) -> Option<String> {
    let (section, name) = key.split_once('.').map_or((None, key), |(s, k)| (Some(s), k));
    let text = fs::read_to_string(ini).unwrap_or_default();
    let mut lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let (mut current, mut section_end, mut found): (Option<String>, Option<usize>, Option<(usize, String)>) = (None, None, None);
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            current = Some(t[1..t.len() - 1].to_string());
            if section.is_some_and(|s| s.eq_ignore_ascii_case(&t[1..t.len() - 1])) {
                section_end = Some(i + 1);
            }
            continue;
        }
        let in_section = section.is_none_or(|s| current.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(s)));
        if !in_section || t.starts_with(';') {
            continue;
        }
        // Blank lines between sections do not extend the current one.
        if section.is_some() && !t.is_empty() {
            section_end = Some(i + 1);
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == name {
                found = Some((i, v.trim().to_string()));
                break;
            }
        }
    }
    let old = found.as_ref().map(|(_, v)| v.clone());
    if let Some(value) = value {
        let entry = format!("{name}={value}");
        match (found, section, section_end) {
            (Some((i, _)), _, _) => lines[i] = entry,
            (None, Some(_), Some(end)) => lines.insert(end, entry),
            (None, Some(s), None) => lines.extend([format!("[{s}]"), entry]),
            (None, None, _) => lines.push(entry),
        }
        let _ = fs::write(ini, lines.join("\n") + "\n");
    }
    old
}

/// Sets FSR4-related keys that are still "auto"; never overrides the user's values.
fn configure_ini(ini: &Path, api: &str, fsr4: Option<&str>) -> Vec<(String, String)> {
    if !ini.exists() {
        return Vec::new();
    }
    let mut wanted: Vec<(&str, &str)> = Vec::new();
    if let Some(mode) = fsr4 {
        wanted.push(("Fsr4Update", "true"));
        // RDNA4 runs FSR4 natively; INT8 is the fallback for the override DLL on older cards.
        if mode == "dll" {
            wanted.push(("Fsr4ForceEnableInt8", "true"));
        }
        match api {
            "dx12" => wanted.push(("Dx12Upscaler", "fsr31")),
            "dx11" => wanted.push(("Dx11Upscaler", "fsr31_12")),
            "vulkan" => wanted.push(("VulkanUpscaler", "fsr31_12")),
            _ => wanted.extend([("Dx12Upscaler", "fsr31"), ("Dx11Upscaler", "fsr31_12"), ("VulkanUpscaler", "fsr31_12")]),
        }
    }
    wanted.extend([("LogToFile", "true"), ("LogLevel", "2")]);
    let mut done = Vec::new();
    for (k, v) in wanted {
        if ini_get_set(ini, k, None).unwrap_or_default().eq_ignore_ascii_case("auto") {
            ini_get_set(ini, k, Some(v));
            done.push((k.to_string(), v.to_string()));
        }
    }
    done
}

const TRISTATE: &[&str] = &["auto", "true", "false"];

/// Allowlist of user-editable keys; the renderer can only touch these.
/// Option lists follow the comments in OptiScaler 0.9.4's OptiScaler.ini; booleans keep
/// "auto" because OptiScaler picks those per GPU vendor.
fn config_fields() -> Vec<Value> {
    let select = |group: &str, key: &str, options: &[&str]| json!({ "group": group, "key": key, "type": "select", "options": options });
    let tristate = |group: &str, key: &str| select(group, key, TRISTATE);
    let number = |group: &str, key: &str, min: f64, max: f64, step: f64| json!({ "group": group, "key": key, "type": "number", "min": min, "max": max, "step": step });
    let keys = ["auto", "0x2D", "0x24", "0x23", "0x2E", "0x21", "0x22", "0x70", "0x71", "0x72", "0x73", "0x74", "0x75", "0x76", "0x77", "0x78", "0x79", "0x7A", "0x7B", "-1"];
    vec![
        select("upscalers", "Dx12Upscaler", &["auto", "xess", "fsr21", "fsr22", "fsr31", "dlss"]),
        select("upscalers", "Dx11Upscaler", &["auto", "fsr22", "fsr31", "xess", "xess_12", "fsr21_12", "fsr22_12", "fsr31_12", "dlss"]),
        select("upscalers", "VulkanUpscaler", &["auto", "fsr21", "fsr22", "fsr31", "xess", "fsr21_12", "fsr31_12", "dlss"]),
        tristate("upscalers", "Fsr4Update"),
        tristate("upscalers", "Fsr4ForceEnableInt8"),
        tristate("upscalers", "OverrideSharpness"),
        number("upscalers", "Sharpness", 0.0, 1.3, 0.05),
        tristate("framegen", "FrameGen.Enabled"),
        // 0.9.4 split the old FGType into an input and an output.
        select("framegen", "FGInput", &["auto", "nofg", "dlssg", "nukems", "fsrfg", "upscaler", "fsrfg30"]),
        select("framegen", "FGOutput", &["auto", "nofg", "fsrfg", "xefg", "nukems"]),
        number("framegen", "FramerateLimit", 0.0, 1000.0, 1.0),
        select("overlay", "ShortcutKey", &keys),
        tristate("overlay", "ShowFps"),
        select("overlay", "FpsOverlayType", &["auto", "0", "1", "2", "3", "4", "5", "6"]),
        tristate("overlay", "DisableOverlays"),
        tristate("spoofing", "Dxgi"),
        tristate("spoofing", "StreamlineSpoofing"),
        tristate("spoofing", "OverrideNvapiDll"),
        tristate("plugins", "LoadAsiPlugins"),
        tristate("plugins", "LoadReshade"),
        tristate("plugins", "LoadSpecialK"),
        tristate("log", "LogToFile"),
        select("log", "LogLevel", &["auto", "0", "1", "2", "3", "4"]),
    ]
}

fn read_config(ini: &Path) -> Value {
    Value::Array(
        config_fields()
            .into_iter()
            .filter_map(|mut f| {
                let value = ini_get_set(ini, f["key"].as_str()?, None)?;
                f["value"] = json!(value);
                Some(f)
            })
            .collect(),
    )
}

/// Checks a value against the allowlist; numbers also accept "auto" (OptiScaler's default).
fn check_value(key: &str, value: &str) -> Result<(), String> {
    let field = config_fields().into_iter().find(|f| f["key"] == key).ok_or(format!("Unsupported setting: {key}"))?;
    let valid = match field["type"].as_str() {
        Some("select") => field["options"].as_array().is_some_and(|o| o.iter().any(|v| v == value)),
        _ => value == "auto" || value.trim().parse::<f64>().is_ok_and(|n| n >= field["min"].as_f64().unwrap_or(0.0) && n <= field["max"].as_f64().unwrap_or(0.0)),
    };
    if valid { Ok(()) } else { Err(format!("Invalid value for {key}: {value}")) }
}

fn write_config_value(ini: &Path, key: &str, value: &str) -> Result<(), String> {
    check_value(key, value)?;
    ini_get_set(ini, key, None).ok_or(format!("Unsupported setting: {key}"))?;
    ini_get_set(ini, key, Some(value));
    Ok(())
}

// ---------- profiles ----------

const PROFILES_KEY: &str = "optiscalerProfiles";

/// Shipped profiles; the renderer names them by id. "amd-fg" turns DLSS-FG games into
/// FSR3 FG through NukemFG and fakenvapi, both already in the OptiScaler release.
fn builtin_profiles() -> Vec<Value> {
    vec![
        json!({ "id": "default", "builtin": true, "values": {} }),
        json!({ "id": "amd-fg", "builtin": true, "values": { "FrameGen.Enabled": "true", "FGInput": "nukems", "FGOutput": "nukems", "OverrideNvapiDll": "true" } }),
    ]
}

fn stored_profiles(app: &AppHandle) -> Vec<Value> {
    app.state::<Store>().get("", PROFILES_KEY).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

/// `getOptiscalerProfiles`.
pub fn profiles(app: &AppHandle) -> Value {
    Value::Array(builtin_profiles().into_iter().chain(stored_profiles(app)).collect())
}

/// Keeps only allowlisted keys with valid values.
fn clean_values(values: &Value) -> Result<Value, String> {
    let map = values.as_object().ok_or("Profile values must be an object")?;
    let mut out = serde_json::Map::new();
    for (k, v) in map {
        let v = v.as_str().ok_or(format!("Invalid value for {k}"))?;
        check_value(k, v)?;
        out.insert(k.clone(), json!(v));
    }
    Ok(Value::Object(out))
}

/// `saveOptiscalerProfile`: creates a profile, or replaces the one with `id`.
pub fn save_profile(app: &AppHandle, id: Option<&str>, name: &str, values: &Value) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err("A profile name needs 1 to 60 characters".into());
    }
    if id.is_some_and(|id| builtin_profiles().iter().any(|p| p["id"] == id)) {
        return Err("Built-in profiles cannot be changed".into());
    }
    let profile = json!({ "id": id.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_string), "name": name, "builtin": false, "values": clean_values(values)? });
    let mut list = stored_profiles(app);
    match list.iter_mut().find(|p| p["id"] == profile["id"]) {
        Some(existing) => *existing = profile.clone(),
        None => list.push(profile.clone()),
    }
    app.state::<Store>().put("", PROFILES_KEY, &Value::Array(list))?;
    Ok(profile)
}

/// `deleteOptiscalerProfile`.
pub fn delete_profile(app: &AppHandle, id: &str) -> Result<(), String> {
    let mut list = stored_profiles(app);
    list.retain(|p| p["id"] != id);
    app.state::<Store>().put("", PROFILES_KEY, &Value::Array(list))
}

/// Writes a profile's values; keys missing from this game's ini (older OptiScaler) are skipped.
fn apply_values(ini: &Path, values: &Value) -> Vec<String> {
    let mut skipped = Vec::new();
    for (k, v) in values.as_object().into_iter().flatten() {
        if write_config_value(ini, k, v.as_str().unwrap_or_default()).is_err() {
            skipped.push(k.clone());
        }
    }
    skipped
}

/// `applyOptiscalerProfile`.
pub fn apply_profile(app: &AppHandle, shop: &str, object_id: &str, id: &str) -> Result<Value, String> {
    let profile = profiles(app).as_array().into_iter().flatten().find(|p| p["id"] == id).cloned().ok_or("Profile not found")?;
    let skipped = apply_values(&ini_path(app, shop, object_id)?, &profile["values"]);
    Ok(json!({ "skipped": skipped }))
}

/// "live" if OptiScaler.log (newer than the install) shows the FSR4 4.1.x DLL, "loaded" if only OptiScaler ran.
fn loaded_state(exe_dir: &Path) -> Option<&'static str> {
    let log = exe_dir.join("OptiScaler.log");
    let (log_meta, marker_meta) = (fs::metadata(&log).ok()?, fs::metadata(exe_dir.join(MARKER)).ok()?);
    if !log_meta.is_file() || log_meta.modified().ok()? < marker_meta.modified().ok()? {
        return None;
    }
    let bytes = fs::read(&log).ok()?;
    let tail = &bytes[bytes.len().saturating_sub(2 << 20)..];
    Some(if String::from_utf8_lossy(tail).contains("4.1.1") { "live" } else { "loaded" })
}

// ---------- game context and channels ----------

/// Unreal games ship a root stub that starts `Binaries/Win64/*-Win64-Shipping.exe`; the proxy must sit next to the real process.
fn resolve_injected_exe(exe: PathBuf) -> PathBuf {
    if exe.to_string_lossy().to_lowercase().ends_with(SHIPPING_SUFFIX) {
        return exe;
    }
    match exe.parent().and_then(find_main_exe) {
        Some(main) if main.to_string_lossy().to_lowercase().ends_with(SHIPPING_SUFFIX) => main,
        _ => exe,
    }
}

pub(crate) fn context(app: &AppHandle, shop: &str, object_id: &str) -> Result<(PathBuf, PathBuf), String> {
    let game = app.state::<Store>().get("games", &game_key(shop, object_id))?;
    let exe = game["executablePath"].as_str().filter(|p| !p.is_empty()).ok_or("Game has no executable path")?;
    // A .bat launcher is not the injected process; target the tracked exe.
    let tracked = game["trackingExecutablePaths"].as_array().into_iter().flatten().filter_map(Value::as_str).find(|p| !p.is_empty());
    let chosen = if crate::launch::is_batch_file(exe) { tracked.unwrap_or(exe) } else { exe };
    let exe = resolve_injected_exe(PathBuf::from(chosen));
    let dir = exe.parent().map(Path::to_path_buf).ok_or("Game has no executable path")?;
    Ok((exe, dir))
}

/// `getOptiscalerStatus`.
pub async fn status(app: &AppHandle, shop: &str, object_id: &str, proxy: Option<&str>) -> Result<Value, String> {
    let (exe, dir) = context(app, shop, object_id)?;
    let prefs = prefs(app);
    let marker = read_marker(&dir);
    let gpu = resolve_gpu(&prefs).await;
    let night = nightly(&prefs);
    {
        // Warm the caches so the first install already has the files.
        let (app, gpu) = (app.clone(), gpu.clone());
        tauri::async_runtime::spawn(async move {
            if gpu != "native" {
                ensure_fsr4_dlls(&app, &gpu).await;
            }
            let _ = fetch_latest(&app, night).await;
        });
    }
    let latest = latest_tag(night).await;
    let version = marker.as_ref().map(|m| m["version"].clone());
    let update = matches!((&version, &latest), (Some(Value::String(v)), Some(l)) if v != l);
    // A foreign DLL under the chosen proxy name would be moved to backup.
    let conflict = proxy.filter(|p| marker.is_none() && dir.join(p).exists());
    Ok(json!({
        "installed": marker.is_some(),
        "version": version.unwrap_or(Value::Null),
        "proxy": marker.as_ref().map_or(Value::Null, |m| m["proxy"].clone()),
        "loaded": if marker.is_some() { json!(loaded_state(&dir)) } else { Value::Null },
        "api": detect_api(&exe),
        "anticheat": detect_anticheat(&game_root_from_exe_dir(&dir)),
        "proxies": PROXIES,
        "latestTag": latest,
        "updateAvailable": update,
        "fsr4Available": gpu == "native" || resolve_fsr4_dll(app, &prefs, &gpu).is_some(),
        "fsr4Gpu": gpu,
        "fsr4Variant": variant(&prefs),
        "proxyConflict": conflict,
        "dxvkInstalled": crate::dxvk::installed(&dir),
        "optipatcher": has_tracked_file(&dir, OPTIPATCHER_REL),
        "leftovers": has_leftovers(&dir),
        // Ours never counts: after an install the proxy is OptiScaler, not ReShade.
        "reshade": if marker.is_some() { None } else { crate::graphics_scan::reshade_dll(&dir) },
    }))
}

/// `getOptiscalerGpu`.
pub async fn gpu(app: &AppHandle) -> Value {
    let effective = resolve_gpu(&prefs(app)).await;
    let native = effective == "native";
    if !native {
        // Fetch now so settings can show the DLLs that install will use.
        ensure_fsr4_dlls(app, &effective).await;
    }
    let dll = |v: &str| if native { Value::Null } else { json!(cached_fsr4_dll(app, &effective, v).map(|p| p.to_string_lossy().into_owned())) };
    json!({ "detected": detect_gpu().await, "effective": effective, "autoDlls": { "exact": dll("exact"), "lossy": dll("lossy") } })
}

/// `installOptiscaler`.
pub async fn install_for_game(app: &AppHandle, shop: &str, object_id: &str, proxy: &str) -> Result<Value, String> {
    if !PROXIES.contains(&proxy) {
        return Err(format!("Invalid proxy: {proxy}"));
    }
    let (exe, dir) = context(app, shop, object_id)?;
    // One per game: DXVK replaces dxgi.dll too, and chaining the two is not supported.
    if crate::dxvk::installed(&dir) {
        return Err("DXVK is installed in this game; uninstall it first".into());
    }
    if read_marker(&dir).is_none() && crate::graphics_scan::reshade_dll(&dir).as_deref() == Some(proxy) {
        return Err(format!("ReShade is installed as {proxy}; pick another injection DLL"));
    }
    let prefs = prefs(app);
    let (tag, payload) = fetch_latest(app, nightly(&prefs)).await?;
    let gpu = resolve_gpu(&prefs).await;
    if gpu != "native" {
        ensure_fsr4_dlls(app, &gpu).await;
    }
    let dll = resolve_fsr4_dll(app, &prefs, &gpu);
    let mode = if gpu == "native" { Some("native") } else if dll.is_some() { Some("dll") } else { None };
    let api = detect_api(&exe);
    let proxy = proxy.to_string();
    // A reinstall removes every tracked file, OptiPatcher included; put it back afterwards.
    let had_optipatcher = has_tracked_file(&dir, OPTIPATCHER_REL);
    let result = tauri::async_runtime::spawn_blocking(move || install(&payload, &dir, &proxy, &tag, dll.as_deref(), mode, api).map(|_| json!({ "version": tag })))
        .await
        .map_err(|e| e.to_string())??;
    if had_optipatcher {
        if let Err(err) = set_optipatcher(app, shop, object_id, true).await {
            eprintln!("[optiscaler] could not restore OptiPatcher: {err}");
        }
    }
    Ok(result)
}

/// `uninstallOptiscaler`.
pub fn uninstall_for_game(app: &AppHandle, shop: &str, object_id: &str) -> Result<(), String> {
    uninstall(&context(app, shop, object_id)?.1, false)
}

fn ini_path(app: &AppHandle, shop: &str, object_id: &str) -> Result<PathBuf, String> {
    let dir = context(app, shop, object_id)?.1;
    read_marker(&dir).ok_or("OptiScaler is not installed")?;
    Ok(dir.join("OptiScaler.ini"))
}

/// `getOptiscalerConfig`.
pub fn get_config(app: &AppHandle, shop: &str, object_id: &str) -> Result<Value, String> {
    let ini = ini_path(app, shop, object_id)?;
    Ok(if ini.exists() { read_config(&ini) } else { json!([]) })
}

/// `setOptiscalerConfig`.
pub fn set_config(app: &AppHandle, shop: &str, object_id: &str, key: &str, value: &str) -> Result<(), String> {
    write_config_value(&ini_path(app, shop, object_id)?, key, value)
}

/// Startup: fetch the FSR4 DLLs, then bring every installed game up to the newest release.
pub async fn auto_update(app: &AppHandle) {
    let prefs = prefs(app);
    let gpu = resolve_gpu(&prefs).await;
    if gpu != "native" {
        ensure_fsr4_dlls(app, &gpu).await;
    }
    let Ok((tag, _)) = fetch_latest(app, nightly(&prefs)).await else { return };
    let games = app.state::<Store>().entries("games").unwrap_or_default();
    for (_, game) in games {
        let (Some(shop), Some(object_id)) = (game["shop"].as_str(), game["objectId"].as_str()) else { continue };
        let Ok((_, dir)) = context(app, shop, object_id) else { continue };
        let Some(marker) = read_marker(&dir) else { continue };
        let Some(proxy) = marker["proxy"].as_str().filter(|_| marker["version"] != tag.as_str()) else { continue };
        // Files stay locked while the game runs; install rolls back and retries next launch.
        match install_for_game(app, shop, object_id, proxy).await {
            Ok(_) => eprintln!("[optiscaler] updated to {tag}: {}", game["title"]),
            Err(err) => eprintln!("[optiscaler] auto-update skipped for {}: {err}", game["title"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-optiscaler-{name}-{}", crate::library::now_ms()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn classifies_gpus_like_electron() {
        let c = |n: &[&str]| classify_gpus(&n.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(c(&["AMD Radeon RX 9070 XT"]), "native");
        assert_eq!(c(&["AMD Radeon(TM) Graphics", "AMD Radeon RX 7800 XT"]), "desktop");
        assert_eq!(c(&["NVIDIA GeForce RTX 4070"]), "desktop");
        assert_eq!(c(&["Intel Arc A770 Graphics"]), "desktop");
        // Same as the JS regex: "(TM)" between Arc and the model breaks the match.
        assert_eq!(c(&["Intel(R) Arc(TM) A770 Graphics"]), "igpu");
        assert_eq!(c(&["Intel(R) Arc(TM) Graphics"]), "igpu");
        assert_eq!(c(&["AMD Radeon 780M"]), "igpu");
        assert_eq!(c(&[]), "desktop");
    }

    #[test]
    fn parses_sums_and_natural_order() {
        let h = "a".repeat(64);
        let sums = parse_sums(&format!("{h}  fsr4.zip\r\n{h} *desktop/exact/x.dll\nbad line\n"));
        assert_eq!(sums.get("fsr4.zip"), Some(&h));
        assert_eq!(sums.get("desktop/exact/x.dll"), Some(&h));
        assert_eq!(sums.len(), 2);
        let mut tags = vec!["dll-10", "dll-9", "dll-2"];
        tags.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(tags, ["dll-2", "dll-9", "dll-10"]);
    }

    #[test]
    fn detects_api_root_and_anticheat() {
        let dir = temp("detect");
        let exe = dir.join("game.exe");
        let wide: Vec<u8> = "D3D11.dll".bytes().flat_map(|b| [b, 0]).collect();
        fs::write(&exe, [b"xx".as_slice(), &wide].concat()).unwrap();
        assert_eq!(detect_api(&exe), "dx11");
        fs::write(&exe, b"vulkan-1.dll d3d12.dll").unwrap();
        assert_eq!(detect_api(&exe), "dx12");
        fs::create_dir_all(dir.join("EasyAntiCheat")).unwrap();
        assert_eq!(detect_anticheat(&dir).as_deref(), Some("EasyAntiCheat"));
        let unreal = dir.join("Game").join("Binaries").join("Win64");
        assert_eq!(game_root_from_exe_dir(&unreal), dir);
        fs::create_dir_all(&unreal).unwrap();
        fs::write(unreal.join("Game-Win64-Shipping.exe"), b"x").unwrap();
        assert_eq!(resolve_injected_exe(exe.clone()), unreal.join("Game-Win64-Shipping.exe"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn installs_backs_up_reinstalls_and_rolls_back() {
        let root = temp("install");
        let (payload, game) = (root.join("payload"), root.join("game"));
        fs::create_dir_all(payload.join("D3D12_Optiscaler")).unwrap();
        fs::create_dir_all(&game).unwrap();
        fs::write(payload.join("OptiScaler.dll"), b"opti").unwrap();
        fs::write(payload.join("OptiScaler.ini"), "[Upscalers]\nDx12Upscaler=auto\nLogToFile=auto\nLogLevel=auto\nFsr4Update=auto\n").unwrap();
        fs::write(payload.join("D3D12_Optiscaler").join("x.dll"), b"x").unwrap();
        fs::write(payload.join("!! READ ME.txt"), b"skip").unwrap();
        fs::write(game.join("dxgi.dll"), b"original").unwrap();
        let fsr4 = root.join("fsr4.dll");
        fs::write(&fsr4, b"fsr4").unwrap();

        let done = install(&payload, &game, "dxgi.dll", "v1", Some(&fsr4), Some("dll"), "dx12").unwrap();
        assert!(done.contains(&("Dx12Upscaler".into(), "fsr31".into())));
        assert_eq!(fs::read(game.join("dxgi.dll")).unwrap(), b"opti");
        assert_eq!(fs::read(game.join(BACKUP).join("dxgi.dll")).unwrap(), b"original");
        assert!(!game.join("!! READ ME.txt").exists());
        assert_eq!(read_marker(&game).unwrap()["version"], "v1");

        // User edits survive a reinstall; the backup keeps the true original.
        write_config_value(&game.join("OptiScaler.ini"), "Dx12Upscaler", "xess").unwrap();
        assert!(write_config_value(&game.join("OptiScaler.ini"), "Dx12Upscaler", "bogus").is_err());
        assert!(write_config_value(&game.join("OptiScaler.ini"), "Unknown", "1").is_err());
        // Booleans can go back to "auto"; numbers stay in range.
        write_config_value(&game.join("OptiScaler.ini"), "Fsr4Update", "auto").unwrap();
        assert!(write_config_value(&game.join("OptiScaler.ini"), "Fsr4Update", "yes").is_err());
        write_config_value(&game.join("OptiScaler.ini"), "Fsr4Update", "true").unwrap();
        install(&payload, &game, "dxgi.dll", "v2", None, None, "dx12").unwrap();
        assert_eq!(ini_get_set(&game.join("OptiScaler.ini"), "Dx12Upscaler", None).as_deref(), Some("xess"));
        assert_eq!(fs::read(game.join(BACKUP).join("dxgi.dll")).unwrap(), b"original");
        assert_eq!(read_config(&game.join("OptiScaler.ini")).as_array().unwrap().len(), 4);

        uninstall(&game, false).unwrap();
        assert_eq!(fs::read(game.join("dxgi.dll")).unwrap(), b"original");
        assert!(!game.join("D3D12_Optiscaler").exists() && !game.join(MARKER).exists() && !game.join(BACKUP).exists());

        // A failed copy rolls back to the original files.
        let missing = root.join("missing.dll");
        assert!(install(&payload, &game, "dxgi.dll", "v3", Some(&missing), Some("dll"), "dx12").is_err());
        assert_eq!(fs::read(game.join("dxgi.dll")).unwrap(), b"original");
        assert!(!game.join(MARKER).exists());

        // Another tool's marker blocks the install.
        fs::write(game.join(MARKER), r#"{"proxy":"bloodborne"}"#).unwrap();
        assert!(install(&payload, &game, "dxgi.dll", "v4", None, None, "dx12").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ini_sections_and_profiles() {
        let dir = temp("ini");
        let ini = dir.join("OptiScaler.ini");
        fs::write(&ini, "[FrameGen]\n; Enabled=comment\nEnabled=auto\nFGInput=auto\n\n[DLSS]\nEnabled=auto\n").unwrap();
        assert_eq!(ini_get_set(&ini, "DLSS.Enabled", Some("false")).as_deref(), Some("auto"));
        assert_eq!(ini_get_set(&ini, "FrameGen.Enabled", None).as_deref(), Some("auto"));
        // Missing sectioned keys land inside their section; a missing section is created.
        ini_get_set(&ini, "FrameGen.MakeDepthCopy", Some("true"));
        ini_get_set(&ini, "Nukems.MakeDepthCopy", Some("true"));
        let text = fs::read_to_string(&ini).unwrap();
        assert!(text.contains("FGInput=auto\nMakeDepthCopy=true\n\n[DLSS]\nEnabled=false\n[Nukems]\nMakeDepthCopy=true\n"), "{text}");

        let amd = builtin_profiles().into_iter().find(|p| p["id"] == "amd-fg").unwrap();
        assert!(clean_values(&amd["values"]).is_ok());
        // FGOutput and OverrideNvapiDll are not in this ini: skipped, not added.
        assert_eq!(apply_values(&ini, &amd["values"]), ["FGOutput", "OverrideNvapiDll"]);
        assert_eq!(ini_get_set(&ini, "FrameGen.Enabled", None).as_deref(), Some("true"));
        assert_eq!(ini_get_set(&ini, "DLSS.Enabled", None).as_deref(), Some("false"));
        assert!(clean_values(&json!({ "FGInput": "bogus" })).is_err());
        assert!(clean_values(&json!({ "Unknown": "1" })).is_err());
        assert!(clean_values(&json!({ "Sharpness": "auto" })).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_and_moves_only_identical_leftovers() {
        let root = temp("leftovers");
        let (payload, game) = (root.join("payload"), root.join("game"));
        fs::create_dir_all(payload.join("D3D12_Optiscaler")).unwrap();
        fs::create_dir_all(game.join("D3D12_Optiscaler")).unwrap();
        fs::write(payload.join("OptiScaler.dll"), b"opti").unwrap();
        fs::write(payload.join("D3D12_Optiscaler").join("D3D12Core.dll"), b"core").unwrap();
        fs::write(payload.join("libxess.dll"), b"xess-ours").unwrap();
        // Left behind by an untracked install: OptiScaler.dll renamed to winmm.dll.
        fs::write(game.join("winmm.dll"), b"opti").unwrap();
        fs::write(game.join("D3D12_Optiscaler").join("D3D12Core.dll"), b"core").unwrap();
        fs::write(game.join("OptiScaler.ini"), b"[x]").unwrap();
        // The game's own files: same name or same size, different bytes.
        fs::write(game.join("libxess.dll"), b"xess-game").unwrap();
        fs::write(game.join("dxgi.dll"), b"game").unwrap();
        assert!(has_leftovers(&game));

        let mut found = find_leftovers(&game, &[payload]);
        found.sort();
        assert_eq!(found, ["D3D12_Optiscaler/D3D12Core.dll", "OptiScaler.ini", "winmm.dll"]);
        move_leftovers(&game, &found).unwrap();
        assert!(!game.join("winmm.dll").exists() && !game.join("D3D12_Optiscaler").exists());
        assert_eq!(fs::read(game.join(CLEANUP).join("winmm.dll")).unwrap(), b"opti");
        assert_eq!(fs::read(game.join("libxess.dll")).unwrap(), b"xess-game");
        assert!(game.join("dxgi.dll").exists() && !has_leftovers(&game));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adds_and_removes_tracked_files() {
        let root = temp("tracked");
        let (payload, game) = (root.join("payload"), root.join("game"));
        fs::create_dir_all(&payload).unwrap();
        fs::create_dir_all(game.join("plugins")).unwrap();
        fs::write(payload.join("OptiScaler.dll"), b"opti").unwrap();
        install(&payload, &game, "dxgi.dll", "v1", None, None, "dx12").unwrap();
        let asi = root.join("op.asi");
        fs::write(&asi, b"patch").unwrap();
        fs::write(game.join(OPTIPATCHER_REL), b"theirs").unwrap();

        add_tracked_file(&game, OPTIPATCHER_REL, &asi).unwrap();
        assert!(has_tracked_file(&game, OPTIPATCHER_REL));
        assert_eq!(fs::read(game.join(OPTIPATCHER_REL)).unwrap(), b"patch");
        remove_tracked_file(&game, OPTIPATCHER_REL).unwrap();
        assert!(!has_tracked_file(&game, OPTIPATCHER_REL));
        assert_eq!(fs::read(game.join(OPTIPATCHER_REL)).unwrap(), b"theirs");

        // Uninstall removes it and restores the foreign file.
        add_tracked_file(&game, OPTIPATCHER_REL, &asi).unwrap();
        uninstall(&game, false).unwrap();
        assert_eq!(fs::read(game.join(OPTIPATCHER_REL)).unwrap(), b"theirs");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "network: downloads OptiPatcher.asi and checks its digest"]
    fn fetches_live_optipatcher() {
        let root = temp("optipatcher-live");
        let file = tauri::async_runtime::block_on(fetch_optipatcher(&root)).unwrap();
        assert!(fs::metadata(&file).unwrap().len() > 10_000);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marker_paths_cannot_leave_the_game_folder() {
        assert!(inside(Path::new("g"), "../evil.dll").is_none());
        assert!(inside(Path::new("g"), "a/b.dll").is_some());
    }
}
