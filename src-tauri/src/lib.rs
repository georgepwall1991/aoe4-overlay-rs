#![recursion_limit = "256"]

mod api;
mod settings;
mod ws;

use serde_json::Value;
use settings::Settings;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HotkeyAction {
    ToggleOverlay,
    BoToggle,
    BoCycle,
    BoPrevStep,
    BoNextStep,
}

pub struct AppState {
    settings: Mutex<Settings>,
    last_game: Mutex<Option<Value>>,
    last_started: Mutex<Option<String>>,
    /// When true (caster override), the poller won't push live data
    prevent_update: Mutex<bool>,
    bo_index: Mutex<usize>,
    bo_step: Mutex<usize>,
    force_check: Notify,
    http: reqwest::Client,
    ws: ws::WsServer,
}

/// %APPDATA%\{identifier} — resolved without an AppHandle so state can be
/// managed before any window (and its JS) starts invoking commands.
fn config_dir_static() -> std::path::PathBuf {
    let base = std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".config"))
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    base.join("com.georgewall.aoe4overlay")
}

fn config_dir(_app: &AppHandle) -> std::path::PathBuf {
    config_dir_static()
}

// ---------------- player / settings commands ----------------

#[tauri::command]
async fn search_player(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<api::PlayerSearchResult>, String> {
    api::search_players(&state.http, &query).await
}

#[tauri::command]
async fn get_match_history(state: State<'_, AppState>) -> Result<Value, String> {
    let (pid, limit) = {
        let s = state.settings.lock().unwrap();
        (s.profile_id, s.max_games_history)
    };
    let pid = pid.ok_or("no profile selected")?;
    api::get_match_history(&state.http, pid, limit).await
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<'_, AppState>, new_settings: Settings) {
    let scale_changed;
    {
        let mut s = state.settings.lock().unwrap();
        let profile_changed = s.profile_id != new_settings.profile_id;
        scale_changed = s.overlay_scale != new_settings.overlay_scale;
        *s = new_settings.clone();
        s.save(&config_dir(&app));
        if profile_changed {
            *state.last_started.lock().unwrap() = None;
            *state.last_game.lock().unwrap() = None;
        }
    }
    if scale_changed {
        apply_overlay_scale(&app, &state, new_settings.overlay_scale);
    }
    register_hotkeys(&app, &new_settings);
    state.ws.send_colors(&new_settings.team_colors);
    let _ = app.emit("settings_changed", &new_settings);
    state.force_check.notify_one();
    emit_bo(&app);
}

#[tauri::command]
fn get_current_data(state: State<'_, AppState>) -> Option<Value> {
    state.last_game.lock().unwrap().clone()
}

#[tauri::command]
fn force_refresh(state: State<'_, AppState>) {
    *state.last_started.lock().unwrap() = None;
    state.force_check.notify_one();
}

/// Returns Some({version, url}) when a newer GitHub release exists.
#[tauri::command]
async fn check_update(state: State<'_, AppState>) -> Result<Option<Value>, String> {
    let resp = state
        .http
        .get("https://api.github.com/repos/georgepwall1991/aoe4-overlay-rs/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(None); // no releases yet / offline — not an error
    }
    let rel: Value = resp.json().await.map_err(|e| e.to_string())?;
    let tag = rel["tag_name"].as_str().unwrap_or("").trim_start_matches('v');
    let newer = {
        let cur: Vec<u32> = env!("CARGO_PKG_VERSION").split('.').filter_map(|p| p.parse().ok()).collect();
        let new: Vec<u32> = tag.split('.').filter_map(|p| p.parse().ok()).collect();
        !new.is_empty() && new > cur
    };
    Ok(newer.then(|| {
        serde_json::json!({
            "version": tag,
            "url": rel["html_url"].as_str().unwrap_or("https://github.com/georgepwall1991/aoe4-overlay-rs/releases"),
        })
    }))
}

// ---------------- overlay commands ----------------

#[tauri::command]
fn toggle_overlay(app: AppHandle) {
    toggle_window(&app, "overlay");
}

/// Base logical overlay size (matches tauri.conf.json); the size slider scales this.
const OVERLAY_BASE: (f64, f64) = (1040.0, 150.0);

/// Resize the overlay window to `scale`% of its base size. The overlay's CSS
/// uses vw units, so the content scales with the window. The new size is
/// persisted into the saved geometry so it survives a restart.
fn apply_overlay_scale(app: &AppHandle, state: &AppState, scale: u32) {
    let Some(w) = app.get_webview_window("overlay") else { return };
    let f = scale.clamp(50, 200) as f64 / 100.0;
    let sf = w.scale_factor().unwrap_or(1.0);
    let (pw, ph) = (
        (OVERLAY_BASE.0 * f * sf).round() as i32,
        (OVERLAY_BASE.1 * f * sf).round() as i32,
    );
    let _ = w.set_size(tauri::PhysicalSize::new(pw as u32, ph as u32));
    let mut s = state.settings.lock().unwrap();
    if let Some(geo) = s.overlay_geometry.as_mut() {
        geo[2] = pw;
        geo[3] = ph;
        s.save(&config_dir(app));
    }
}

/// Edit mode: a window becomes draggable/clickable with a visible frame.
#[tauri::command]
fn set_edit_mode(app: AppHandle, state: State<'_, AppState>, window: String, edit: bool) {
    if let Some(w) = app.get_webview_window(&window) {
        let _ = w.set_ignore_cursor_events(!edit);
        // Let the user drag-resize the overlay while repositioning
        if window == "overlay" {
            let _ = w.set_resizable(edit);
        }
        let _ = w.show();
        let _ = app.emit("edit_mode", serde_json::json!({"window": window, "edit": edit}));
        if !edit {
            if let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) {
                let geo = Some([pos.x, pos.y, size.width as i32, size.height as i32]);
                let mut s = state.settings.lock().unwrap();
                if window == "overlay" {
                    s.overlay_geometry = geo;
                } else {
                    s.bo_geometry = geo;
                }
                s.save(&config_dir(&app));
            }
        }
    }
}

