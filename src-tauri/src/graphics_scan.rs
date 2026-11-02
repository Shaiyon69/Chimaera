//! Which upscaler and frame-gen DLLs a game ships (DLSS, FSR, XeSS), with their file
//! versions, so the Graphics tab can show what OptiScaler has to work with.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use tauri::AppHandle;

use crate::optiscaler::{context, game_root_from_exe_dir, walk};

/// Kind for a DLL name, or None when it is not an upscaler/frame-gen library.
fn kind(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "nvngx_dlss.dll" => "dlss",
        "nvngx_dlssg.dll" => "dlssg",
        "nvngx_dlssd.dll" => "dlssd",
        "libxess.dll" | "libxess_dx11.dll" => "xess",
        "libxess_fg.dll" => "xefg",
        n if n.ends_with(".dll") && (n.starts_with("amd_fidelityfx") || n.starts_with("ffx_fsr")) => {
            if n.contains("framegeneration") { "fsrfg" } else { "fsr" }
        }
        _ => return None,
    })
}

/// File version from the VS_FIXEDFILEINFO block (signature 0xFEEF04BD), e.g. "310.2.1.0".
fn file_version(bytes: &[u8]) -> Option<String> {
    const SIG: [u8; 4] = [0xBD, 0x04, 0xEF, 0xFE];
    let at = bytes.windows(4).rposition(|w| w == SIG)?;
    let word = |o: usize| bytes.get(at + o..at + o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let (ms, ls) = (word(8)?, word(12)?);
    Some(format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff))
}

/// Upscaler DLLs under `root`; `added` marks files an OptiScaler install of ours put there.
fn scan(root: &Path, exe_dir: &Path) -> Vec<Value> {
    let ours: Vec<std::path::PathBuf> = crate::optiscaler::read_marker(exe_dir)
        .map(|m| m["files"].as_array().into_iter().flatten().filter_map(Value::as_str).filter_map(|r| crate::optiscaler::inside(exe_dir, r)).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    walk(root, 6, &mut |_, e| {
        let lower = e.file_name().to_string_lossy().to_lowercase();
        if let (Some(kind), true) = (kind(&lower), e.file_type().is_ok_and(|t| t.is_file())) {
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push(json!({
                "kind": kind,
                "file": rel,
                "version": fs::read(&path).ok().as_deref().and_then(file_version),
                "added": ours.contains(&path),
            }));
        }
        true
    });
    out
}

/// `getGameUpscalers`.
pub async fn upscalers(app: &AppHandle, shop: &str, object_id: &str) -> Result<Value, String> {
    let dir = context(app, shop, object_id)?.1;
    let root = game_root_from_exe_dir(&dir);
    tauri::async_runtime::spawn_blocking(move || Value::Array(scan(&root, &dir))).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_dll_names() {
        assert_eq!(kind("nvngx_dlss.dll"), Some("dlss"));
        assert_eq!(kind("nvngx_dlssg.dll"), Some("dlssg"));
        assert_eq!(kind("amd_fidelityfx_dx12.dll"), Some("fsr"));
        assert_eq!(kind("amd_fidelityfx_framegeneration_dx12.dll"), Some("fsrfg"));
        assert_eq!(kind("ffx_fsr2_api_x64.dll"), Some("fsr"));
        assert_eq!(kind("libxess.dll"), Some("xess"));
        assert_eq!(kind("d3d11.dll"), None);
        assert_eq!(kind("amd_fidelityfx.txt"), None);
    }

    #[test]
    fn reads_fixed_file_version() {
        let mut bytes = vec![0u8; 64];
        bytes.extend([0xBD, 0x04, 0xEF, 0xFE, 0, 0, 1, 0]);
        bytes.extend(((310u32 << 16) | 2).to_le_bytes());
        bytes.extend(((1u32 << 16) | 0).to_le_bytes());
        assert_eq!(file_version(&bytes).as_deref(), Some("310.2.1.0"));
        assert_eq!(file_version(b"no version here"), None);
    }

    #[test]
    fn scans_and_marks_our_files() {
        let root = std::env::temp_dir().join(format!("chimaera-upscalers-{}", crate::library::now_ms()));
        let bin = root.join("Game").join("Binaries").join("Win64");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("nvngx_dlss.dll"), b"x").unwrap();
        fs::write(bin.join("amd_fidelityfx_dx12.dll"), b"x").unwrap();
        fs::write(bin.join("optiscaler_manager.json"), r#"{"proxy":"dxgi.dll","files":["amd_fidelityfx_dx12.dll"]}"#).unwrap();
        let mut found = scan(&root, &bin);
        found.sort_by_key(|v| v["kind"].as_str().unwrap_or_default().to_string());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["kind"], "dlss");
        assert_eq!(found[0]["added"], false);
        assert_eq!(found[1]["file"], "Game/Binaries/Win64/amd_fidelityfx_dx12.dll");
        assert_eq!(found[1]["added"], true);
        fs::remove_dir_all(root).unwrap();
    }
}
