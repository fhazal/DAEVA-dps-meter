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
    /// Switch between the Classic and Solo layouts.
    pub layout: String,
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
            layout: "Ctrl+Shift+O".into(),
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

/// The Daeva bot's upload address on aion2.bajubesi.org.
pub const DEFAULT_SHARE_ENDPOINT: &str = "https://aion2.bajubesi.org/api/meter";

impl Default for Share {
    fn default() -> Self {
        Share { enabled: false, endpoint: DEFAULT_SHARE_ENDPOINT.into(), token: String::new(), scope: "self".into(), share_loot: false, min_loot_grade: 4, min_duration: 20.0 }
    }
}

/// What the Solo layout shows besides your DPS.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Solo {
    /// Your place in the party ("#2 of 5") and share of the damage.
    pub rank: bool,
    /// Crit, double, perfect, front and back chips.
    pub chips: bool,
    /// Odyle, party DPS and ping at the bottom.
    pub footer: bool,
}

impl Default for Solo {
    fn default() -> Self {
        Solo { rank: true, chips: true, footer: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Alerts {
    /// Show in-game event alerts from the Daeva schedule feed.
    pub enabled: bool,
    /// Daeva web `/api/overview` URL (same data as the Discord bot).
    pub feed_url: String,
    /// Minutes before an event to warn (0 = only when it starts).
    pub lead_minutes: f64,
    pub at_start: bool,
    pub sound: bool,
    /// Chime volume, 0-100.
    pub volume: f64,
    /// Show the next event under the meter.
    pub show_next: bool,
    /// Event ids that never raise an alert.
    pub muted: Vec<String>,
    /// How long a banner stays up, seconds.
    pub banner_secs: f64,
    /// Where banners appear: "above_meter", "below_meter", "screen_top_center",
    /// "screen_top_right" (left of the minimap), "screen_top_left", or
    /// "custom" (dragged into place, see `custom`).
    pub position: String,
    /// Nudge, in screen pixels at 100% scaling (fixed spots only).
    pub offset_x: f64,
    pub offset_y: f64,
    /// position "custom": where the user dragged the banner, as a share of
    /// the monitor (0..1 across, 0..1 down) plus that monitor's name, so it
    /// stays put at any resolution.
    pub custom: Option<(f64, f64)>,
    pub custom_monitor: String,
    /// "normal" or "compact" (one line).
    pub size: String,
    /// Keep banners back while you're in a fight; show them when it ends.
    pub hold_in_fight: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts {
            enabled: true,
            feed_url: "https://aion2.bajubesi.org/api/overview?region=asia".into(),
            lead_minutes: 5.0,
            at_start: true,
            sound: true,
            volume: 70.0,
            show_next: true,
            muted: vec!["shugo_festival".into(), "dimensional_invasion".into()],
            banner_secs: 12.0,
            position: "above_meter".into(),
            offset_x: 0.0,
            offset_y: 0.0,
            custom: None,
            custom_monitor: String::new(),
            size: "normal".into(),
            hold_in_fight: false,
        }
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
    /// Whose rows the overlay lists: "auto" (your party while you're in one,
    /// everyone otherwise), "all" (everyone fighting your targets), "party"
    /// (you and your group, from the game's party list) or "me".
    pub show_players: String,
    /// Settings file revision, for one-time upgrades (0 = before 0.5.0).
    #[serde(default)]
    pub settings_rev: u32,
    /// "dps" | "dmg" | "share"
    pub primary_metric: String,
    /// Hide other players' names on the overlay (screenshots/streams).
    pub anonymize_others: bool,
    pub click_through: bool,
    pub always_on_top: bool,
    /// Overlay layout: "classic" (the party table) or "solo" (just you).
    pub layout: String,
    pub solo: Solo,
    /// Window size in the Solo layout (Classic keeps `window`).
    pub solo_size: Option<(u32, u32)>,
    pub idle_timeout: f64,
    /// What DPS is divided by: "boss" (first hit on the boss → kill),
    /// "personal" (each player's first → last hit) or "fight" (whole fight).
    pub dps_clock: String,
    /// Extra overlay columns, in order (at most 3): crit, back, perfect,
    /// hits, max, hps, taken, deaths.
    pub columns: Vec<String>,
    /// Show "New personal best" under the meter after a boss kill.
    pub personal_bests: bool,
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
    pub alerts: Alerts,
    /// Fetch names of unknown monsters/skills from aion2.gaming.tools.
    pub lookup_names: bool,
    /// Server name shown on the meter; empty = detect from the game.
    pub server_name: String,
    /// Daeva server mirror of the gaming.tools name files (tried first).
    pub names_mirror: String,
    /// Show class emblems, skill icons and monster portraits from
    /// aion2.gaming.tools (falls back to the built-in icons).
    pub game_art: bool,
    pub demo: bool,
    /// Show the research-mode (flask) button in the header. Off for normal
    /// users; testers turn it on to record captures after a game patch.
    pub show_research: bool,
    /// Ask GitHub for the latest published release (start-up, then every 6 h).
    pub check_updates: bool,
    /// A version the user hid the update notice for ("v0.3.1").
    pub skipped_update: String,
    pub window: Option<(i32, i32, u32, u32)>,
    pub history_keep_days: u32,
    /// Last known local player (entity id, name, unix time). Entity ids only
    /// change on zone change/relog, which the game announces; this lets a
    /// restarted meter recognise you straight away.
    pub last_self: Option<(u64, Option<String>, f64)>,
    /// Last detected game server number, so the name shows straight away.
    pub last_server: Option<u32>,
    /// Last Shugo Festival Reward Key count the game sent (it is only sent
    /// at login and when it changes, so it is remembered between sessions).
    pub last_keys: Option<u64>,
    /// Last Odyle (base, bonus) the game sent, for display after a restart.
    pub last_odyle: Option<(u64, u64)>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            opacity: 0.88,
            scale: 0.9,
            accent: "#d9a441".into(),
            rows: 8,
            show_heal: false,
            show_target: true,
            show_players: "auto".into(),
            settings_rev: 1,
            primary_metric: "dps".into(),
            anonymize_others: false,
            click_through: false,
            always_on_top: true,
            layout: "classic".into(),
            solo: Solo::default(),
            solo_size: None,
            idle_timeout: 8.0,
            dps_clock: "boss".into(),
            columns: vec![],
            personal_bests: true,
            merge_summons: true,
            end_on_target_death: true,
            process_names: vec!["aion2".into()],
            manual_ports: vec![],
            ignore_ports: vec![80, 443],
            device_filter: String::new(),
            hotkeys: Hotkeys::default(),
            share: Share::default(),
            alerts: Alerts::default(),
            lookup_names: true,
            server_name: String::new(),
            names_mirror: "https://aion2.bajubesi.org/api/gt".into(),
            game_art: true,
            demo: false,
            show_research: false,
            check_updates: true,
            skipped_update: String::new(),
            window: None,
            history_keep_days: 30,
            last_self: None,
            last_server: None,
            last_keys: None,
            last_odyle: None,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        let mut s: Settings = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        // Older versions saved an empty address; fill in the Daeva bot's.
        if s.share.endpoint.trim().is_empty() {
            s.share.endpoint = DEFAULT_SHARE_ENDPOINT.into();
        }
        // 0.5.0: "Everyone" was the default; Auto replaces it (your party
        // while you're in one). Choices made after this are kept.
        if s.settings_rev < 1 {
            if s.show_players == "all" {
                s.show_players = "auto".into();
            }
            s.settings_rev = 1;
        }
        s
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
            ("layout".into(), h.layout.clone()),
        ]
    }
}