// ---------------- caster override ----------------

#[tauri::command]
fn override_data(app: AppHandle, state: State<'_, AppState>, data: Value, prevent: bool) {
    *state.prevent_update.lock().unwrap() = prevent;
    *state.last_game.lock().unwrap() = Some(data.clone());
    state.ws.send_player_data(&data);
    let _ = app.emit("game_data", &data);
}

#[tauri::command]
fn reset_override(state: State<'_, AppState>) {
    *state.prevent_update.lock().unwrap() = false;
    *state.last_started.lock().unwrap() = None;
    state.force_check.notify_one();
}

// ---------------- build order ----------------

#[tauri::command]
fn bo_action(app: AppHandle, action: String) {
    let act = match action.as_str() {
        "toggle" => HotkeyAction::BoToggle,
        "cycle" => HotkeyAction::BoCycle,
        "prev" => HotkeyAction::BoPrevStep,
        "next" => HotkeyAction::BoNextStep,
        _ => return,
    };
    run_action(&app, act);
}

#[tauri::command]
fn bo_select(app: AppHandle, state: State<'_, AppState>, name: String) {
    {
        let s = state.settings.lock().unwrap();
        if let Some(i) = s.buildorders.keys().position(|k| *k == name) {
            *state.bo_index.lock().unwrap() = i;
            *state.bo_step.lock().unwrap() = 0;
        }
    }
    emit_bo(&app);
    if let Some(w) = app.get_webview_window("buildorder") {
        let _ = w.show();
    }
}

#[tauri::command]
fn get_bo(app: AppHandle) -> Option<Value> {
    bo_payload(&app)
}

fn bo_step_count(content: &str) -> usize {
    serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|v| v["build_order"].as_array().map(|a| a.len()))
        .unwrap_or(1)
}

