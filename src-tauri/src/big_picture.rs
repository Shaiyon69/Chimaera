//! Big Picture window (WindowManager.openBigPictureWindow) and the system power
//! actions only that window may run (events/big-picture/system-power.ts).

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::store::Store;

pub const LABEL: &str = "big-picture";
pub const ARG: &str = "--big-picture";

static POWER_ACTION_PENDING: AtomicBool = AtomicBool::new(false);

/// launchInBigPicture, or `--big-picture` on the command line.
pub fn should_launch_in(store: &Store, args: &[String]) -> bool {
    args.iter().any(|a| a == ARG) || store.get("", "userPreferences").unwrap_or(Value::Null)["launchInBigPicture"] == Value::Bool(true)
}

fn initial_route(preferences: &Value) -> &'static str {
    let to_library = match &preferences["bigPictureLaunchToLibraryPage"] {
        Value::Bool(b) => *b,
        _ => preferences["launchToLibraryPage"] == Value::Bool(true),
    };
    if to_library {
        "big-picture/library"
    } else {
        "big-picture"
    }
}

/// Focuses the Big Picture window if it is open; openMainWindow and second launches go there first.
pub fn focus_if_open(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window(LABEL) else { return false };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    true
}

pub fn open(app: &AppHandle) -> Result<(), String> {
    if focus_if_open(app) {
        return Ok(());
    }
    let preferences = app.state::<Store>().get("", "userPreferences").unwrap_or(Value::Null);
    let main = app.get_webview_window("main");
    let monitor = main.as_ref().and_then(|m| m.current_monitor().ok().flatten()).or_else(|| app.primary_monitor().ok().flatten());
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(format!("tauri.html#/{}", initial_route(&preferences)).into()))
        .title("Chimaera")
        .decorations(false)
        .background_color(tauri::webview::Color(0, 0, 0, 0xff))
        .initialization_script(crate::process_script());
    if let Some(monitor) = monitor {
        let scale = monitor.scale_factor();
        let (pos, size) = (monitor.position().to_logical::<f64>(scale), monitor.size().to_logical::<f64>(scale));
        builder = builder.position(pos.x, pos.y).inner_size(size.width, size.height);
    }
    let window = builder.fullscreen(true).build().map_err(|e| e.to_string())?;
    if let Some(main) = &main {
        let _ = main.hide();
    }
    let app = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.show();
                let _ = main.set_focus();
            }
            crate::webview_memory::refresh(&app);
        }
    });
    let _ = window.set_focus();
    crate::webview_memory::refresh(window.app_handle());
    Ok(())
}

/// getSystemPowerCommand: program and arguments for the action on this OS.
fn power_command(action: &str) -> Result<(String, Vec<String>), String> {
    if !matches!(action, "power-off" | "restart" | "suspend") {
        return Err("Invalid system power action".into());
    }
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        if action == "suspend" {
            // Windows Forms enables the shutdown privilege before SetSuspendState; wake events stay on.
            let script = "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.Windows.Forms; if (-not [System.Windows.Forms.Application]::SetSuspendState([System.Windows.Forms.PowerState]::Suspend, $false, $false)) { throw 'Windows refused to suspend' }";
            let file = format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe");
            return Ok((file, ["-NoProfile", "-NonInteractive", "-Command", script].map(String::from).to_vec()));
        }
        // A positive timeout implies /f; keep applications free to block shutdown.
        let flag = if action == "power-off" { "/s" } else { "/r" };
        return Ok((format!(r"{root}\System32\shutdown.exe"), [flag, "/t", "0"].map(String::from).to_vec()));
    }
    if cfg!(target_os = "linux") {
        let verb = match action {
            "power-off" => "poweroff",
            "restart" => "reboot",
            _ => "suspend",
        };
        return Ok(("systemctl".into(), vec![verb.into()]));
    }
    if cfg!(target_os = "macos") {
        let verb = match action {
            "power-off" => "shut down",
            "restart" => "restart",
            _ => "sleep",
        };
        return Ok(("/usr/bin/osascript".into(), vec!["-e".into(), format!("tell application \"System Events\" to {verb}")]));
    }
    Err("System power actions are not supported on this platform".into())
}

pub async fn execute_power_action(window_label: &str, action: &Value) -> Result<(), String> {
    if window_label != LABEL {
        return Err("System power actions require the Big Picture window".into());
    }
    let (file, args) = power_command(action.as_str().unwrap_or_default())?;
    if POWER_ACTION_PENDING.swap(true, Ordering::SeqCst) {
        return Err("A system power action is already pending".into());
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut command = std::process::Command::new(&file);
        command.args(&args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command.output().map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!("{file} failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
        }
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    POWER_ACTION_PENDING.store(false, Ordering::SeqCst);
    if let Err(error) = &result {
        eprintln!("[power] {action}: {error}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_unknown_actions() {
        assert!(power_command("hibernate").is_err());
        assert!(power_command("power-off").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn windows_shutdown_never_forces() {
        let (file, args) = power_command("restart").unwrap();
        assert!(file.ends_with(r"System32\shutdown.exe"));
        assert_eq!(args, ["/r", "/t", "0"]);
    }

    #[test]
    fn route_prefers_the_big_picture_setting() {
        assert_eq!(initial_route(&json!({})), "big-picture");
        assert_eq!(initial_route(&json!({ "launchToLibraryPage": true })), "big-picture/library");
        assert_eq!(initial_route(&json!({ "launchToLibraryPage": true, "bigPictureLaunchToLibraryPage": false })), "big-picture");
    }

    #[test]
    fn only_the_big_picture_window_may_power_off() {
        assert!(tauri::async_runtime::block_on(execute_power_action("main", &json!("power-off"))).is_err());
    }
}
