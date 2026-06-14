#![recursion_limit = "256"]

mod api;
mod bo_import;
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
async fn get_player_profile(state: State<'_, AppState>) -> Result<Value, String> {
    let pid = { state.settings.lock().unwrap().profile_id };
    let pid = pid.ok_or("no profile selected")?;
    api::get_player_profile(&state.http, pid).await
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<'_, AppState>, new_settings: Settings) {
    let scale_changed;
    let hotkeys_changed;
    {
        let mut s = state.settings.lock().unwrap();
        let profile_changed = s.profile_id != new_settings.profile_id;
        scale_changed = s.overlay_scale != new_settings.overlay_scale;
        hotkeys_changed = s.hotkeys() != new_settings.hotkeys();
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
    if hotkeys_changed {
        register_hotkeys(&app, &new_settings);
    }
    state.ws.send_colors(&new_settings.team_colors);
    let _ = app.emit("settings_changed", &new_settings);
    state.force_check.notify_one();
    emit_bo(&app);
}

#[tauri::command]
fn get_app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
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
    let tag = rel["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v');
    let newer = {
        let cur: Vec<u32> = env!("CARGO_PKG_VERSION")
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();
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
    let Some(w) = app.get_webview_window("overlay") else {
        return;
    };
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
        let _ = app.emit(
            "edit_mode",
            serde_json::json!({"window": window, "edit": edit}),
        );
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

#[tauri::command]
async fn bo_import_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<bo_import::ImportedBo, String> {
    bo_import::import_from_url(&state.http, &url).await
}

#[tauri::command]
async fn bo_search_guides(
    state: State<'_, AppState>,
    civ: Option<String>,
    order_by: Option<String>,
    author: Option<String>,
) -> Result<Value, String> {
    bo_import::search_aoe4guides(
        &state.http,
        civ.as_deref(),
        order_by.as_deref(),
        author.as_deref(),
    )
    .await
}

fn strip_json_code_fence(content: &str) -> &str {
    let t = content.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let body = rest
        .strip_prefix("jsonc")
        .or_else(|| rest.strip_prefix("json"))
        .or_else(|| rest.strip_prefix("javascript"))
        .or_else(|| rest.strip_prefix("js"))
        .unwrap_or(rest)
        .trim_start();
    body.strip_suffix("```").unwrap_or(body).trim()
}

fn parse_bo_json_lenient(content: &str) -> Option<Value> {
    parse_bo_json_value(content)
        .or_else(|| extract_json_like_slice(content).and_then(parse_bo_json_value))
}

fn parse_bo_json_value(content: &str) -> Option<Value> {
    serde_json::from_str::<Value>(content)
        .ok()
        .or_else(|| repair_json_syntax(content).and_then(|fixed| serde_json::from_str(&fixed).ok()))
}

fn extract_json_like_slice(content: &str) -> Option<&str> {
    let s = content.trim();
    if s.starts_with(['{', '[']) {
        return Some(s);
    }
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    for (pos, (start, open)) in chars.iter().copied().enumerate() {
        if !matches!(open, '{' | '[') {
            continue;
        }
        let close = if open == '{' { '}' } else { ']' };
        let mut stack = vec![close];
        let mut quote = '\0';
        let mut escaped = false;
        for (idx, (byte, c)) in chars.iter().copied().enumerate().skip(pos + 1) {
            if quote != '\0' {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if json_quote_closes(quote, c) && !is_word_apostrophe_indexed(&chars, idx) {
                    quote = '\0';
                }
                continue;
            }
            if c == '"' || c == '\'' || c == '‘' {
                quote = c;
                continue;
            }
            match c {
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' if stack.last() == Some(&c) => {
                    stack.pop();
                    if stack.is_empty() {
                        let end = byte + c.len_utf8();
                        let candidate = s[start..end].trim();
                        if is_plausible_extracted_json_like(candidate) {
                            return Some(candidate);
                        }
                        break;
                    }
                }
                '}' | ']' => break,
                _ => {}
            }
        }
    }
    None
}

fn is_plausible_extracted_json_like(candidate: &str) -> bool {
    if candidate.starts_with('{') {
        return true;
    }
    let Some(inner) = candidate
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::trim)
    else {
        return false;
    };
    !inner.is_empty() && !matches!(inner, "x" | "X")
}

fn repair_json_syntax(content: &str) -> Option<String> {
    let mut fixed = content
        .trim_start_matches('\u{feff}')
        .replace(['“', '”'], "\"");
    fixed = remove_json_comments(&fixed);
    fixed = quote_bare_json_keys(&fixed);
    fixed = normalize_json_key_equals(&fixed);
    fixed = convert_single_quoted_json_strings(&fixed);
    fixed = quote_known_bare_string_values(&fixed);
    fixed = quote_known_bare_string_array_values(&fixed);
    fixed = convert_non_json_literals(&fixed);
    fixed = normalize_json_semicolons(&fixed);
    fixed = insert_missing_json_commas(&fixed);
    fixed = quote_bare_json_keys(&fixed);
    fixed = normalize_json_key_equals(&fixed);
    fixed = remove_trailing_json_commas(&fixed);
    (fixed != content).then_some(fixed)
}

fn remove_json_comments(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if json_quote_closes(quote, c) && !is_word_apostrophe(&chars, i) {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' || c == '‘' {
            in_string = true;
            quote = c;
            out.push(c);
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            i += 2;
            while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn quote_bare_json_keys(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;
    let mut after_object_delim = true;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if json_quote_closes(quote, c) && !is_word_apostrophe(&chars, i) {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' || c == '‘' {
            in_string = true;
            quote = c;
            out.push(c);
            after_object_delim = false;
            i += 1;
            continue;
        }
        if c == '{' || c == ',' {
            after_object_delim = true;
            out.push(c);
            i += 1;
            continue;
        }
        if after_object_delim && c.is_ascii_whitespace() {
            out.push(c);
            i += 1;
            continue;
        }
        if after_object_delim && (c.is_ascii_alphabetic() || c == '_') {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let key: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_whitespace() {
                j += 1;
            }
            if matches!(chars.get(j), Some(':') | Some('=')) {
                out.push('"');
                out.push_str(&key);
                out.push('"');
                while i < j {
                    out.push(chars[i]);
                    i += 1;
                }
                out.push(chars[j]);
                i = j + 1;
            } else {
                out.push_str(&key);
            }
            after_object_delim = false;
            continue;
        }
        after_object_delim = false;
        out.push(c);
        i += 1;
    }
    out
}

fn normalize_json_key_equals(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            let start_out_len = out.len();
            out.push(c);
            i += 1;
            let mut escaped_key = false;
            while i < chars.len() {
                let kc = chars[i];
                out.push(kc);
                i += 1;
                if escaped_key {
                    escaped_key = false;
                } else if kc == '\\' {
                    escaped_key = true;
                } else if kc == '"' {
                    break;
                }
            }
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if chars.get(j) == Some(&'=') && key_follows_json_object_delim(&out[..start_out_len]) {
                while i < j {
                    out.push(chars[i]);
                    i += 1;
                }
                out.push(':');
                i = j + 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn key_follows_json_object_delim(prefix: &str) -> bool {
    prefix
        .chars()
        .rev()
        .find(|c| !c.is_whitespace())
        .is_none_or(|c| c == '{' || c == ',')
}

fn json_quote_closes(open: char, c: char) -> bool {
    match open {
        '‘' => c == '’',
        _ => c == open,
    }
}

fn convert_single_quoted_json_strings(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_double = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_double {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_double = true;
            out.push(c);
            i += 1;
        } else if c == '\'' || c == '‘' {
            let close = if c == '‘' { '’' } else { '\'' };
            i += 1;
            let mut value = String::new();
            let mut single_escaped = false;
            while i < chars.len() {
                let ch = chars[i];
                if single_escaped {
                    value.push(if ch == '\'' { '\'' } else { ch });
                    single_escaped = false;
                } else if ch == '\\' {
                    single_escaped = true;
                } else if ch == close && !is_word_apostrophe(&chars, i) {
                    break;
                } else {
                    value.push(ch);
                }
                i += 1;
            }
            if i < chars.len() && chars[i] == close {
                i += 1;
            }
            out.push_str(&serde_json::to_string(&value).unwrap_or_else(|_| "\"\"".to_string()));
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

const BARE_STRING_VALUE_KEYS: &[&str] = &[
    "name",
    "title",
    "buildName",
    "build_name",
    "civilization",
    "civilisation",
    "civ",
    "source",
    "url",
    "link",
    "buildUrl",
    "build_url",
    "sourceUrl",
    "source_url",
    "time",
    "timestamp",
    "timecode",
    "timing",
    "at",
    "start",
    "start_time",
    "startTime",
    "minute",
    "time_text",
    "timeText",
    "note",
    "notes",
    "text",
    "description",
    "action",
    "task",
    "phase",
    "ageName",
    "era",
    "map",
    "strategy",
    "author",
    "video",
    "season",
];

fn is_bare_string_value_key(key: &str) -> bool {
    BARE_STRING_VALUE_KEYS.contains(&key)
}

const BARE_STRING_ARRAY_KEYS: &[&str] = &[
    "build_order",
    "buildOrder",
    "build order",
    "build-order",
    "buildorder",
    "build_order_steps",
    "buildOrderSteps",
    "build order steps",
    "build-order-steps",
    "buildordersteps",
    "steps",
    "order",
    "sequence",
    "timeline",
    "plan",
    "notes",
    "actions",
    "tasks",
    "civilization",
    "civilisation",
    "civilizations",
    "civilisations",
    "civs",
];

fn is_bare_string_array_key(key: &str) -> bool {
    BARE_STRING_ARRAY_KEYS.contains(&key)
}

fn quote_known_bare_string_values(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c != '"' {
            out.push(c);
            i += 1;
            continue;
        }

        let key_start_out = out.len();
        let mut key = String::new();
        out.push(c);
        i += 1;
        let mut escaped_key = false;
        while i < chars.len() {
            let kc = chars[i];
            out.push(kc);
            i += 1;
            if escaped_key {
                key.push(kc);
                escaped_key = false;
            } else if kc == '\\' {
                escaped_key = true;
            } else if kc == '"' {
                break;
            } else {
                key.push(kc);
            }
        }
        if !is_bare_string_value_key(&key) || !key_follows_json_object_delim(&out[..key_start_out])
        {
            continue;
        }

        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) != Some(&':') {
            continue;
        }
        while i <= j {
            out.push(chars[i]);
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            out.push(chars[i]);
            i += 1;
        }
        let Some(first) = chars.get(i).copied() else {
            break;
        };
        if matches!(first, '"' | '{' | '[') || bare_json_literal_starts(&chars, i) {
            continue;
        }
        let value_start = i;
        while i < chars.len() && !matches!(chars[i], ',' | '}' | ']' | ';') {
            i += 1;
        }
        let value_end = i;
        let mut trim_end = value_end;
        while trim_end > value_start && chars[trim_end - 1].is_whitespace() {
            trim_end -= 1;
        }
        let value: String = chars[value_start..trim_end].iter().collect();
        if value.trim().is_empty() {
            for ch in &chars[value_start..value_end] {
                out.push(*ch);
            }
            continue;
        }
        out.push_str(&serde_json::to_string(value.trim()).unwrap_or_else(|_| "\"\"".to_string()));
        for ch in &chars[trim_end..value_end] {
            out.push(*ch);
        }
    }
    out
}

fn quote_known_bare_string_array_values(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c != '"' {
            out.push(c);
            i += 1;
            continue;
        }

        let key_start_out = out.len();
        let mut key = String::new();
        out.push(c);
        i += 1;
        let mut escaped_key = false;
        while i < chars.len() {
            let kc = chars[i];
            out.push(kc);
            i += 1;
            if escaped_key {
                key.push(kc);
                escaped_key = false;
            } else if kc == '\\' {
                escaped_key = true;
            } else if kc == '"' {
                break;
            } else {
                key.push(kc);
            }
        }
        if !is_bare_string_array_key(&key) || !key_follows_json_object_delim(&out[..key_start_out])
        {
            continue;
        }
        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) != Some(&':') {
            continue;
        }
        while i <= j {
            out.push(chars[i]);
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            out.push(chars[i]);
            i += 1;
        }
        if chars.get(i) != Some(&'[') {
            continue;
        }
        out.push('[');
        i += 1;
        i = quote_bare_items_in_array(&chars, i, &mut out);
    }
    out
}

fn quote_bare_items_in_array(chars: &[char], mut i: usize, out: &mut String) -> usize {
    while i < chars.len() {
        let c = chars[i];
        if c == ']' {
            out.push(c);
            return i + 1;
        }
        if c.is_whitespace() || c == ',' {
            out.push(c);
            i += 1;
            continue;
        }
        if c == '"' {
            i = copy_quoted_json_string(chars, i, out);
            continue;
        }
        if c == '{' || c == '[' {
            let (balanced, next_i) = copy_balanced_json_value(chars, i);
            out.push_str(&quote_known_bare_string_array_values(&balanced));
            i = next_i;
            continue;
        }
        let value_start = i;
        while i < chars.len() && !matches!(chars[i], ',' | ']') {
            i += 1;
        }
        let value_end = i;
        let mut trim_end = value_end;
        while trim_end > value_start && chars[trim_end - 1].is_whitespace() {
            trim_end -= 1;
        }
        let value: String = chars[value_start..trim_end].iter().collect();
        let trimmed = value.trim();
        if trimmed.is_empty()
            || bare_json_literal_starts(chars, value_start)
            || is_json_number_literal(trimmed)
        {
            for ch in &chars[value_start..value_end] {
                out.push(*ch);
            }
        } else {
            out.push_str(&serde_json::to_string(trimmed).unwrap_or_else(|_| "\"\"".to_string()));
            for ch in &chars[trim_end..value_end] {
                out.push(*ch);
            }
        }
    }
    i
}

fn copy_quoted_json_string(chars: &[char], mut i: usize, out: &mut String) -> usize {
    let mut escaped = false;
    out.push(chars[i]);
    i += 1;
    while i < chars.len() {
        let c = chars[i];
        out.push(c);
        i += 1;
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        }
    }
    i
}

fn copy_balanced_json_value(chars: &[char], mut i: usize) -> (String, usize) {
    let mut out = String::new();
    let mut stack = vec![if chars[i] == '{' { '}' } else { ']' }];
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        out.push(c);
        i += 1;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' if stack.last() == Some(&c) => {
                stack.pop();
                if stack.is_empty() {
                    break;
                }
            }
            _ => {}
        }
    }
    (out, i)
}

