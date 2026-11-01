//! WebView2 memory target level. Low tells the renderer to trim caches and page out memory;
//! it is set while a window is out of sight or a game is running, so the launcher gives RAM back
//! when it is not being used. The page keeps working either way.

use tauri::{AppHandle, Manager, WebviewWindow};

/// Low while a game runs, or for any window that is minimized or hidden.
pub fn refresh(app: &AppHandle) {
    let gaming = !app.state::<crate::watcher::Sessions>().is_empty();
    for window in app.webview_windows().into_values() {
        let out_of_sight = window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true);
        set_low(&window, gaming || out_of_sight);
    }
}

#[cfg(windows)]
fn set_low(window: &WebviewWindow, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    use windows::core::Interface;
    let level = if low { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL };
    let _ = window.with_webview(move |webview| unsafe {
        // ICoreWebView2_19 needs WebView2 runtime 114+; older runtimes keep the default level.
        if let Ok(core) = webview.controller().CoreWebView2().and_then(|c| c.cast::<ICoreWebView2_19>()) {
            let _ = core.SetMemoryUsageTargetLevel(level);
        }
    });
}

#[cfg(not(windows))]
fn set_low(_window: &WebviewWindow, _low: bool) {}
