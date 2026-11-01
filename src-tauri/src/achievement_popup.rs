//! The corner pop-up shown when achievements unlock: a transparent, click-through,
//! always-on-top window that plays one notification at a time. Ported from
//! src/main/services/achievement-notification-presenter.ts (+ -electron.ts).
//!
//! Handshake with the page (src/renderer/src/pages/achievements/notification):
//! window loads -> page calls achievementNotificationHostReady -> we send
//! prepare-achievement-notification -> page calls ...ContentReady -> we show the
//! window and send start-achievement-notification -> page calls ...Finished.
//! Each step has a watchdog; a stuck step drops the window and its queue.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "achievement-notification";
const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 180.0;
const HOST_READY_TIMEOUT: Duration = Duration::from_millis(5_000);
const CONTENT_READY_TIMEOUT: Duration = Duration::from_millis(5_000);
const COMPLETION_TIMEOUT: Duration = Duration::from_millis(6_450);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    Idle,
    Loading,
    Preparing,
    Visible,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Value>,
    active: Option<Value>,
    stage: Option<Stage>,
    sequence: u64,
    /// Bumped on every stage change, so a watchdog only fires for the step it guards.
    step: u64,
}

#[derive(Default)]
pub struct Popup(Mutex<State>);

impl Popup {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn stage(state: &State) -> Stage {
    state.stage.unwrap_or(Stage::Idle)
}

/// Window position for "top-left" ... "bottom-right" on the primary display, in physical pixels.
pub fn corner(position: &str, origin: (i32, i32), size: (u32, u32), scale: f64) -> (i32, i32) {
    let (w, h) = ((WIDTH * scale).round() as i32, (HEIGHT * scale).round() as i32);
    let (sw, sh) = (size.0 as i32, size.1 as i32);
    let x = if position.ends_with("center") {
        origin.0 + (sw - w) / 2
    } else if position.ends_with("right") {
        origin.0 + sw - w
    } else {
        origin.0
    };
    let y = if position.starts_with("bottom") { origin.1 + sh - h } else { origin.1 };
    (x, y)
}

fn move_to(app: &AppHandle, position: &str) {
    let (Some(window), Ok(Some(monitor))) = (app.get_webview_window(LABEL), app.primary_monitor()) else { return };
    let (origin, size) = (monitor.position(), monitor.size());
    let (x, y) = corner(position, (origin.x, origin.y), (size.width, size.height), monitor.scale_factor());
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn create_window(app: &AppHandle) -> Result<(), String> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("tauri.html#/achievement-notification".into()))
        .title("Chimaera")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .initialization_script(crate::process_script())
        .build()
        .map_err(|e| e.to_string())?;
    window.set_ignore_cursor_events(true).map_err(|e| e.to_string())
}

fn destroy_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.destroy();
    }
}

fn arm_watchdog(app: &AppHandle, state: &mut State, delay: Duration, reason: &'static str) {
    state.step += 1;
    let (app, step) = (app.clone(), state.step);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        let popup = app.state::<Popup>();
        let mut state = popup.lock();
        if state.step == step && stage(&state) != Stage::Idle {
            fail(&app, &mut state, reason);
        }
    });
}

fn fail(app: &AppHandle, state: &mut State, reason: &str) {
    eprintln!("[achievement-popup] failed: {reason}");
    state.queue.clear();
    reset(app, state);
}

fn reset(app: &AppHandle, state: &mut State) {
    state.active = None;
    state.stage = Some(Stage::Idle);
    state.step += 1;
    destroy_window(app);
}

fn prepare_next(app: &AppHandle, state: &mut State) {
    let Some(request) = state.queue.pop_front() else {
        reset(app, state);
        return;
    };
    move_to(app, request["position"].as_str().unwrap_or("top-left"));
    state.stage = Some(Stage::Preparing);
    if let Err(error) = app.emit_to(LABEL, "prepare-achievement-notification", [&request]) {
        state.active = Some(request);
        return fail(app, state, &format!("could not send the notification: {error}"));
    }
    state.active = Some(request);
    arm_watchdog(app, state, CONTENT_READY_TIMEOUT, "notification content did not become ready");
}