fn is_json_number_literal(value: &str) -> bool {
    serde_json::from_str::<Value>(value).is_ok_and(|value| value.is_number())
}

fn bare_json_literal_starts(chars: &[char], i: usize) -> bool {
    [
        "true",
        "false",
        "null",
        "undefined",
        "NaN",
        "Infinity",
        "+Infinity",
        "-Infinity",
    ]
    .iter()
    .any(|literal| {
        let end = i + literal.len();
        chars.get(i..end).is_some_and(|slice| {
            slice.iter().collect::<String>() == *literal
                && chars
                    .get(end)
                    .is_none_or(|c| matches!(c, ',' | '}' | ']' | ';') || c.is_whitespace())
        })
    })
}

fn is_word_apostrophe(chars: &[char], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1).and_then(|idx| chars.get(idx)) else {
        return false;
    };
    let Some(next) = chars.get(i + 1) else {
        return false;
    };
    prev.is_alphanumeric() && next.is_alphanumeric()
}

fn is_word_apostrophe_indexed(chars: &[(usize, char)], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1).and_then(|idx| chars.get(idx)) else {
        return false;
    };
    let Some(next) = chars.get(i + 1) else {
        return false;
    };
    prev.1.is_alphanumeric() && next.1.is_alphanumeric()
}

