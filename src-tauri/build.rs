fn main() {
    // Same build-time API URL the Electron main process gets from .env via electron-vite.
    println!("cargo:rerun-if-changed=../.env");
    if let Ok(env) = std::fs::read_to_string("../.env") {
        for line in env.lines() {
            if let Some(url) = line.strip_prefix("MAIN_VITE_API_URL=") {
                println!("cargo:rustc-env=CHIMAERA_API_URL={}", url.trim());
            }
        }
    }
    tauri_build::build()
}