/// Current BO payload sent to the buildorder window + control panel.
fn bo_payload(app: &AppHandle) -> Option<Value> {
    let state = app.state::<AppState>();
    let s = state.settings.lock().unwrap();
    let idx = (*state.bo_index.lock().unwrap()).min(s.buildorders.len().saturating_sub(1));
    let step = *state.bo_step.lock().unwrap();
    s.buildorders.iter().nth(idx).map(|(name, content)| {
        serde_json::json!({
            "name": name,
            "content": content,
            "step": step.min(bo_step_count(content).saturating_sub(1)),
        })
    })
}

fn emit_bo(app: &AppHandle) {
    let _ = app.emit("bo_data", &bo_payload(app));
}

fn run_action(app: &AppHandle, action: HotkeyAction) {
    let state = app.state::<AppState>();
    match action {
        HotkeyAction::ToggleOverlay => toggle_window(app, "overlay"),
        HotkeyAction::BoToggle => toggle_window(app, "buildorder"),
        HotkeyAction::BoCycle => {
            {
                let s = state.settings.lock().unwrap();
                let names: Vec<&String> = s.buildorders.keys().collect();
                if names.is_empty() {
                    return;
                }
                let checked: Vec<usize> = names
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| !s.unchecked_buildorders.contains(**n))
                    .map(|(i, _)| i)
                    .collect();
                if checked.is_empty() {
                    return;
                }
                let mut idx = state.bo_index.lock().unwrap();
                let next = checked
                    .iter()
                    .find(|i| **i > *idx)
                    .or_else(|| checked.first())
                    .copied()
                    .unwrap();
                *idx = next;
                *state.bo_step.lock().unwrap() = 0;
            }
            emit_bo(app);
            if let Some(w) = app.get_webview_window("buildorder") {
                let _ = w.show();
            }
        }
        HotkeyAction::BoPrevStep | HotkeyAction::BoNextStep => {
            {
                let s = state.settings.lock().unwrap();
                let idx = *state.bo_index.lock().unwrap();
                let count = s
                    .buildorders
                    .values()
                    .nth(idx)
                    .map(|c| bo_step_count(c))
                    .unwrap_or(1);
                let mut step = state.bo_step.lock().unwrap();
                *step = if action == HotkeyAction::BoNextStep {
                    (*step + 1).min(count.saturating_sub(1))
                } else {
                    step.saturating_sub(1)
                };
            }
            emit_bo(app);
        }
    }
}

// ---------------- window / hotkey helpers ----------------

fn toggle_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
        }
    }
}

fn register_hotkeys(app: &AppHandle, s: &Settings) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    for (key, action) in s.hotkeys() {
        let result = gs.on_shortcut(key.as_str(), move |app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                run_action(app, action);
            }
        });
        if let Err(e) = result {
            log::error!("failed to register hotkey '{key}': {e}");
        }
    }
}

// ---------------- poller ----------------

async fn poller(app: AppHandle) {
    loop {
        let (profile_id, interval) = {
            let state = app.state::<AppState>();
            let s = state.settings.lock().unwrap();
            (s.profile_id, s.interval.max(5))
        };

        if let Some(pid) = profile_id {
            let state = app.state::<AppState>();
            match api::get_last_game(&state.http, pid).await {
                Ok(game) => {
                    let started = game["started_at"].as_str().map(String::from);
                    let prevented = *state.prevent_update.lock().unwrap();
                    let is_new = {
                        let last = state.last_started.lock().unwrap();
                        started.is_some() && *last != started
                    };
                    if is_new && !prevented {
                        let processed = api::process_game(&game, pid);
                        *state.last_started.lock().unwrap() = started;
                        *state.last_game.lock().unwrap() = Some(processed.clone());
                        state.ws.send_player_data(&processed);
                        let _ = app.emit("game_data", &processed);
                        let show = {
                            let s = state.settings.lock().unwrap();
                            s.show_overlay_on_new_game
                        };
                        let ongoing = game["ongoing"].as_bool().unwrap_or(false);
                        if show && ongoing {
                            if let Some(w) = app.get_webview_window("overlay") {
                                let _ = w.show();
                            }
                        }
                    }
                    let _ = app.emit("poll_ok", game["ongoing"].as_bool().unwrap_or(false));
                }
                Err(e) => {
                    log::warn!("poll failed: {e}");
                    let _ = app.emit("poll_error", e);
                }
            }
        }

        let state = app.state::<AppState>();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(interval),
            state.force_check.notified(),
        )
        .await;
    }
}