fn convert_non_json_literals(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            quote = c;
            out.push(c);
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || matches!(c, '+' | '-') {
            let start = i;
            if matches!(c, '+' | '-') {
                i += 1;
            }
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            match word.as_str() {
                "True" => out.push_str("true"),
                "False" => out.push_str("false"),
                "None" => out.push_str("null"),
                "undefined" | "NaN" | "Infinity" | "+Infinity" | "-Infinity" => {
                    out.push_str("null")
                }
                _ => out.push_str(&word),
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn json_value_can_end(c: char) -> bool {
    c == '}' || c == ']' || c == '"' || c.is_ascii_digit()
}

fn json_value_can_start(c: char) -> bool {
    c == '{' || c == '[' || c == '"' || c == '-' || c == '_' || c.is_ascii_alphanumeric()
}

fn insert_missing_json_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut prev_sig: Option<char> = None;
    let mut pending_ws = String::new();

    for c in input.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                prev_sig = Some('"');
            }
            continue;
        }

        if c.is_whitespace() {
            pending_ws.push(c);
            continue;
        }

        if !pending_ws.is_empty() {
            if prev_sig.is_some_and(json_value_can_end) && json_value_can_start(c) {
                out.push(',');
            }
            out.push_str(&pending_ws);
            pending_ws.clear();
        }

        out.push(c);
        if c == '"' {
            in_string = true;
            escaped = false;
        } else {
            prev_sig = Some(c);
        }
    }
    out.push_str(&pending_ws);
    out
}

