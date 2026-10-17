use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub click_through: String,
    pub reset: String,
    pub hide: String,
    pub marker: String,
    pub copy: String,
    pub report: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys {
            click_through: "Ctrl+Shift+L".into(),
            reset: "Ctrl+Shift+R".into(),
            hide: "Ctrl+Shift+H".into(),
            marker: "Ctrl+Shift+M".into(),
            copy: "Ctrl+Shift+C".into(),
            report: "Ctrl+Shift+P".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Share {
    /// Send finished fights / loot to a Daeva bot endpoint.
    pub enabled: bool,
    pub endpoint: String,
    pub token: String,
    /// "self" (only you), "party" (everyone in the fight), "none".
    pub scope: String,
    pub share_loot: bool,
    /// Only share loot at or above this grade.
    pub min_loot_grade: u32,
    /// Minimum fight length in seconds worth sharing.
    pub min_duration: f64,
}

impl Default for Share {
    fn default() -> Self {
        Share { enabled: false, endpoint: String::new(), token: String::new(), scope: "self".into(), share_loot: false, min_loot_grade: 4, min_duration: 20.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub opacity: f64,
    pub scale: f64,
    pub accent: String,
    pub rows: usize,
    pub show_heal: bool,
    pub show_target: bool,
    /// "dps" | "dmg" | "share"
    pub primary_metric: String,
    /// Hide other players' names on the overlay (screenshots/streams).
    pub anonymize_others: bool,
    pub click_through: bool,
    pub always_on_top: bool,
    pub idle_timeout: f64,
    pub merge_summons: bool,
    pub end_on_target_death: bool,
    /// Executable names to look for (substring, case-insensitive).
    pub process_names: Vec<String>,
    /// Extra server ports to treat as game traffic.
    pub manual_ports: Vec<u16>,
    /// Skip these remote ports when auto-detecting (launcher HTTPS etc).
    pub ignore_ports: Vec<u16>,
    /// Restrict capture to adapters whose name/description contains this.
    pub device_filter: String,
    pub hotkeys: Hotkeys,
    pub share: Share,
    pub demo: bool,
    pub window: Option<(i32, i32, u32, u32)>,
    pub history_keep_days: u32,
    /// Last known local player (entity id, name, unix time). Entity ids only
    /// change on zone change/relog, which the game announces; this lets a
    /// restarted meter recognise you straight away.
    pub last_self: Option<(u64, Option<String>, f64)>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            opacity: 0.88,
            scale: 1.0,
            accent: "#d9a441".into(),
            rows: 8,
            show_heal: false,
            show_target: true,
            primary_metric: "dps".into(),
            anonymize_others: false,
            click_through: false,
            always_on_top: true,
            idle_timeout: 8.0,
            merge_summons: true,
            end_on_target_death: true,
            process_names: vec!["aion2".into()],
            manual_ports: vec![],
            ignore_ports: vec![80, 443],
            device_filter: String::new(),
            hotkeys: Hotkeys::default(),
            share: Share::default(),
            demo: false,
            window: None,
            history_keep_days: 30,
            last_self: None,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, path)
    }
    pub fn hotkey_bindings(&self) -> Vec<(String, String)> {
        let h = &self.hotkeys;
        vec![
            ("click_through".into(), h.click_through.clone()),
            ("reset".into(), h.reset.clone()),
            ("hide".into(), h.hide.clone()),
            ("marker".into(), h.marker.clone()),
            ("copy".into(), h.copy.clone()),
            ("report".into(), h.report.clone()),
        ]
    }
}
