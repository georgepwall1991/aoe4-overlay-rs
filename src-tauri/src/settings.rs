use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    pub max_games_history: u32,

    // Appearance
    /// RGBA team colors (0-255 rgb, 0-1 alpha), team 1 first
    pub team_colors: Vec<[f64; 4]>,
    pub civ_stats_color: String,
    /// Overlay font scale percent (100 = default)
    pub font_scale: u32,
    /// Overlay window size percent (100 = default); content scales with the window
    pub overlay_scale: u32,

    // OBS websocket streaming
    pub websocket_port: u16,

    // Build order overlay
    pub bo_hotkey_show: String,
    pub bo_hotkey_cycle: String,
    pub bo_hotkey_prev_step: String,
    pub bo_hotkey_next_step: String,
    pub bo_show_title: bool,
    /// show a dimmed preview of the upcoming step under the notes
    pub bo_show_next: bool,
    pub bo_font_size: u32,
    pub bo_text_color: [u8; 3],
    pub bo_title_color: [u8; 3],
    pub bo_color_background: [u8; 3],
    pub bo_opacity: f64,
    pub bo_geometry: Option<[i32; 4]>,
    /// name -> raw TXT or RTS_Overlay JSON content
    pub buildorders: BTreeMap<String, String>,
    /// build order names excluded from hotkey cycling
    pub unchecked_buildorders: Vec<String>,
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
            max_games_history: 50,
            team_colors: vec![
                [74.0, 255.0, 2.0, 0.35],
                [3.0, 179.0, 255.0, 0.35],
                [255.0, 0.0, 0.0, 0.35],
            ],
            civ_stats_color: "#BC8AEA".into(),
            font_scale: 100,
            overlay_scale: 100,
            websocket_port: 7307,
            bo_hotkey_show: String::new(),
            bo_hotkey_cycle: String::new(),
            bo_hotkey_prev_step: String::new(),
            bo_hotkey_next_step: String::new(),
            bo_show_title: true,
            bo_show_next: true,
            bo_font_size: 12,
            bo_text_color: [255, 255, 255],
            bo_title_color: [255, 255, 255],
            bo_color_background: [30, 30, 30],
            bo_opacity: 0.75,
            bo_geometry: None,
            buildorders: BTreeMap::from([("Example: villagers".into(), DEFAULT_BO.trim().into())]),
            unchecked_buildorders: Vec::new(),
        }
    }
}

const DEFAULT_BO: &str = r#"
{
  "civilization": "English",
  "name": "Example: villagers",
  "build_order": [
    {
      "population_count": 7,
      "villager_count": 6,
      "age": 1,
      "resources": { "food": 6, "wood": 0, "gold": 0, "stone": 0 },
      "notes": ["Send @unit_worker/villager@ to sheep", "Build a house with first villager"]
    },
    {
      "population_count": 12,
      "villager_count": 11,
      "age": 1,
      "resources": { "food": 8, "wood": 3, "gold": 0, "stone": 0 },
      "notes": ["3 villagers to wood", "Take map control with scout"],
      "time": "3:00"
    }
  ]
}
"#;

impl Settings {
    pub fn path(config_dir: &std::path::Path) -> PathBuf {
        config_dir.join("config.json")
    }

    pub fn load(config_dir: &std::path::Path) -> Self {
        std::fs::read_to_string(Self::path(config_dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, config_dir: &std::path::Path) {
        let _ = std::fs::create_dir_all(config_dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Err(e) = std::fs::write(Self::path(config_dir), json) {
                log::error!("failed to save settings: {e}");
            }
        }
    }

    pub fn hotkeys(&self) -> Vec<(String, crate::HotkeyAction)> {
        use crate::HotkeyAction::*;
        [
            (&self.overlay_hotkey, ToggleOverlay),
            (&self.bo_hotkey_show, BoToggle),
            (&self.bo_hotkey_cycle, BoCycle),
            (&self.bo_hotkey_prev_step, BoPrevStep),
            (&self.bo_hotkey_next_step, BoNextStep),
        ]
        .into_iter()
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, a)| (k.clone(), a))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.interval, 15);
        assert_eq!(s.websocket_port, 7307);
        assert_eq!(s.overlay_hotkey, "Alt+O");
        assert_eq!(s.buildorders.len(), 1, "ships one example build order");
        let bo = s.buildorders.values().next().unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(bo).is_ok(),
            "example BO is valid JSON"
        );
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("aoe4ov-test-{}", std::process::id()));
        let mut s = Settings::default();
        s.profile_id = Some(42);
        s.player_name = Some("tester".into());
        s.team_colors = vec![[1.0, 2.0, 3.0, 0.5]];
        s.buildorders.insert("custom".into(), "step one".into());
        s.save(&dir);
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.profile_id, Some(42));
        assert_eq!(loaded.player_name.as_deref(), Some("tester"));
        assert_eq!(loaded.team_colors, vec![[1.0, 2.0, 3.0, 0.5]]);
        assert_eq!(
            loaded.buildorders.get("custom").map(String::as_str),
            Some("step one")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_or_corrupt_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("aoe4ov-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&dir).interval, 15);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Settings::path(&dir), "{not json!").unwrap();
        assert_eq!(Settings::load(&dir).interval, 15);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_config_fills_defaults() {
        let dir = std::env::temp_dir().join(format!("aoe4ov-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Settings::path(&dir), r#"{"profile_id": 7, "interval": 30}"#).unwrap();
        let s = Settings::load(&dir);
        assert_eq!(s.profile_id, Some(7));
        assert_eq!(s.interval, 30);
        assert_eq!(s.websocket_port, 7307, "unspecified fields use defaults");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hotkeys_skips_empty_bindings() {
        let mut s = Settings::default();
        s.bo_hotkey_show = "Alt+B".into();
        let keys = s.hotkeys();
        assert_eq!(keys.len(), 2, "overlay + bo show only");
        assert!(keys
            .iter()
            .any(|(k, a)| k == "Alt+O" && *a == crate::HotkeyAction::ToggleOverlay));
        assert!(keys
            .iter()
            .any(|(k, a)| k == "Alt+B" && *a == crate::HotkeyAction::BoToggle));
    }
}