fn normalize_json_semicolons(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            escaped = false;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ';' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_whitespace() {
                j += 1;
            }
            if matches!(chars.get(j), None | Some('}' | ']')) {
                i += 1;
                continue;
            }
            out.push(',');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn remove_trailing_json_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            quote = c;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_whitespace() {
                j += 1;
            }
            if matches!(chars.get(j), Some('}' | ']')) {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn bo_step_count(content: &str) -> usize {
    let content = strip_json_code_fence(content);
    parse_bo_json_lenient(content)
        .and_then(|v| bo_step_collection_len(&v))
        .or_else(|| bo_table_step_count(content))
        .or_else(|| bo_tsv_table_step_count(content))
        .or_else(|| bo_csv_table_step_count(content))
        .or_else(|| bo_timed_text_step_count(content))
        .or_else(|| bo_plain_list_step_count(content))
        // plain TXT: each non-empty line is a rough fallback step count
        .unwrap_or_else(|| content.lines().filter(|l| !l.trim().is_empty()).count())
        .max(1)
}

fn bo_table_step_count(content: &str) -> Option<usize> {
    let mut non_empty = 0;
    let mut pipe_lines = 0;
    let mut parsed = 0;
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        non_empty += 1;
        if line.contains('|') {
            pipe_lines += 1;
        }
        if bo_piped_step_line_has_content(line) {
            parsed += 1;
        }
    }
    (parsed > 0 && (parsed >= 2 || pipe_lines == non_empty)).then_some(parsed)
}

fn bo_piped_step_line_has_content(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('|').trim();
    if trimmed.is_empty() || !trimmed.contains('|') {
        return false;
    }
    let cells: Vec<&str> = trimmed
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect();
    if cells.len() < 2 || cells.iter().all(|cell| bo_is_table_separator_cell(cell)) {
        return false;
    }
    let has_time = cells.iter().any(|cell| bo_is_table_clock_cell(cell));
    let has_resources = cells.iter().any(|cell| bo_is_table_resource_cell(cell))
        || bo_table_resource_run(&cells).is_some();
    let has_note = cells.iter().any(|cell| {
        !bo_is_table_clock_cell(cell)
            && !bo_is_table_resource_cell(cell)
            && !bo_is_table_header_cell(cell)
            && !bo_is_table_separator_cell(cell)
            && !bo_table_int_cell(cell)
    });
    (has_time || has_resources) && has_note
}

fn bo_tsv_table_step_count(content: &str) -> Option<usize> {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut headers: Option<Vec<&str>> = None;
    let mut parsed = 0;
    for line in lines {
        if !line.contains('\t') {
            continue;
        }
        let cells: Vec<&str> = line
            .split('\t')
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .collect();
        if cells.len() < 2 {
            continue;
        }
        if bo_is_table_header_row(&cells) {
            headers = Some(cells);
            continue;
        }
        if headers
            .as_ref()
            .is_some_and(|header| header.len() == cells.len())
            && bo_headered_table_row_has_content(&cells)
        {
            parsed += 1;
        }
    }
    (parsed > 0).then_some(parsed)
}

fn bo_csv_table_step_count(content: &str) -> Option<usize> {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut headers: Option<Vec<String>> = None;
    let mut parsed = 0;
    for line in lines {
        if !line.contains(',') {
            continue;
        }
        let cells = bo_csv_cells(line);
        if cells.len() < 2 {
            continue;
        }
        let cell_refs: Vec<&str> = cells.iter().map(String::as_str).collect();
        if bo_is_table_header_row(&cell_refs) {
            headers = Some(cells);
            continue;
        }
        if headers
            .as_ref()
            .is_some_and(|header| header.len() == cells.len())
            && bo_headered_table_row_has_content(&cell_refs)
        {
            parsed += 1;
        }
    }
    (parsed > 0).then_some(parsed)
}

fn bo_csv_cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                let trimmed = cell.trim();
                if !trimmed.is_empty() {
                    cells.push(trimmed.to_string());
                }
                cell.clear();
            }
            _ => cell.push(ch),
        }
    }
    let trimmed = cell.trim();
    if !trimmed.is_empty() {
        cells.push(trimmed.to_string());
    }
    cells
}

fn bo_is_table_header_row(cells: &[&str]) -> bool {
    cells
        .iter()
        .filter(|cell| bo_is_table_header_cell(cell))
        .count()
        >= 2.max(cells.len().div_ceil(2))
}

fn bo_headered_table_row_has_content(cells: &[&str]) -> bool {
    let has_note = cells.iter().any(|cell| {
        !bo_is_table_header_cell(cell)
            && !bo_is_table_separator_cell(cell)
            && !bo_table_int_cell(cell)
            && !bo_is_table_resource_cell(cell)
            && !bo_is_table_clock_cell(cell)
    });
    let has_signal = cells.iter().any(|cell| {
        bo_is_table_clock_cell(cell)
            || bo_is_table_resource_cell(cell)
            || bo_table_int_cell(cell)
            || !bo_is_table_header_cell(cell)
    });
    has_signal && has_note
}

fn bo_is_table_separator_cell(cell: &str) -> bool {
    let s = cell.trim();
    !s.is_empty() && s.chars().all(|c| matches!(c, '-' | ':' | '=' | '+' | ' '))
}

fn bo_is_table_header_cell(cell: &str) -> bool {
    matches!(
        cell.trim()
            .to_ascii_lowercase()
            .replace(|c: char| !c.is_ascii_alphanumeric(), "")
            .as_str(),
        "time"
            | "timing"
            | "at"
            | "start"
            | "starttime"
            | "when"
            | "minute"
            | "minutes"
            | "timestamp"
            | "timecode"
            | "timetext"
            | "age"
            | "phase"
            | "era"
            | "f"
            | "food"
            | "w"
            | "wood"
            | "g"
            | "gold"
            | "s"
            | "stone"
            | "b"
            | "builder"
            | "builders"
            | "vill"
            | "villager"
            | "villcount"
            | "villagercount"
            | "villagers"
            | "vills"
            | "worker"
            | "workercount"
            | "workers"
            | "pop"
            | "popcap"
            | "popcount"
            | "population"
            | "populationcount"
            | "supply"
            | "supplycount"
            | "supplyused"
            | "foodvills"
            | "woodvills"
            | "goldvills"
            | "stonevills"
            | "buildervills"
            | "buildersvills"
            | "res"
            | "resources"
            | "resource"
            | "allocation"
            | "allocations"
            | "resourceallocation"
            | "resourceallocations"
            | "villagerallocation"
            | "villagerallocations"
            | "villagerson"
            | "eco"
            | "econ"
            | "economy"
            | "action"
            | "actions"
            | "build"
            | "note"
            | "notes"
            | "description"
            | "instruction"
            | "instructions"
            | "plan"
            | "step"
            | "steps"
            | "todo"
            | "todos"
            | "task"
            | "tasks"
    )
}

fn bo_is_table_clock_cell(cell: &str) -> bool {
    let s = cell.trim().trim_start_matches(['~', '=']);
    if bo_is_seconds_clock_cell(s) || bo_is_stopwatch_clock_cell(s) {
        return true;
    }
    let Some((m, sec)) = s.split_once(':').or_else(|| s.split_once('.')) else {
        return s
            .strip_suffix("min")
            .or_else(|| s.strip_suffix("mins"))
            .is_some_and(|m| m.trim().parse::<u32>().is_ok());
    };
    !m.is_empty()
        && !sec.is_empty()
        && m.chars().all(|c| c.is_ascii_digit())
        && sec.chars().all(|c| c.is_ascii_digit())
        && sec.parse::<u32>().is_ok_and(|n| n < 60)
}