fn enqueue(app: &AppHandle, requests: Vec<Value>) {
    if requests.is_empty() {
        return;
    }
    let popup = app.state::<Popup>();
    let mut state = popup.lock();
    state.queue.extend(requests);
    if stage(&state) != Stage::Idle {
        return;
    }
    if let Err(error) = create_window(app) {
        return fail(app, &mut state, &format!("could not create the window: {error}"));
    }
    state.stage = Some(Stage::Loading);
    arm_watchdog(app, &mut state, HOST_READY_TIMEOUT, "notification page did not become ready");
}

fn next_id(app: &AppHandle) -> String {
    let popup = app.state::<Popup>();
    let mut state = popup.lock();
    state.sequence += 1;
    format!("achievement-notification-{}", state.sequence)
}

/// One pop-up per achievement (enqueueAchievements).
pub fn show_achievements(app: &AppHandle, position: &str, achievements: Vec<Value>) {
    let requests = achievements.into_iter().map(|a| json!({ "id": next_id(app), "type": "achievement", "position": position, "achievement": a })).collect();
    enqueue(app, requests);
}

/// "N achievements in M games", after the startup scan (enqueueCombined).
pub fn show_combined(app: &AppHandle, position: &str, game_count: usize, achievement_count: usize) {
    if game_count == 0 || achievement_count == 0 {
        return;
    }
    let request = json!({ "id": next_id(app), "type": "combined", "position": position, "gameCount": game_count, "achievementCount": achievement_count });
    enqueue(app, vec![request]);
}

/// The page's handshake calls, by channel name.
pub fn handle(app: &AppHandle, channel: &str, request_id: Option<&str>, reason: Option<&str>) {
    let popup = app.state::<Popup>();
    let mut state = popup.lock();
    let is_active = state.active.as_ref().and_then(|a| a["id"].as_str()) == request_id && request_id.is_some();
    match (channel, stage(&state)) {
        ("achievementNotificationHostReady", Stage::Loading) => prepare_next(app, &mut state),
        ("achievementNotificationContentReady", Stage::Preparing) if is_active => {
            if let Some(window) = app.get_webview_window(LABEL) {
                let _ = window.show();
            }
            state.stage = Some(Stage::Visible);
            let _ = app.emit_to(LABEL, "start-achievement-notification", [request_id]);
            arm_watchdog(app, &mut state, COMPLETION_TIMEOUT, "notification animation timed out");
        }
        ("achievementNotificationFinished", Stage::Visible) if is_active => {
            if let Some(window) = app.get_webview_window(LABEL) {
                let _ = window.hide();
            }
            state.active = None;
            prepare_next(app, &mut state);
        }
        ("achievementNotificationFailed", s) if s != Stage::Idle && (request_id.is_none() || is_active) => {
            fail(app, &mut state, reason.unwrap_or("notification page failed"));
        }
        _ => {}
    }
}

/// `updateAchievementCustomNotificationWindow`: settings changed, so drop the window (dispose).
pub fn dispose(app: &AppHandle) {
    let popup = app.state::<Popup>();
    let mut state = popup.lock();
    state.queue.clear();
    if stage(&state) != Stage::Idle {
        reset(app, &mut state);
    }
}

/// isRareAchievement: Hydra's points encode rarity; under 10 % of players is rare.
fn is_rare(points: f64) -> bool {
    (50.0 - points.sqrt()) * 2.0 < 10.0
}

/// AchievementNotificationInfo for each new unlock, oldest first (publishAchievementUnlockNotifications).
/// The last one is platinum when it completes the list.
pub fn notification_infos(new: &[Value], previously_unlocked: usize, catalogue: &[Value]) -> Vec<Value> {
    let mut sorted: Vec<&Value> = new.iter().collect();
    sorted.sort_by_key(|u| u["unlockTime"].as_i64().unwrap_or(0));
    let matched: Vec<&Value> = sorted
        .iter()
        .filter_map(|u| {
            let name = u["name"].as_str().unwrap_or_default().to_uppercase();
            catalogue.iter().find(|a| a["name"].as_str().unwrap_or_default().to_uppercase() == name)
        })
        .collect();
    let completes = new.len() + previously_unlocked == catalogue.len();
    matched
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let points = a["points"].as_f64();
            json!({
                "title": a["displayName"],
                "description": a["description"],
                "points": a["points"],
                "isHidden": a["hidden"].as_bool().unwrap_or(false),
                "isRare": points.is_some_and(|p| p != 0.0 && is_rare(p)),
                "isPlatinum": i == matched.len() - 1 && completes,
                "iconUrl": a["icon"],
            })
        })
        .collect()
}

