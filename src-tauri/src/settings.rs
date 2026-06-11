use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profile_id: Option<u64>,
    pub player_name: Option<String>,
    /// API polling interval in seconds
    pub interval: u64,
    /// Global hotkey to show/hide the overlay, e.g. "Alt+O"
    pub overlay_hotkey: String,
    /// Saved overlay geometry: [x, y, w, h]
    pub overlay_geometry: Option<[i32; 4]>,
    pub show_overlay_on_new_game: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profile_id: None,
            player_name: None,
            interval: 15,
            overlay_hotkey: "Alt+O".into(),
            overlay_geometry: None,
            show_overlay_on_new_game: true,
        }
    }
}

impl Settings {
    pub fn path(config_dir: &PathBuf) -> PathBuf {
        config_dir.join("config.json")
    }

    pub fn load(config_dir: &PathBuf) -> Self {
        std::fs::read_to_string(Self::path(config_dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, config_dir: &PathBuf) {
        let _ = std::fs::create_dir_all(config_dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Err(e) = std::fs::write(Self::path(config_dir), json) {
                log::error!("failed to save settings: {e}");
            }
        }
    }
}