fn bo_is_seconds_clock_cell(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    for suffix in ["seconds", "second", "secs", "sec", "s"] {
        if let Some(n) = lower.strip_suffix(suffix) {
            return !n.trim().is_empty() && n.trim().chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

fn bo_is_stopwatch_clock_cell(s: &str) -> bool {
    let compact = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let Some(m_pos) = compact.find('m') else {
        return false;
    };
    if m_pos == 0 || !compact[..m_pos].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let rest = &compact[m_pos + 1..];
    let rest = rest
        .strip_prefix("inutes")
        .or_else(|| rest.strip_prefix("inute"))
        .or_else(|| rest.strip_prefix("ins"))
        .or_else(|| rest.strip_prefix("in"))
        .or_else(|| rest.strip_prefix('s'))
        .unwrap_or(rest);
    let rest = rest
        .strip_suffix("seconds")
        .or_else(|| rest.strip_suffix("second"))
        .or_else(|| rest.strip_suffix("secs"))
        .or_else(|| rest.strip_suffix("sec"))
        .or_else(|| rest.strip_suffix('s'))
        .unwrap_or(rest);
    !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
        && rest.parse::<u32>().is_ok_and(|n| n < 60)
}

fn bo_is_table_resource_cell(cell: &str) -> bool {
    let clean = cell.trim().trim_matches(['(', ')']);
    if bo_is_dash_resource_cell(clean) {
        return true;
    }
    let parts: Vec<&str> = clean
        .split(['/', ','])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    (parts.len() == 4 || parts.len() == 5) && parts.iter().all(|part| bo_table_int_cell(part))
}

fn bo_is_dash_resource_cell(cell: &str) -> bool {
    let parts = cell
        .split('-')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (parts.len() == 4 || parts.len() == 5)
        && parts
            .iter()
            .all(|part| part.chars().all(|c| c.is_ascii_digit()))
}

fn bo_table_resource_run(cells: &[&str]) -> Option<usize> {
    for window in [5, 4] {
        if cells.len() < window {
            continue;
        }
        if cells
            .windows(window)
            .any(|items| items.iter().all(|cell| bo_table_int_cell(cell)))
        {
            return Some(window);
        }
    }
    None
}

fn bo_table_int_cell(cell: &str) -> bool {
    let s = cell.trim();
    !s.is_empty()
        && s.strip_prefix('-')
            .unwrap_or(s)
            .chars()
            .all(|c| c.is_ascii_digit())
}

fn bo_timed_text_logical_lines(line: &str) -> Vec<String> {
    let parts: Vec<String> = line
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    if parts.len() >= 2
        && parts
            .iter()
            .filter(|part| bo_timed_text_line_has_content(part))
            .count()
            >= 2
    {
        parts
    } else {
        vec![line.trim().to_string()]
    }
}

fn bo_timed_text_step_count(content: &str) -> Option<usize> {
    let logical_lines: Vec<String> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .flat_map(bo_timed_text_logical_lines)
        .collect();
    if logical_lines.is_empty() {
        return None;
    }
    let parsed = logical_lines
        .iter()
        .filter(|line| bo_timed_text_line_has_content(line))
        .count();
    (parsed > 0 && (parsed >= 2 || parsed == logical_lines.len())).then_some(parsed)
}

fn bo_plain_list_step_count(content: &str) -> Option<usize> {
    let mut steps = 0usize;
    let mut marked = 0usize;
    let mut seen_marked = false;
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if bo_plain_text_age_heading(line) {
            continue;
        }
        let body = bo_strip_plain_step_prefix(line)
            .trim_start_matches('#')
            .trim_matches(|c: char| {
                c.is_whitespace()
                    || matches!(c, ':' | ';' | '|' | ',' | '.' | '-' | '–' | '—' | '>')
            })
            .trim();
        if body.is_empty() || bo_plain_text_age_heading(body) {
            continue;
        }
        let is_marked = bo_plain_text_list_marker(line);
        if !seen_marked && !is_marked {
            continue;
        }
        if seen_marked && !is_marked && bo_plain_text_footer_heading(line) {
            break;
        }
        steps += 1;
        if is_marked {
            marked += 1;
            seen_marked = true;
        }
    }
    (steps >= 2 && marked >= 2.max(steps.div_ceil(2))).then_some(steps)
}

fn bo_plain_text_list_marker(line: &str) -> bool {
    let s = line.trim_start();
    if s.starts_with("- ") || s.starts_with("* ") || s.starts_with("• ") {
        return true;
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("[ ] ") || lower.starts_with("[x] ") {
        return true;
    }
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    (1..=3).contains(&digits)
        && s[digits..]
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '.' | ')'))
        && s[digits + 1..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
}

fn bo_plain_text_footer_heading(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    lower == "note"
        || lower == "notes"
        || lower.starts_with("notes:")
        || lower.starts_with("link to ")
        || lower.starts_with("created by")
        || lower.starts_with("uploaded by")
        || lower.starts_with("generated by")
}

fn bo_strip_plain_step_prefix(line: &str) -> &str {
    let s = line.trim();
    if let Some(rest) = s
        .strip_prefix("- ")
        .or_else(|| s.strip_prefix("* "))
        .or_else(|| s.strip_prefix("• "))
    {
        return rest.trim();
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("[ ] ") || lower.starts_with("[x] ") {
        return s[4..].trim();
    }
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits)
        && s[digits..]
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '.' | ')'))
        && s[digits + 1..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        return s[digits + 1..].trim();
    }
    s
}

fn bo_plain_text_age_heading(line: &str) -> bool {
    let text = bo_strip_plain_step_prefix(line)
        .trim_start_matches('#')
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—'))
        .to_ascii_lowercase();
    matches!(
        text.as_str(),
        "age i"
            | "age 1"
            | "i"
            | "1"
            | "dark"
            | "dark age"
            | "age ii"
            | "age 2"
            | "ii"
            | "2"
            | "feudal"
            | "feudal age"
            | "age iii"
            | "age 3"
            | "iii"
            | "3"
            | "castle"
            | "castle age"
            | "age iv"
            | "age 4"
            | "iv"
            | "4"
            | "imperial"
            | "imperial age"
    )
}

fn bo_timed_text_line_has_content(line: &str) -> bool {
    if bo_plain_text_list_marker(line) {
        let body = bo_strip_plain_step_prefix(line);
        if bo_timed_text_line_has_resource_prefix(body) {
            return true;
        }
    }
    let tokens: Vec<String> = line
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c: char| {
                    matches!(
                        c,
                        '(' | ')' | '[' | ']' | '{' | '}' | '|' | '-' | '–' | '—' | ',' | ';'
                    )
                })
                .to_string()
        })
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.len() < 2 {
        return false;
    }
    let has_time =
        tokens.iter().any(|token| bo_is_table_clock_cell(token)) || bo_is_stopwatch_text_line(line);
    let has_note = tokens.iter().any(|token| {
        !bo_is_table_clock_cell(token)
            && !bo_is_table_resource_cell(token)
            && !bo_is_table_header_cell(token)
            && !bo_is_table_separator_cell(token)
            && !bo_table_int_cell(token)
            && !bo_is_plain_step_marker(token)
    });
    has_time && has_note
}

fn bo_timed_text_line_has_resource_prefix(line: &str) -> bool {
    let body = line.trim_start();
    if let Some(rest) = body.strip_prefix('(') {
        if let Some(close) = rest.find(')') {
            return bo_is_table_resource_cell(&rest[..close]);
        }
    }
    body.split_whitespace()
        .next()
        .is_some_and(bo_is_table_resource_cell)
}

