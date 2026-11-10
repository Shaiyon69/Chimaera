//! The data folder picked in Chimaera Setup. Setup writes its path to a `data-location`
//! file next to the exe; without that file Chimaera uses Tauri's app_data_dir, as before.
//! Moving the data later is not supported: the file is read once per run.

use std::path::PathBuf;
use std::sync::OnceLock;

pub const FILE: &str = "data-location";

/// The chosen folder, or None for the default.
pub fn override_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        parse(&std::fs::read_to_string(exe.parent()?.join(FILE)).ok()?)
    })
    .as_ref()
}

/// Only an absolute path counts; anything else falls back to the default.
pub fn parse(text: &str) -> Option<PathBuf> {
    let path = PathBuf::from(text.trim());
    path.is_absolute().then_some(path)
}

/// `app.path().chimaera_data_dir()` in place of `app_data_dir()` everywhere Chimaera keeps data.
pub trait DataDir {
    fn chimaera_data_dir(&self) -> tauri::Result<PathBuf>;
}

impl<R: tauri::Runtime> DataDir for tauri::path::PathResolver<R> {
    fn chimaera_data_dir(&self) -> tauri::Result<PathBuf> {
        match override_dir() {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                Ok(dir.clone())
            }
            None => self.app_data_dir(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absolute_paths_override() {
        let abs = if cfg!(windows) { r"D:\Games\Chimaera\data" } else { "/games/chimaera/data" };
        assert_eq!(parse(&format!("{abs}\r\n")), Some(PathBuf::from(abs)));
        assert_eq!(parse("data"), None);
        assert_eq!(parse("   "), None);
    }
}
