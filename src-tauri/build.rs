fn main() {
    // Same build-time API URL the Electron main process gets from .env via electron-vite.
    println!("cargo:rerun-if-changed=../.env");
    if let Ok(env) = std::fs::read_to_string("../.env") {
        for line in env.lines() {
            if let Some(url) = line.strip_prefix("MAIN_VITE_API_URL=") {
                println!("cargo:rustc-env=CHIMAERA_API_URL={}", url.trim());
            }
            // Desktop OAuth client for Drive save backups; Google treats its secret as public.
            if let Some(id) = line.strip_prefix("MAIN_VITE_GOOGLE_CLIENT_ID=") {
                println!("cargo:rustc-env=CHIMAERA_GOOGLE_CLIENT_ID={}", id.trim());
            }
            if let Some(secret) = line.strip_prefix("MAIN_VITE_GOOGLE_CLIENT_SECRET=") {
                println!("cargo:rustc-env=CHIMAERA_GOOGLE_CLIENT_SECRET={}", secret.trim());
            }
        }
    }
    link_torrent_bridge();
    tauri_build::build()
}
/// Links the prebuilt libtorrent bridge (native/torrent-bridge) and puts the shared
/// library next to the dev and test binaries. HYDRA_TORRENT_LIB_DIR overrides the folder.
fn link_torrent_bridge() {
    use std::path::PathBuf;
    println!("cargo:rerun-if-env-changed=HYDRA_TORRENT_LIB_DIR");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let prebuilt = PathBuf::from(if windows { "../native/torrent-bridge/prebuilt/x64-windows-static" } else { "../native/torrent-bridge/prebuilt/x64-linux" });
    let lib_dir = std::env::var("HYDRA_TORRENT_LIB_DIR").map(PathBuf::from).unwrap_or_else(|_| prebuilt.join("lib"));
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    let shared = if windows { prebuilt.join("bin/hydra_torrent_bridge.dll") } else { lib_dir.join("libhydra_torrent_bridge.so") };
    if !windows {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
    println!("cargo:rerun-if-changed={}", shared.display());
    // OUT_DIR is target/<profile>/build/<pkg>/out.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile = out.ancestors().nth(3).unwrap();
    for dir in [profile.to_path_buf(), profile.join("deps")] {
        let _ = std::fs::create_dir_all(&dir);
        if let Err(e) = std::fs::copy(&shared, dir.join(shared.file_name().unwrap())) {
            println!("cargo:warning=Could not copy {}: {e}", shared.display());
        }
    }
}