fn bo_is_stopwatch_text_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '(' | ')' | '[' | ']' | '{' | '}' | '|' | '-' | '–' | '—' | ',' | ';'
                )
        })
        .filter(|token| !token.is_empty())
        .any(bo_is_stopwatch_clock_cell)
        || lower
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(4)
            .any(|w| {
                w[0].chars().all(|c| c.is_ascii_digit())
                    && matches!(w[1], "min" | "mins" | "minute" | "minutes")
                    && w[2].chars().all(|c| c.is_ascii_digit())
                    && matches!(w[3], "s" | "sec" | "secs" | "second" | "seconds")
            })
}

fn bo_is_plain_step_marker(token: &str) -> bool {
    let s = token.trim();
    let Some(last) = s.chars().last() else {
        return false;
    };
    matches!(last, '.' | ')')
        && s[..s.len() - last.len_utf8()]
            .chars()
            .all(|c| c.is_ascii_digit())
}

fn bo_step_key_order(key: &str) -> Option<usize> {
    let s = key.trim();
    if let Ok(n) = s.parse::<usize>() {
        return Some(n);
    }
    let lower = s.to_ascii_lowercase();
    let tail = lower
        .strip_prefix("step")
        .map(|tail| tail.trim_start_matches([' ', '_', '-']))?;
    tail.parse::<usize>().ok()
}

fn bo_raw_step_value_has_content(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(items) => items.iter().any(bo_raw_step_value_has_content),
        Value::Object(map) => map.values().any(bo_raw_step_value_has_content),
    }
}

fn bo_is_sectioned_step(value: &Value) -> bool {
    value["steps"]
        .as_array()
        .is_some_and(|children| !children.is_empty())
        && (value["type"].is_string() || !value["gameplan"].is_null() || !value["age"].is_null())
}

fn bo_step_array_len(array: &[Value]) -> usize {
    if array.iter().any(bo_is_sectioned_step) {
        return array
            .iter()
            .map(|step| {
                if bo_is_sectioned_step(step) {
                    step["steps"]
                        .as_array()
                        .map(|children| {
                            children
                                .iter()
                                .filter(|child| bo_raw_step_value_has_content(child))
                                .count()
                        })
                        .unwrap_or(0)
                } else {
                    usize::from(bo_raw_step_value_has_content(step))
                }
            })
            .sum();
    }
    array.len()
}

const BO_STEP_COLLECTION_KEYS: &[&str] = &[
    "build_order",
    "buildOrder",
    "build order",
    "build-order",
    "buildorder",
    "build_order_steps",
    "buildOrderSteps",
    "build order steps",
    "build-order-steps",
    "buildordersteps",
    "steps",
    "order",
    "sequence",
    "timeline",
    "plan",
];

const BO_STEP_LIKE_KEYS: &[&str] = &[
    "time",
    "timestamp",
    "timecode",
    "timing",
    "at",
    "start",
    "start_time",
    "startTime",
    "minute",
    "time_text",
    "timeText",
    "age",
    "phase",
    "ageName",
    "era",
    "notes",
    "note",
    "text",
    "description",
    "action",
    "actions",
    "task",
    "tasks",
    "population_count",
    "population",
    "pop",
    "pop_count",
    "popCount",
    "pop_cap",
    "popCap",
    "supply",
    "supply_count",
    "supplyCount",
    "supply_used",
    "supplyUsed",
    "villager_count",
    "vill",
    "villager",
    "villagers",
    "vill_count",
    "villCount",
    "vills",
    "worker",
    "workers",
    "worker_count",
    "workerCount",
    "resources",
    "resource_allocation",
    "villagers_on",
];

fn bo_is_step_like_object(object: &serde_json::Map<String, Value>) -> bool {
    BO_STEP_LIKE_KEYS
        .iter()
        .any(|key| object.contains_key(*key))
}

fn bo_step_collection_len(value: &Value) -> Option<usize> {
    if let Some(array) = value.as_array() {
        return Some(bo_step_array_len(array));
    }
    if let Some(text) = value.as_str() {
        let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
        return (lines > 0).then_some(lines);
    }
    let object = value.as_object()?;
    for key in BO_STEP_COLLECTION_KEYS {
        if let Some(steps) = object.get(*key).and_then(bo_step_collection_len) {
            return Some(steps);
        }
    }
    if bo_is_step_like_object(object) {
        return Some(usize::from(bo_raw_step_value_has_content(value)));
    }
    (!object.is_empty() && object.keys().all(|key| bo_step_key_order(key).is_some()))
        .then_some(object.len())
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
    let mut failed: Vec<String> = Vec::new();
    for (key, action) in s.hotkeys() {
        let result = gs.on_shortcut(key.as_str(), move |app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                run_action(app, action);
            }
        });
        if let Err(e) = result {
            log::error!("failed to register hotkey '{key}': {e}");
            failed.push(key);
        }
    }
    // Surface failures in the control panel — a silently dead hotkey is
    // indistinguishable from a working one until mid-game.
    if !failed.is_empty() {
        let _ = app.emit("hotkey_error", failed);
    }
}

// ---------------- poller ----------------

