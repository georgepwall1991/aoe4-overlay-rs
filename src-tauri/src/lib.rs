mod api;
mod settings;

use serde_json::Value;
use settings::Settings;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tokio::sync::Notify;

pub struct AppState {
    settings: Mutex<Settings>,
    last_game: Mutex<Option<Value>>,
    last_started: Mutex<Option<String>>,
    force_check: Notify,
    http: reqwest::Client,
}

fn config_dir(app: &AppHandle) -> std::path::PathBuf {
    app.path().app_config_dir().expect("no config dir")
}

#[tauri::command]
async fn search_player(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<api::PlayerSearchResult>, String> {
    api::search_players(&state.http, &query).await
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<'_, AppState>, new_settings: Settings) {
    let old_hotkey;
    {
        let mut s = state.settings.lock().unwrap();
        old_hotkey = s.overlay_hotkey.clone();
        let profile_changed = s.profile_id != new_settings.profile_id;
        *s = new_settings.clone();
        s.save(&config_dir(&app));
        if profile_changed {
            *state.last_started.lock().unwrap() = None;
            *state.last_game.lock().unwrap() = None;
        }
    }
    if old_hotkey != new_settings.overlay_hotkey {
        register_hotkey(&app, &old_hotkey, &new_settings.overlay_hotkey);
    }
    state.force_check.notify_one();
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

#[tauri::command]
fn toggle_overlay(app: AppHandle) {
    toggle_overlay_window(&app);
}

/// Edit mode: overlay becomes draggable/clickable with a visible frame.
#[tauri::command]
fn set_overlay_edit_mode(app: AppHandle, state: State<'_, AppState>, edit: bool) {
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.set_ignore_cursor_events(!edit);
        let _ = w.show();
        let _ = w.emit("edit_mode", edit);
        if !edit {
            // Persist geometry on leaving edit mode
            if let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) {
                let mut s = state.settings.lock().unwrap();
                s.overlay_geometry = Some([pos.x, pos.y, size.width as i32, size.height as i32]);
                s.save(&config_dir(&app));
            }
        }
    }
}

fn toggle_overlay_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("overlay") {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
        }
    }
}

fn register_hotkey(app: &AppHandle, old: &str, new: &str) {
    let gs = app.global_shortcut();
    if !old.is_empty() {
        let _ = gs.unregister(old);
    }
    if !new.is_empty() {
        if let Err(e) = gs.register(new) {
            log::error!("failed to register hotkey '{new}': {e}");
        }
    }
}

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
                    let is_new = {
                        let last = state.last_started.lock().unwrap();
                        started.is_some() && *last != started
                    };
                    if is_new {
                        let processed = api::process_game(&game, pid);
                        *state.last_started.lock().unwrap() = started;
                        *state.last_game.lock().unwrap() = Some(processed.clone());
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

pub fn run() {
    env_logger::init();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_overlay_window(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let handle = app.handle().clone();
            let s = Settings::load(&config_dir(&handle));

            // Restore overlay geometry and make it click-through
            if let Some(w) = app.get_webview_window("overlay") {
                if let Some([x, y, width, height]) = s.overlay_geometry {
                    let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
                    let _ = w.set_size(tauri::PhysicalSize::new(width as u32, height as u32));
                }
                let _ = w.set_ignore_cursor_events(true);
            }

            register_hotkey(&handle, "", &s.overlay_hotkey);

            app.manage(AppState {
                settings: Mutex::new(s),
                last_game: Mutex::new(None),
                last_started: Mutex::new(None),
                force_check: Notify::new(),
                http: reqwest::Client::builder()
                    .user_agent("AoE4-Overlay-Rust/0.1")
                    .build()
                    .unwrap(),
            });

            tauri::async_runtime::spawn(poller(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_player,
            get_settings,
            save_settings,
            get_current_data,
            force_refresh,
            toggle_overlay,
            set_overlay_edit_mode
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