// ---------------- entry ----------------

pub fn run() {
    env_logger::init();

    let s = Settings::load(&config_dir_static());
    let ws_server = ws::WsServer::new();
    let state = AppState {
        settings: Mutex::new(s.clone()),
        last_game: Mutex::new(None),
        last_started: Mutex::new(None),
        prevent_update: Mutex::new(false),
        bo_index: Mutex::new(0),
        bo_step: Mutex::new(0),
        force_check: Notify::new(),
        http: reqwest::Client::builder()
            .user_agent("AoE4-Overlay-Rust/0.1")
            .build()
            .unwrap(),
        ws: ws_server.clone(),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Second launch: focus the existing control panel instead
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();

            // Tray icon: left-click opens the panel; menu for overlay/quit
            {
                use tauri::menu::{MenuBuilder, MenuItemBuilder};
                use tauri::tray::TrayIconBuilder;
                let open = MenuItemBuilder::with_id("open", "Open control panel").build(app)?;
                let ovl = MenuItemBuilder::with_id("overlay", "Show / hide overlay").build(app)?;
                let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
                let menu = MenuBuilder::new(app).items(&[&open, &ovl, &quit]).build()?;
                TrayIconBuilder::new()
                    .icon(app.default_window_icon().unwrap().clone())
                    .tooltip("AoE4 Overlay")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "open" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                            }
                        }
                        "overlay" => toggle_window(app, "overlay"),
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let tauri::tray::TrayIconEvent::Click {
                            button: tauri::tray::MouseButton::Left,
                            button_state: tauri::tray::MouseButtonState::Up,
                            ..
                        } = event
                        {
                            if let Some(w) = tray.app_handle().get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                            }
                        }
                    })
                    .build(app)?;
            }

            for (label, geo) in [("overlay", s.overlay_geometry), ("buildorder", s.bo_geometry)] {
                if let Some(w) = app.get_webview_window(label) {
                    if let Some([x, y, width, height]) = geo {
                        let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
                        let _ = w.set_size(tauri::PhysicalSize::new(width as u32, height as u32));
                    } else if label == "overlay" && s.overlay_scale != 100 {
                        apply_overlay_scale(&handle, &app.state::<AppState>(), s.overlay_scale);
                    }
                    let _ = w.set_ignore_cursor_events(true);
                }
            }

            register_hotkeys(&handle, &s);

            let port = s.websocket_port;
            let h2 = handle.clone();
            tauri::async_runtime::spawn(ws::run(ws_server, port, move || {
                let state = h2.state::<AppState>();
                let mut msgs = vec![serde_json::json!({
                    "type": "color",
                    "data": state.settings.lock().unwrap().team_colors,
                })
                .to_string()];
                if let Some(d) = state.last_game.lock().unwrap().as_ref() {
                    msgs.push(
                        serde_json::json!({"type": "player_data", "data": d}).to_string(),
                    );
                }
                msgs
            }));

            tauri::async_runtime::spawn(poller(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_player,
            get_match_history,
            get_settings,
            save_settings,
            get_current_data,
            force_refresh,
            toggle_overlay,
            set_edit_mode,
            override_data,
            reset_override,
            bo_action,
            bo_select,
            get_bo,
            check_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