/// Whether unlocks are announced at all.
pub fn enabled(preferences: &Value) -> bool {
    preferences["achievementNotificationsEnabled"].as_bool() != Some(false)
}

/// Whether the custom pop-up shows them; otherwise an OS toast does (publishNewAchievementNotification).
pub fn custom_enabled(preferences: &Value) -> bool {
    preferences["achievementCustomNotificationsEnabled"].as_bool() != Some(false)
}

/// publishCombinedNewAchievementNotification: the startup summary as an OS toast.
pub fn show_combined_toast(app: &AppHandle, game_count: usize, achievement_count: usize) {
    let store = app.state::<crate::store::Store>();
    let (games, achievements) = (game_count.to_string(), achievement_count.to_string());
    let title = crate::i18n::t(&store, "achievement", "achievement_unlocked", &[]);
    let body = crate::i18n::t(&store, "achievement", "new_achievements_unlocked", &[("gameCount", &games), ("achievementCount", &achievements)]);
    crate::notifications::toast(app, &title, &body);
}

/// publishNewAchievementNotification: one OS toast for this batch of unlocks.
// shortcut: no icon or progress bar on the toast (the plugin builder takes neither from a URL).
pub fn show_toast(app: &AppHandle, game_title: &str, infos: &[Value]) {
    let store = app.state::<crate::store::Store>();
    let names: Vec<&str> = infos.iter().filter_map(|i| i["title"].as_str()).collect();
    let (title, body) = match names.as_slice() {
        [] => return,
        [one] => (crate::i18n::t(&store, "achievement", "achievement_unlocked", &[]), one.to_string()),
        many => {
            let count = many.len().to_string();
            let title = crate::i18n::t(&store, "achievement", "achievements_unlocked_for_game", &[("gameTitle", game_title), ("achievementCount", &count)]);
            (title, many.join(", "))
        }
    };
    crate::notifications::toast(app, &title, &body);
}

pub fn position(preferences: &Value) -> String {
    preferences["achievementCustomNotificationPosition"].as_str().unwrap_or("top-left").to_string()
}

/// `showAchievementTestNotification`: three samples (plain, rare + hidden, platinum).
pub fn show_test(app: &AppHandle, preferences: &Value) {
    let icon = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(include_bytes!("../icons/128x128.png")));
    let store = app.state::<crate::store::Store>();
    let title = crate::i18n::t(&store, "notifications", "test_achievement_notification_title", &[]);
    let description = crate::i18n::t(&store, "notifications", "test_achievement_notification_description", &[]);
    let sample = |rare: bool, hidden: bool, platinum: bool| {
        json!({
            "title": title, "description": description, "iconUrl": icon,
            "points": 2440, "isHidden": hidden, "isRare": rare, "isPlatinum": platinum,
        })
    };
    show_achievements(app, &position(preferences), vec![sample(false, false, false), sample(true, true, false), sample(false, false, true)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_the_window_in_each_corner() {
        let screen = ((0, 0), (1920, 1080));
        assert_eq!(corner("top-left", screen.0, screen.1, 1.0), (0, 0));
        assert_eq!(corner("top-center", screen.0, screen.1, 1.0), (780, 0));
        assert_eq!(corner("bottom-right", screen.0, screen.1, 1.0), (1560, 900));
        // 150 % scaling: the window is 540 x 270 physical pixels.
        assert_eq!(corner("bottom-right", screen.0, screen.1, 1.5), (1380, 810));
    }

    #[test]
    fn builds_infos_oldest_first_with_platinum_on_completion() {
        let catalogue = vec![
            json!({ "name": "A", "displayName": "Alpha", "icon": "a.png", "points": 10, "hidden": false }),
            json!({ "name": "B", "displayName": "Beta", "icon": "b.png", "points": 2400, "hidden": true }),
        ];
        let new = vec![json!({ "name": "B", "unlockTime": 9 }), json!({ "name": "A", "unlockTime": 1 })];
        let infos = notification_infos(&new, 0, &catalogue);
        assert_eq!(infos[0]["title"], "Alpha");
        assert_eq!(infos[0]["isRare"], false);
        assert_eq!(infos[1]["isRare"], true);
        assert_eq!(infos[1]["isHidden"], true);
        assert_eq!((infos[0]["isPlatinum"].as_bool(), infos[1]["isPlatinum"].as_bool()), (Some(false), Some(true)));
        assert_eq!(notification_infos(&new[..1], 0, &catalogue)[0]["isPlatinum"], false);
    }
}