async fn poller(app: AppHandle) {
    let mut consecutive_errors: u32 = 0;
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
                    consecutive_errors = 0;
                    let _ = app.emit("poll_ok", game["ongoing"].as_bool().unwrap_or(false));
                }
                Err(e) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    log::warn!("poll failed ({consecutive_errors} in a row): {e}");
                    let _ = app.emit("poll_error", e);
                }
            }
        }

        // Back off while the API keeps failing (offline, rate-limited) so we
        // don't hammer it; a manual refresh still breaks the wait instantly.
        let wait = interval * 2u64.saturating_pow(consecutive_errors.min(3)).min(8);
        let state = app.state::<AppState>();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(wait.min(300)),
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
            .user_agent(concat!("AoE4-Overlay-Rust/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
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
                let bo =
                    MenuItemBuilder::with_id("buildorder", "Show / hide build order").build(app)?;
                let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
                let menu = MenuBuilder::new(app)
                    .items(&[&open, &ovl, &bo, &quit])
                    .build()?;
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
                        "buildorder" => toggle_window(app, "buildorder"),
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

            for (label, geo) in [
                ("overlay", s.overlay_geometry),
                ("buildorder", s.bo_geometry),
            ] {
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
                    msgs.push(serde_json::json!({"type": "player_data", "data": d}).to_string());
                }
                msgs
            }));

            tauri::async_runtime::spawn(poller(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_player,
            get_match_history,
            get_player_profile,
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
            bo_import_url,
            bo_search_guides,
            check_update,
            get_app_version
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bo_step_count_accepts_common_json_shapes() {
        assert_eq!(bo_step_count(r#"{"build_order":[{},{}]}"#), 2);
        assert_eq!(bo_step_count(r#"[{}, "note", {}]"#), 3);
        assert_eq!(bo_step_count("```json\n[{},{},{}]\n```"), 3);
        assert_eq!(bo_step_count("```json\n{\"build_order\":[{},{}]}\n```"), 2);
        assert_eq!(bo_step_count(r#"{"buildOrder":[{}, {}, {}]}"#), 3);
        assert_eq!(bo_step_count(r#"{"steps":["a","b","c","d"]}"#), 4);
        assert_eq!(bo_step_count(r#"{"build order":[{},{}]}"#), 2);
        assert_eq!(bo_step_count(r#"{"build-order":[{}, {}, {}]}"#), 3);
        assert_eq!(bo_step_count(r#"{"buildorder":{"1":"a","2":"b"}}"#), 2);
        assert_eq!(bo_step_count(r#"{"buildOrderSteps":[{},{}]}"#), 2);
        assert_eq!(bo_step_count(r#"{"build_order":{"steps":[{},{}]}}"#), 2);
        assert_eq!(
            bo_step_count(r#"{"build_order":{"2":{},"1":{},"10":{}}}"#),
            3
        );
        assert_eq!(
            bo_step_count(r#"{"buildOrder":{"step_1":{},"step_2":{}}}"#),
            2
        );
        assert_eq!(bo_step_count(r#"{"steps":{"steps":{"1":"a","2":"b"}}}"#), 2);
        assert_eq!(
            bo_step_count(r#"{"build_order":{"time":"0:00","notes":["Queue villagers"]}}"#),
            1
        );
        assert_eq!(
            bo_step_count(r#"{"buildOrder":{"notes":["Queue villagers"]}}"#),
            1
        );
        assert_eq!(
            bo_step_count(r#"{"buildOrder":{"start_time":"1:05","notes":["Queue villagers"]}}"#),
            1
        );
        assert_eq!(
            bo_step_count(r#"{"buildOrder":{"worker_count":"7","supplyUsed":"8"}}"#),
            1
        );
        assert_eq!(
            bo_step_count(r#"{"build_order":{"villCount":"12","popCap":"20"}}"#),
            1
        );
        assert_eq!(
            bo_step_count("{\"build_order\":\"Queue villagers\\nScout\\n\\nBuild house\"}"),
            3
        );
        assert_eq!(bo_step_count("{\"steps\":\"Queue villagers\\nScout\"}"), 2);
    }

    #[test]
    fn bo_step_count_accepts_repairable_json_syntax() {
        let messy = r#"```jsonc
{
  // user pasted from a JS object
  buildOrder: [
    { time: '00:00', notes: ['Queue // not a comment' 'Scout'], active: True, meta: None, }
    { time: '01:10', notes: ['Scout'], },
  ],
}
```"#;
        assert_eq!(bo_step_count(messy), 2);
        assert_eq!(
            bo_step_count(r#"{"build_order":[{"notes":["http://example.test//x"]},]}"#),
            1
        );
        assert_eq!(
            bo_step_count(r#"{"name":"Missing comma" "build_order":[{"notes":["a" "b"]}]}"#),
            1
        );
        assert_eq!(
            bo_step_count(
                r#"{"name":"Semi"; "build_order":[{"notes":["a;b"]}; {"notes":["c"]};]};"#
            ),
            2
        );
        assert_eq!(
            bo_step_count("```jsonc\n{name:'x'; build_order:[{time:'0' notes:['a' 'b'];},];}\n```"),
            1
        );
        assert_eq!(
            bo_step_count("{name='x', build_order=[{time='0' notes=['a', 'b']}]}"),
            1
        );
        assert_eq!(
            bo_step_count("const build = {name:'x', build_order:[{notes:['a']}]};"),
            1
        );
        assert_eq!(
            bo_step_count("Here is the build:\n{name:'x', build_order:[{notes:['a']}]}\nThanks"),
            1
        );
        let apostrophe_values = parse_bo_json_lenient(
            "{civilization:'Jeanne d’Arc', build_order:[{notes:[\"King's Palace\", 'Zhu Xi’s Legacy']}]}",
        )
        .unwrap();
        assert_eq!(apostrophe_values["civilization"], "Jeanne d’Arc");
        assert_eq!(
            apostrophe_values["build_order"][0]["notes"][1],
            "Zhu Xi’s Legacy"
        );
        let curly_quoted =
            parse_bo_json_lenient("{build_order:[{notes:[‘Zhu Xi’s Legacy // not a comment’]}]}")
                .unwrap();
        assert_eq!(
            curly_quoted["build_order"][0]["notes"][0],
            "Zhu Xi’s Legacy // not a comment"
        );
        let unescaped_apostrophe =
            parse_bo_json_lenient(r#"{build_order:[{notes:['King's Palace']}]} "#).unwrap();
        assert_eq!(
            unescaped_apostrophe["build_order"][0]["notes"][0],
            "King's Palace"
        );
        let js_literals = parse_bo_json_lenient(
            r#"{description:undefined, build_order:[{time:undefined, villager_count:NaN, population_count:Infinity, notes:["undefined", "NaN"]}]}"#,
        )
        .unwrap();
        assert!(js_literals["description"].is_null());
        assert!(js_literals["build_order"][0]["time"].is_null());
        assert!(js_literals["build_order"][0]["villager_count"].is_null());
        assert!(js_literals["build_order"][0]["population_count"].is_null());
        assert_eq!(js_literals["build_order"][0]["notes"][0], "undefined");
        assert_eq!(js_literals["build_order"][0]["notes"][1], "NaN");
        let bare_text_values = parse_bo_json_lenient(
            "{name: Fast Castle, civilization: English, build_order:[{time: 0:00, note: Queue villagers}]}",
        )
        .unwrap();
        assert_eq!(bare_text_values["name"], "Fast Castle");
        assert_eq!(bare_text_values["civilization"], "English");
        assert_eq!(bare_text_values["build_order"][0]["time"], "0:00");
        assert_eq!(
            bare_text_values["build_order"][0]["note"],
            "Queue villagers"
        );
        let bare_array_values = parse_bo_json_lenient(
            "{civilization:[English, French], build_order:[Queue villagers, {notes:[Scout, Build house, 2]}]}",
        )
        .unwrap();
        assert_eq!(bare_array_values["civilization"][0], "English");
        assert_eq!(bare_array_values["civilization"][1], "French");
        assert_eq!(bare_array_values["build_order"][0], "Queue villagers");
        assert_eq!(bare_array_values["build_order"][1]["notes"][0], "Scout");
        assert_eq!(
            bare_array_values["build_order"][1]["notes"][1],
            "Build house"
        );
        assert_eq!(bare_array_values["build_order"][1]["notes"][2], 2);
    }

    #[test]
    fn bo_step_count_ignores_pipe_table_headers() {
        let table = r#"
| Time | Food | Wood | Gold | Stone | Action |
| --- | ---: | ---: | ---: | ---: | --- |
| 0:00 | 6 | 0 | 0 | 0 | Queue villagers |
| 1.05 | 5 | 2 | 0 | 0 | Build house |
| 2:24 | 8/3/0/0 | Scout and age |
"#;
        assert_eq!(bo_step_count(table), 3);
    }

    #[test]
    fn bo_step_count_ignores_timed_text_headings() {
        let text = r#"
Dark Age
0:00 - Queue villagers
1.05 (5/2/0/0) Build house
2:24 Scout and age
"#;
        assert_eq!(bo_step_count(text), 3);
    }

    #[test]
    fn bo_step_count_splits_semicolon_timed_text() {
        assert_eq!(
            bo_step_count("0:00 Queue villagers; 1.05 Build house; 2:24 Scout and age"),
            3
        );
    }

    #[test]
    fn bo_step_count_accepts_checklist_timed_text() {
        let text = r#"
- [ ] 0:00 Queue villagers
1. [x] 1.05 Build house
2) [ ] 2:24 Scout and age
"#;
        assert_eq!(bo_step_count(text), 3);
    }

    #[test]
    fn bo_step_count_accepts_plain_checklist_text() {
        let text = r#"
Dark Age
- Queue villagers
- Build house
Feudal Age
1. Build archery range
2. Add farms
"#;
        assert_eq!(bo_step_count(text), 4);
    }

    #[test]
    fn bo_step_count_accepts_mixed_aoeivbuilds_exports() {
        let text = r#"
BeastyQT House of Lancaster

* (6/0/0/0)		6 vills -> sheep
* (6/0/3/0)		Rally 3 vills -> gold, 1st -> house -> mining camp -> gold
* (7/0/3/0)	~1:30	Rally vills -> food
* (6/0/0/3)	~2:00	AGE UP w/ gold vills -> Landcaster Castle, 3 food vills -> stone

Notes
Continue upgrades.

Generated by AOE4 Builds
"#;
        assert_eq!(bo_step_count(text), 4);
    }

    #[test]
    fn bo_step_count_accepts_stopwatch_timed_text() {
        let text = r#"
90s Queue villagers
1m30s Build house
2 min 05 sec Scout and age
"#;
        assert_eq!(bo_step_count(text), 3);
    }

    #[test]
    fn bo_step_count_accepts_dash_resource_allocations() {
        let text = r#"
0:00 6-0-0-0 Queue villagers
| 1:05 | 5-2-0-0 | Build house |
"#;
        assert_eq!(bo_step_count(text), 2);
    }

    #[test]
    fn bo_step_count_accepts_tsv_tables() {
        let table = "Time\tAge\tVills\tPop\tFood\tWood\tGold\tStone\tAction\n\
0:00\tDark\t6\t6/15\t6\t0\t0\t0\tQueue villagers\n\
4:30\tFeudal\t18\t20/30\t8\t4\t3\t0\tAdd archery range";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_accepts_csv_tables() {
        let table = "Time,Age,Vills,Pop,Food,Wood,Gold,Stone,Action\n\
0:00,Dark,6,6/15,6,0,0,0,Queue villagers\n\
4:30,Feudal,18,20/30,8,4,3,0,\"Add archery range, then blacksmith\"";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_accepts_short_resource_headers() {
        let table = "Time\tAge\tVills\tPop\tF\tW\tG\tS\tB\tAction\n\
0:00\tDark\t6\t6/15\t6\t0\t0\t0\t1\tQueue villagers\n\
4:30\tFeudal\t18\t20/30\t8\t4\t3\t0\t2\tAdd archery range";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_accepts_loose_table_headers() {
        let table = "Time,Vill,Pop Cap,Res,Build\n\
0:00,6,6/15,6/0/0/0,Queue villagers\n\
4:30,18,20/30,8/4/3/0,Add archery range";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_accepts_count_and_resource_vill_headers() {
        let table =
            "Time,Vill Count,Pop Count,Food Vills,Wood Vills,Gold Vills,Stone Vills,Build\n\
0:00,6,6/15,6,0,0,0,Queue villagers\n\
4:30,18,20/30,8,4,3,0,Add archery range";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_accepts_instruction_and_allocation_headers() {
        let table = "Time,Resource Allocation,Instructions\n\
0:00,6/0/0/0,Queue villagers\n\
4:30,8/4/3/0,Add archery range";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_accepts_timing_and_villager_allocation_headers() {
        let table = "When,Villager Allocation,Tasks\n\
0:00,6/0/0/0,Queue villagers\n\
4:30,8/4/3/0,Add archery range";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_accepts_start_time_headers() {
        let table = "Start Time,Villager Allocation,Tasks\n\
0:00,6/0/0/0,Queue villagers\n\
4:30,8/4/3/0,Add archery range";
        assert_eq!(bo_step_count(table), 2);
    }

    #[test]
    fn bo_step_count_flattens_aoe4guides_sectioned_steps() {
        let sectioned = r#"{
  "steps": [
    {
      "type": "age",
      "age": 1,
      "gameplan": "Dark age plan",
      "steps": [
        { "time": "0:00", "description": "Queue" },
        { "time": "0:35", "description": "Scout" },
        { "time": "", "description": "", "food": "" }
      ]
    },
    {
      "type": "ageUp",
      "age": 1,
      "steps": [
        { "time": "2:45", "description": "Age up" }
      ]
    },
    { "time": "4:00", "description": "Direct top-level step" }
  ]
}"#;
        assert_eq!(bo_step_count(sectioned), 4);
    }

    #[test]
    fn bo_step_count_falls_back_to_text_lines() {
        assert_eq!(bo_step_count("a\n\nb\n"), 2);
        assert_eq!(bo_step_count(""), 1);
    }
}
