//! Shared application state and the capture / processing threads.

use crate::npcap::{Capture, Npcap};
use crate::settings::Settings;
use crate::winsys;
use meter_core::engine::{Engine, EngineConfig, Fight, Names};
use meter_core::json::{self, Obj, Value};
use meter_core::net::{self, Endpoint};
use meter_core::pcapfile::PcapWriter;
use meter_core::protocol::{Decoder, Decompressor, Definitions};
use meter_core::schedule::{self, Alert, Notifier, Occurrence};
use meter_core::sim::{Simulator, DEMO_NAMES};
use meter_core::Pipeline;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now_unix() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub struct Paths {
    pub data: PathBuf,
    pub resources: PathBuf,
    pub settings: PathBuf,
    pub history: PathBuf,
    pub research: PathBuf,
}

#[derive(Default)]
pub struct Status {
    pub npcap_version: Option<String>,
    pub npcap_error: Option<String>,
    pub devices: Vec<String>,
    /// Adapters Npcap listed that we don't capture on, with the reason.
    pub devices_skipped: Vec<String>,
    pub device_errors: Vec<String>,
    pub game_process: Option<(u32, String)>,
    pub servers: Vec<String>,
    pub dropped: u64,
    pub defs_name: String,
    pub defs_error: Option<String>,
    pub names_error: Option<String>,
    pub hotkey_errors: Vec<String>,
    pub last_save_error: Option<String>,
}

pub struct Recorder {
    writer: PcapWriter<BufWriter<File>>,
    pub path: PathBuf,
    pub markers: Vec<(f64, String)>,
    pub started: f64,
    pub packets: u64,
    pub bytes: u64,
    pub label: String,
}

impl Recorder {
    fn markers_path(&self) -> PathBuf {
        self.path.with_extension("markers.json")
    }
    fn save_markers(&self) {
        let arr: Vec<Value> = self.markers.iter().map(|(t, l)| Obj::new().set("t", *t).set("label", l.as_str()).build()).collect();
        let v = Obj::new().set("label", self.label.as_str()).set("started", self.started).set("markers", Value::Arr(arr)).build();
        let _ = std::fs::write(self.markers_path(), v.to_string_pretty());
    }
}

pub struct Core {
    pub pipeline: Pipeline,
    pub sim: Option<Simulator>,
    pub settings: Settings,
    pub paths: Paths,
    pub status: Status,
    pub recorder: Option<Recorder>,
    pub uploads: Vec<String>,
    pub loot_uploaded: u64,
    pub settings_dirty: bool,
    pub window_params: HashMap<String, String>,
    last_settings_save: Instant,
    /// Last few minutes of game traffic, kept in memory for problem reports.
    rolling: std::collections::VecDeque<(f64, Vec<u8>)>,
    rolling_bytes: usize,
    /// For reports: what the meter noticed (fights, card, party, zone), and
    /// once a second what it showed, so a report explains itself.
    pub diary: Diary,
    health_at: f64,
    health_decoded: u64,
    health_frames: u64,
    /// Frames other than keep-alive pings, at the last health check.
    health_content: u64,
    /// Resyncs at the last health check.
    health_resyncs: u64,
    pub warning: Option<String>,
    /// When the game's server connection was first seen (0 = none).
    connected_at: f64,
    /// Connected for 20 s without one game packet (see check_health).
    no_traffic: bool,
    pub events: EventFeed,
    /// Monsters and skills seen without a name, waiting for you to name them.
    pub wanted: Wanted,
    /// gaming.tools lookups: key ("n123"/"s456") -> unix time it may be tried again.
    pub lookup_retry: HashMap<String, f64>,
    /// Keys gaming.tools has no page for.
    pub lookup_missing: HashSet<String>,
    pub lookup_status: Option<String>,
    /// gaming.tools bulk data loaded: (npc names, skill names, fetched at).
    pub site_bulk: Option<(usize, usize, f64)>,
    /// Monsters and skills with an official name (names.json or
    /// gaming.tools): your own name for them is kept but not shown.
    pub official: Official,
    /// Picture paths from gaming.tools: "n<template>" / "s<skill>" -> path.
    pub icons: HashMap<String, String>,
    /// Set by "Update names now".
    pub site_wake: bool,
    /// The monster data is downloading and no copy is loaded yet (first run).
    pub site_loading: bool,
    /// Which edge of the alert window the banners stack from ("top"/"bottom").
    pub toast_anchor: &'static str,
    /// Latest published release on GitHub: (tag like "v0.3.1", page url).
    pub update_latest: Option<(String, String)>,
    pub update_checked: f64,
    pub update_error: Option<String>,
    /// Set by "Check now".
    pub update_wake: bool,
    /// Best own DPS per character and boss, from saved fights.
    pub bests: Bests,
    /// "New personal best" shown under the meter after a kill.
    pub pb_notice: Option<Value>,
    /// When Daeva Meter was opened again while running (for the notice).
    pub opened_again: Option<f64>,
    /// The meter updating itself at start-up (see selfupdate.rs).
    pub self_update: Option<crate::selfupdate::SelfUpdate>,
    /// The installed version (tauri.conf, as the updater sees it). Test
    /// builds set it without touching Cargo.toml.
    pub app_version: String,
    /// Game servers to time for the ping, per server number (from the Daeva
    /// feed; the built-in Asia entry otherwise), and the last reading:
    /// milliseconds, when, and where ("Tokyo"). `ping_note` says why there
    /// is none: "region" (no server known for yours) or "unreachable".
    pub probes: Vec<Probe>,
    pub game_rtt: Option<(f64, f64, String)>,
    pub ping_note: Option<&'static str>,
    /// Banner rectangles in the alert window (CSS px: x, y, w, h), so the
    /// window only catches the mouse over a banner, and only while unlocked.
    pub toast_rects: Vec<(f64, f64, f64, f64)>,
    /// A banner is being dragged: (started, last window position, when it
    /// last moved).
    pub alert_drag: Option<(f64, Option<(i32, i32)>, f64)>,
    pub toast_health: ToastHealth,
    /// Banners held back during a fight (Settings → During fights).
    held: Vec<meter_core::schedule::Alert>,
    /// Capture-gap counter (dropped packets + decoder resyncs) at the start
    /// of the current fight: (fight id, counter).
    gap_base: Option<(u64, u64)>,
}

/// Your best DPS per (character, boss, DPS clock): (dps, started_unix, fights).
#[derive(Default)]
pub struct Bests {
    map: HashMap<(String, u64, String), (f64, f64, u32)>,
}

/// Fights shorter than this on the boss don't count towards personal bests.
const PB_MIN_SECS: f64 = 30.0;

impl Bests {
    /// Key for a History row, or None when it can't be a personal best
    /// (no kill, not a boss, too short, you weren't in it).
    fn key(row: &Value) -> Option<(String, u64, String)> {
        let name = row.str_or("self_name", "");
        let clock = row.str_or("clock", "");
        let npc = row.get("clock_npc").and_then(|x| x.as_u64())?;
        let ok = !name.is_empty() && clock == "boss" && row.bool_or("boss_killed", false) && row.f64_or("clock_secs", 0.0) >= PB_MIN_SECS && row.f64_or("mine", 0.0) > 0.0;
        ok.then(|| (name.to_string(), npc, clock.to_string()))
    }
    /// Adds a fight: Some(Some(prev)) when it beat the best, Some(None) when
    /// it's the first recorded kill on that boss, None otherwise.
    pub fn add(&mut self, row: &Value) -> Option<Option<f64>> {
        let k = Self::key(row)?;
        let (dps, at) = (row.f64_or("mine", 0.0), row.f64_or("started_unix", 0.0));
        match self.map.get_mut(&k) {
            Some(b) => {
                b.2 += 1;
                if dps > b.0 {
                    let prev = b.0;
                    b.0 = dps;
                    b.1 = at;
                    Some(Some(prev))
                } else {
                    None
                }
            }
            None => {
                self.map.insert(k, (dps, at, 1));
                Some(None)
            }
        }
    }
    pub fn best_for(&self, row: &Value) -> Option<(f64, f64, u32)> {
        self.map.get(&Self::key(row)?).copied()
    }
    /// True when this row is the record holder.
    pub fn is_best(&self, row: &Value) -> bool {
        self.best_for(row).map_or(false, |b| (b.1 - row.f64_or("started_unix", -1.0)).abs() < 0.5)
    }
    /// Marks the record holders in a list of History rows.
    pub fn mark(&self, list: &mut Value) {
        if let Value::Arr(items) = list {
            for it in items.iter_mut() {
                let pb = self.is_best(it);
                if let Value::Obj(m) = it {
                    m.insert("pb".into(), Value::from(pb));
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Wanted {
    /// template -> (max hp, zone name, first seen, last seen)
    pub npcs: std::collections::BTreeMap<u32, (Option<u64>, Option<String>, f64, f64)>,
    /// skill key -> (class, last seen)
    pub skills: std::collections::BTreeMap<u32, (Option<String>, f64)>,
    dirty: bool,
    scanned_at: f64,
}

/// In-game event alerts, fed by the Daeva schedule feed.
pub struct EventFeed {
    pub occurrences: Vec<Occurrence>,
    pub notifier: Notifier,
    /// Alerts raised recently, newest last; the overlay shows unseen ones.
    pub recent: std::collections::VecDeque<Alert>,
    /// Keys of alerts the alert window has put on screen. Until then an
    /// alert waits (a stalled window shows it late rather than never).
    pub acked: HashSet<String>,
    pub fetched_at: f64,
    pub error: Option<String>,
    /// Bumped to make the fetch thread reload now (settings changed).
    pub wake: u64,
}

impl EventFeed {
    fn new(a: &crate::settings::Alerts) -> EventFeed {
        EventFeed {
            occurrences: Vec::new(),
            notifier: Notifier::new(a.lead_minutes.max(0.0) * 60.0, a.at_start, a.muted.iter().cloned()),
            recent: Default::default(),
            acked: HashSet::new(),
            fetched_at: 0.0,
            error: None,
            wake: 0,
        }
    }
    fn configure(&mut self, a: &crate::settings::Alerts) {
        self.notifier.lead_secs = a.lead_minutes.clamp(0.0, 120.0) * 60.0;
        self.notifier.at_start = a.at_start;
        self.notifier.muted = a.muted.iter().cloned().collect();
    }
    pub fn push(&mut self, a: Alert) {
        self.recent.push_back(a);
        while self.recent.len() > 20 {
            self.recent.pop_front();
        }
        let recent = &self.recent;
        self.acked.retain(|k| recent.iter().any(|a| &a.key == k));
    }
    /// Should the alert window (still) get this alert? Shown ones stay in
    /// the list a little while (for reports); unshown ones wait up to 10 min,
    /// a "starts in" one only until its event starts.
    pub fn listed(&self, a: &Alert, now: f64, banner_secs: f64) -> bool {
        if self.acked.contains(&a.key) {
            now - a.raised < banner_secs.max(3.0) + 5.0
        } else {
            self.pending(a, now)
        }
    }
    fn pending(&self, a: &Alert, now: f64) -> bool {
        !self.acked.contains(&a.key) && now - a.raised < 600.0 && (a.kind == "start" || now < a.starts)
    }
    /// An alert is waiting for the alert window to show it.
    pub fn any_pending(&self, now: f64) -> bool {
        self.recent.iter().any(|a| self.pending(a, now))
    }
}

/// How the alert window is doing: when it last asked for alerts, and how
/// often the meter had to wake it (nudge) or reload it.
#[derive(Default)]
pub struct ToastHealth {
    pub seen: f64,
    pub nudges: u32,
    pub reloads: u32,
    pub last_fix: Option<(f64, String)>,
}

const ROLLING_SECS: f64 = 300.0;
const ROLLING_MAX_BYTES: usize = 48 << 20;

/// The meter's own notes for problem reports: events it noticed (automatic
/// markers) and, once a second, what the overlay showed.
#[derive(Default)]
pub struct Diary {
    pub events: std::collections::VecDeque<(f64, String)>,
    pub screen: std::collections::VecDeque<(f64, Value)>,
    last_sample: f64,
    party: String,
    fight: Option<u64>,
    card: String,
    self_id: Option<u64>,
    self_name: Option<String>,
    layout: String,
    /// "Report saved" for the overlay: when, and the file name.
    pub saved: Option<(f64, String)>,
}

impl Diary {
    pub fn note(&mut self, ts: f64, what: impl Into<String>) {
        self.events.push_back((ts, what.into()));
        while self.events.front().map_or(false, |(t, _)| ts - t > ROLLING_SECS * 2.0) || self.events.len() > 2000 {
            self.events.pop_front();
        }
    }
}

pub struct Shared {
    pub core: Mutex<Core>,
    pub capture_gen: AtomicU64,
    pub hotkeys: Mutex<Option<winsys::HotkeyThread>>,
}

type Pkt = (u32, f64, Vec<u8>);

/// The game's keep-alive ping, sent every ~0.2 s even when nothing happens.
const KEEPALIVE_OPCODE: u64 = 0x3600;
const NO_TRAFFIC: &str = "Can't see the game's traffic. Using a VPN or ping booster? Exclude AION2.exe from it (split tunneling). Click to send a report.";

fn decompressor() -> Decompressor {
    Arc::new(|method: &str, data: &[u8]| -> Option<Vec<u8>> {
        match method {
            "zlib" => miniz_oxide::inflate::decompress_to_vec_zlib(data).ok(),
            "deflate" => miniz_oxide::inflate::decompress_to_vec(data).ok(),
            "lz4" => lz4_flex::block::decompress_size_prepended(data).ok(),
            _ => None,
        }
    })
}

const FALLBACK_DEFS: &str = r#"{"name": "none", "framing": {"header": ["length:u16le", "opcode:u16le"]}, "messages": []}"#;

fn engine_cfg(s: &Settings) -> EngineConfig {
    EngineConfig {
        idle_timeout: s.idle_timeout.clamp(2.0, 120.0),
        merge_summons: s.merge_summons,
        end_on_target_death: s.end_on_target_death,
        dps_clock: meter_core::engine::DpsClock::parse(&s.dps_clock),
        ..Default::default()
    }
}

impl Core {
    pub fn new(data: PathBuf, resources: PathBuf) -> Core {
        let paths = Paths {
            settings: data.join("settings.json"),
            history: data.join("history"),
            research: data.join("research"),
            data,
            resources,
        };
        let _ = std::fs::create_dir_all(&paths.history);
        let _ = std::fs::create_dir_all(paths.data.join("definitions"));
        let settings = Settings::load(&paths.settings);
        let mut status = Status::default();
        let (defs, names, official) = load_definitions(&paths, &mut status);
        let mut pipeline = Pipeline::new(Decoder::new(defs, Some(decompressor())), Engine::new(engine_cfg(&settings), names));
        pipeline.server_ports.extend(settings.manual_ports.iter().copied());
        if !settings.demo {
            if let Some((id, name, at)) = settings.last_self.clone() {
                if now_unix() - at < 12.0 * 3600.0 {
                    pipeline.engine.restore_self(id, name);
                }
            }
        }
        let sim = settings.demo.then(|| Simulator::new(now_unix() as u64));
        if settings.demo {
            pipeline.engine.names = Names::from_json(DEMO_NAMES).unwrap_or_default();
        }
        let mut c = Core {
            pipeline,
            sim,
            settings,
            paths,
            status,
            recorder: None,
            uploads: Vec::new(),
            loot_uploaded: 0,
            settings_dirty: false,
            window_params: HashMap::new(),
            last_settings_save: Instant::now(),
            rolling: Default::default(),
            rolling_bytes: 0,
            diary: Diary::default(),
            health_at: 0.0,
            health_decoded: 0,
            health_frames: 0,
            health_content: 0,
            health_resyncs: 0,
            warning: None,
            connected_at: 0.0,
            no_traffic: false,
            events: EventFeed::new(&Default::default()),
            wanted: Wanted::default(),
            lookup_retry: HashMap::new(),
            lookup_missing: HashSet::new(),
            lookup_status: None,
            site_loading: false,
            site_bulk: None,
            official,
            icons: HashMap::new(),
            site_wake: false,
            toast_anchor: "bottom",
            update_latest: None,
            update_checked: 0.0,
            bests: Bests::default(),
            pb_notice: None,
            opened_again: None,
            self_update: None,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            probes: Vec::new(),
            game_rtt: None,
            ping_note: None,
            gap_base: None,
            toast_rects: Vec::new(),
            alert_drag: None,
            toast_health: ToastHealth::default(),
            held: Vec::new(),
            update_error: None,
            update_wake: false,
        };
        c.load_wanted();
        c.load_site_missing();
        c.events = EventFeed::new(&c.settings.alerts);
        c.pipeline.decoder.keep_recent = 200;
        c.prune_history();
        c.load_bests();
        c
    }

    pub fn reload_definitions(&mut self) {
        let (defs, names, official) = load_definitions(&self.paths, &mut self.status);
        self.official = official;
        // A fresh decoder must not be fed the middle of a stream as if it
        // were a frame boundary: restart reassembly so every flow resyncs,
        // and restart the health counters (they count from zero again).
        self.pipeline.reasm.reset();
        self.pipeline.decoder = Decoder::new(defs, Some(decompressor()));
        self.health_frames = 0;
        self.health_content = 0;
        self.health_decoded = 0;
        self.health_resyncs = 0;
        self.pipeline.decoder.keep_recent = if self.recorder.is_some() { 400 } else { 200 };
        self.pipeline.server_ports = self.pipeline.decoder.defs.ports.iter().copied().chain(self.settings.manual_ports.iter().copied()).collect();
        if !self.settings.demo {
            self.pipeline.engine.names = names;
        }
    }

    pub fn apply_settings(&mut self, mut s: Settings) {
        // The dragged alert spot is set by Move alerts, not by the form.
        s.alerts.custom = self.settings.alerts.custom;
        s.alerts.custom_monitor = self.settings.alerts.custom_monitor.clone();
        // So are the window's spot and sizes (the form may hold old ones).
        s.window = self.settings.window;
        s.solo_size = self.settings.solo_size;
        // And the update bookkeeping (the meter's, or ✕ on the notice).
        s.skipped_update = self.settings.skipped_update.clone();
        s.updating_to = self.settings.updating_to.clone();
        s.updated_from = self.settings.updated_from.clone();
        s.update_tries = self.settings.update_tries;
        s.auto_update_gave_up = self.settings.auto_update_gave_up.clone();
        let demo_changed = s.demo != self.settings.demo;
        self.pipeline.engine.cfg = engine_cfg(&s);
        if s.alerts.feed_url != self.settings.alerts.feed_url || s.alerts.enabled != self.settings.alerts.enabled {
            self.events.wake += 1;
        }
        self.events.configure(&s.alerts);
        self.settings = s;
        self.pipeline.server_ports = self.pipeline.decoder.defs.ports.iter().copied().chain(self.settings.manual_ports.iter().copied()).collect();
        if demo_changed {
            self.set_demo(self.settings.demo);
        }
        self.settings_dirty = true;
    }

    pub fn set_demo(&mut self, on: bool) {
        self.settings.demo = on;
        self.pipeline.engine.finish();
        self.pipeline.engine.clear_history();
        self.pipeline.engine.actors.clear();
        self.pipeline.engine.self_id = None;
        self.pipeline.engine.self_name = None;
        if on {
            self.sim = Some(Simulator::new(now_unix() as u64));
            self.pipeline.engine.names = Names::from_json(DEMO_NAMES).unwrap_or_default();
        } else {
            self.sim = None;
            let (_, names, official) = load_definitions(&self.paths, &mut self.status);
            self.pipeline.engine.names = names;
            self.official = official;
        }
        self.settings_dirty = true;
    }

    fn ingest(&mut self, p: Pkt) {
        if self.sim.is_some() {
            return;
        }
        let (lt, ts, data) = p;
        if let Some(seg) = net::parse_frame(lt, &data) {
            if self.pipeline.classify(&seg.flow).is_some() {
                // Re-encode as a clean Ethernet frame: consistent link type
                // and no MAC addresses in shared captures.
                let frame = net::build_frame_ack(&seg.flow, seg.seq, seg.ack, seg.flags, seg.payload);
                if let Some(rec) = self.recorder.as_mut() {
                    if rec.writer.write(ts, &frame).is_ok() {
                        rec.packets += 1;
                        rec.bytes += seg.payload.len() as u64;
                    }
                }
                self.rolling_bytes += frame.len();
                self.rolling.push_back((ts, frame));
                while let Some((t0, f0)) = self.rolling.front() {
                    if ts - t0 > ROLLING_SECS || self.rolling_bytes > ROLLING_MAX_BYTES {
                        self.rolling_bytes -= f0.len();
                        self.rolling.pop_front();
                    } else {
                        break;
                    }
                }
            }
        }
        for f in self.pipeline.push_frame(lt, &data, ts) {
            self.fight_finished(f);
        }
    }

    fn periodic(&mut self, now: f64) {
        if now - self.diary.last_sample >= 1.0 {
            self.diary.last_sample = now;
            self.sample_diary(now);
        }
        if let Some(sim) = self.sim.as_mut() {
            let evs = sim.step(now);
            for f in self.pipeline.push_events(&evs) {
                self.fight_finished(f);
            }
        }
        if let Some(f) = self.pipeline.tick(now) {
            self.fight_finished(f);
        }
        self.track_gaps();
        // The personal-best / first-record line stays 10 minutes, through
        // later fights; only a newer record replaces it (or ✕ in the window).
        let pb_stale = self.pb_notice.as_ref().map_or(false, |n| now - n.f64_or("at", 0.0) > 600.0);
        if pb_stale {
            self.pb_notice = None;
        }
        self.queue_loot();
        self.check_health(now);
        if self.settings.alerts.enabled && !self.events.occurrences.is_empty() {
            let fresh = self.events.notifier.poll(now, &self.events.occurrences);
            if !fresh.is_empty() && self.settings.alerts.sound {
                let kind = if fresh.iter().any(|a| a.kind == "start") { "start" } else { "soon" };
                winsys::play_wav(meter_core::chime::chime_wav(kind, (self.settings.alerts.volume / 100.0) as f32));
            }
            let hold = self.settings.alerts.hold_in_fight && self.in_fight();
            for a in fresh {
                if hold {
                    self.held.push(a);
                } else {
                    self.events.push(a);
                }
            }
        }
        // Held banners appear as soon as the fight is over.
        if !self.held.is_empty() && !(self.settings.alerts.hold_in_fight && self.in_fight()) {
            for mut a in std::mem::take(&mut self.held) {
                a.raised = now;
                self.events.push(a);
            }
        }
        if self.sim.is_none() {
            let e = &self.pipeline.engine;
            if e.self_server.is_some() && e.self_server != self.settings.last_server {
                self.settings.last_server = e.self_server;
                self.settings_dirty = true;
            }
            if e.self_odyle.is_some() && e.self_odyle != self.settings.last_odyle {
                self.settings.last_odyle = e.self_odyle;
                self.settings_dirty = true;
            }
            if e.self_keys.is_some() && e.self_keys != self.settings.last_keys {
                self.settings.last_keys = e.self_keys;
                self.settings_dirty = true;
            }
            let e = &self.pipeline.engine;
            if let Some(id) = e.self_id {
                let name = e.self_name.clone();
                let changed = self.settings.last_self.as_ref().map_or(true, |(i, n, at)| *i != id || *n != name || now - at > 600.0);
                if changed {
                    self.settings.last_self = Some((id, name, now));
                    self.settings_dirty = true;
                }
            }
        }
        if self.sim.is_none() && now - self.wanted.scanned_at > 2.0 {
            self.scan_wanted(now);
        }
        if let Some(r) = self.recorder.as_mut() {
            let _ = r.writer.flush();
        }
        if self.settings_dirty && self.last_settings_save.elapsed() > Duration::from_secs(2) {
            self.save_settings();
        }
    }

    // ------------------------------------------------------------ naming

    fn my_names_path(&self) -> PathBuf {
        self.paths.data.join("my-names.json")
    }
    pub fn site_names_path(&self) -> PathBuf {
        self.paths.data.join("gaming-tools-names.json")
    }
    fn wanted_path(&self) -> PathBuf {
        self.paths.data.join("unnamed.json")
    }

    fn load_site_missing(&mut self) {
        let Some(doc) = std::fs::read_to_string(self.site_names_path()).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else { return };
        if let Some(o) = doc["icons"].as_object() {
            self.icons = o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect();
        }
        let now = now_unix();
        if let Some(o) = doc["missing"].as_object() {
            for (k, at) in o {
                // Not on the site: ask again after a week (new pages appear).
                let again = at.as_f64().unwrap_or(0.0) + 7.0 * 86400.0;
                if again > now {
                    self.lookup_retry.insert(k.clone(), again);
                    self.lookup_missing.insert(k.clone());
                }
            }
        }
    }

    /// Next unnamed monster/skill to look up on gaming.tools, if any.
    pub fn next_lookup(&self) -> Option<(String, String, &'static str)> {
        // The daily bulk download covers everything the site knows.
        if !self.settings.lookup_names || self.sim.is_some() || self.site_bulk.is_some() {
            return None;
        }
        let now = now_unix();
        let ready = |k: &String| self.lookup_retry.get(k).map_or(true, |t| *t <= now);
        let mut npcs: Vec<_> = self.wanted.npcs.iter().collect();
        npcs.sort_by(|a, b| b.1 .3.total_cmp(&a.1 .3));
        for (id, _) in npcs {
            let k = format!("n{id}");
            if ready(&k) {
                return Some((k, meter_core::gamingtools::npc_url(*id), "NPCs"));
            }
        }
        for id in self.wanted.skills.keys() {
            let k = format!("s{id}");
            if ready(&k) {
                return Some((k, meter_core::gamingtools::skill_url(*id), "Skills"));
            }
        }
        None
    }

    /// Records a lookup result: a name, "not on the site", or a failure.
    pub fn lookup_result(&mut self, key: &str, result: Result<Option<String>, String>) {
        let now = now_unix();
        let Ok(id) = key[1..].parse::<u32>() else { return };
        let is_npc = key.starts_with('n');
        match result {
            Ok(Some(name)) => {
                self.lookup_retry.remove(key);
                self.lookup_missing.remove(key);
                self.lookup_status = Some(format!("Found {name}"));
                self.update_site_file(|doc| {
                    let sec = if is_npc { "npcs" } else { "skills" };
                    doc[sec][id.to_string()] = serde_json::json!(name);
                    if let Some(m) = doc["missing"].as_object_mut() {
                        m.remove(key);
                    }
                });
                let e = &mut self.pipeline.engine;
                if is_npc {
                    if !e.names.npcs.contains_key(&id) {
                        e.rename_npc(id, &name);
                    }
                    self.wanted.npcs.remove(&id);
                } else {
                    e.names.skills.entry(id).or_insert(name);
                    self.wanted.skills.remove(&id);
                }
                self.save_wanted();
            }
            Ok(None) => {
                self.lookup_retry.insert(key.to_string(), now + 7.0 * 86400.0);
                self.lookup_missing.insert(key.to_string());
                self.update_site_file(|doc| doc["missing"][key] = serde_json::json!(now));
            }
            Err(e) => {
                self.lookup_retry.insert(key.to_string(), now + 600.0);
                self.lookup_status = Some(e);
            }
        }
    }

    /// Stores names from the gaming.tools data files and applies them.
    pub fn apply_site_bulk(&mut self, npcs: Vec<meter_core::gamingtools::Entry>, skills: Vec<meter_core::gamingtools::Entry>, fetched_at: f64) {
        let counts = (npcs.len(), skills.len());
        let icons: HashMap<String, String> = npcs
            .iter()
            .filter_map(|e| Some((format!("n{}", e.id), e.icon.clone()?)))
            .chain(skills.iter().filter_map(|e| Some((format!("s{}", e.id), e.icon.clone()?))))
            .collect();
        self.update_site_file(|doc| {
            doc["npcs"] = serde_json::Value::Object(npcs.iter().map(|e| (e.id.to_string(), serde_json::json!(e.name))).collect());
            doc["skills"] = serde_json::Value::Object(skills.iter().map(|e| (e.id.to_string(), serde_json::json!(e.name))).collect());
            doc["icons"] = serde_json::Value::Object(icons.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect());
            doc["ranks"] = serde_json::Value::Object(npcs.iter().filter_map(|e| Some((e.id.to_string(), serde_json::json!(e.rank.clone()?)))).collect());
            doc["families"] = serde_json::Value::Object(npcs.iter().filter_map(|e| Some((e.id.to_string(), serde_json::json!(e.family.clone()?)))).collect());
            doc["missing"] = serde_json::json!({});
            doc["fetched_at"] = serde_json::json!(fetched_at);
        });
        self.site_bulk = Some((counts.0, counts.1, fetched_at));
        self.icons = icons;
        self.lookup_missing.clear();
        self.lookup_retry.clear();
        self.lookup_status = None;
        if self.sim.is_some() {
            return;
        }
        self.reload_definitions();
        // Fights already frozen with numbers pick up the new names too.
        let e = &mut self.pipeline.engine;
        let named: Vec<(u32, String)> = self.wanted.npcs.keys().filter_map(|id| e.names.npcs.get(id).map(|n| (*id, n.clone()))).collect();
        for (id, n) in named {
            e.rename_npc(id, &n);
            self.wanted.npcs.remove(&id);
        }
        let names = &e.names;
        self.wanted.skills.retain(|id, _| !names.skill_named(*id));
        self.save_wanted();
    }

    fn update_site_file(&self, f: impl FnOnce(&mut serde_json::Value)) {
        let path = self.site_names_path();
        let mut doc: serde_json::Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({}));
        if !doc.is_object() {
            doc = serde_json::json!({});
        }
        doc["about"] = serde_json::json!("Names from the AION 2 Global Database, https://aion2.gaming.tools, used with the site admin's permission. Fetched by Daeva Meter.");
        for sec in ["npcs", "skills", "missing"] {
            if !doc[sec].is_object() {
                doc[sec] = serde_json::json!({});
            }
        }
        f(&mut doc);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn load_wanted(&mut self) {
        let Ok(src) = std::fs::read_to_string(self.wanted_path()) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&src) else { return };
        let n = &self.pipeline.engine.names;
        if let Some(o) = v.get("npcs").and_then(|x| x.as_object()) {
            for (k, x) in o {
                let Ok(id) = k.parse::<u32>() else { continue };
                if n.npcs.contains_key(&id) {
                    continue;
                }
                self.wanted.npcs.insert(
                    id,
                    (x["max_hp"].as_u64(), x["zone"].as_str().map(String::from), x["first"].as_f64().unwrap_or(0.0), x["last"].as_f64().unwrap_or(0.0)),
                );
            }
        }
        if let Some(o) = v.get("skills").and_then(|x| x.as_object()) {
            for (k, x) in o {
                let Ok(id) = k.parse::<u32>() else { continue };
                if n.skill_named(id) {
                    continue;
                }
                self.wanted.skills.insert(id, (x["class"].as_str().map(String::from), x["last"].as_f64().unwrap_or(0.0)));
            }
        }
    }

    fn save_wanted(&mut self) {
        self.wanted.dirty = false;
        let npcs: serde_json::Map<String, serde_json::Value> = self
            .wanted
            .npcs
            .iter()
            .map(|(id, (hp, zone, first, last))| (id.to_string(), serde_json::json!({"max_hp": hp, "zone": zone, "first": first, "last": last})))
            .collect();
        let skills: serde_json::Map<String, serde_json::Value> =
            self.wanted.skills.iter().map(|(id, (class, last))| (id.to_string(), serde_json::json!({"class": class, "last": last}))).collect();
        let body = serde_json::to_string_pretty(&serde_json::json!({"npcs": npcs, "skills": skills})).unwrap_or_default();
        let _ = std::fs::write(self.wanted_path(), body);
    }

    fn scan_wanted(&mut self, now: f64) {
        self.wanted.scanned_at = now;
        let e = &self.pipeline.engine;
        let (npcs, skills) = e.unnamed();
        let zone = e.zone.map(|z| e.names.zones.get(&z).cloned().unwrap_or_else(|| format!("Zone {z}")));
        for (id, hp) in npcs {
            let w = self.wanted.npcs.entry(id).or_insert_with(|| {
                self.wanted.dirty = true;
                (None, zone.clone(), now, now)
            });
            if hp > w.0 {
                w.0 = hp;
                self.wanted.dirty = true;
            }
            w.3 = now;
        }
        for (id, class) in skills {
            let w = self.wanted.skills.entry(id).or_insert_with(|| {
                self.wanted.dirty = true;
                (class.clone(), now)
            });
            w.1 = now;
        }
        if self.wanted.dirty {
            self.save_wanted();
        }
    }

    /// Names a monster (kind "npc") or skill (kind "skill"). An empty name
    /// removes your entry again. Saved in my-names.json in the data folder,
    /// which survives updates and overrides names.json.
    pub fn set_name(&mut self, kind: &str, id: u32, name: &str) -> Result<String, String> {
        if self.sim.is_some() {
            return Err("Turn demo mode off first".into());
        }
        let name: String = name.trim().chars().filter(|c| !c.is_control()).collect();
        if name.chars().count() > 60 {
            return Err("Name is too long (60 characters max)".into());
        }
        let (section, key) = match kind {
            "npc" => ("npcs", id),
            "skill" => ("skills", Names::skill_name_key(id)),
            _ => return Err("unknown kind".into()),
        };
        let path = self.my_names_path();
        let mut doc: serde_json::Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({}));
        if !doc.is_object() {
            doc = serde_json::json!({});
        }
        doc["about"] = serde_json::json!("Names you gave in Daeva Meter. Overrides names.json. Keep this file: it is your own monster and skill database.");
        if !doc[section].is_object() {
            doc[section] = serde_json::json!({});
        }
        let map = doc[section].as_object_mut().unwrap();
        if name.is_empty() {
            map.remove(&key.to_string());
        } else {
            map.insert(key.to_string(), serde_json::json!(name));
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap_or_default()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        if name.is_empty() {
            // Reload so the names.json entry (if any) comes back.
            self.reload_definitions();
            return Ok(String::new());
        }
        let e = &mut self.pipeline.engine;
        match kind {
            // Saved either way; an official name stays on screen.
            "npc" if self.official.npcs.contains(&key) => {}
            "npc" => {
                e.rename_npc(key, &name);
                self.wanted.npcs.remove(&key);
            }
            _ if self.official.skills.contains(&key) => {}
            _ => {
                e.names.skills.insert(key, name.clone());
                self.wanted.skills.remove(&key);
            }
        }
        self.save_wanted();
        Ok(name)
    }

    fn lookup_state(&self, key: &str) -> &'static str {
        if !self.settings.lookup_names {
            "off"
        } else if self.lookup_missing.contains(key) || self.site_bulk.is_some() {
            "missing"
        } else if self.lookup_retry.get(key).map_or(false, |t| *t > now_unix()) {
            "failed"
        } else {
            "pending"
        }
    }

    /// A published release newer than this build that the user hasn't hidden.
    pub fn update_available(&self) -> Option<(String, String)> {
        let (tag, url) = self.update_latest.clone()?;
        let newer = version_key(&tag) > version_key(&self.app_version);
        (newer && self.settings.check_updates && tag != self.settings.skipped_update).then_some((tag, url))
    }

    pub fn update_view(&self) -> String {
        serde_json::json!({
            "current": self.app_version,
            "latest": self.update_latest.as_ref().map(|(t, _)| t.trim_start_matches('v').to_string()),
            "available": self.update_available().map(|(t, _)| t),
            "checked_at": if self.update_checked > 0.0 { Some(self.update_checked) } else { None },
            "error": self.update_error,
            "enabled": self.settings.check_updates,
        })
        .to_string()
    }

    pub fn icons_view(&self) -> String {
        let mut icons: serde_json::Map<String, serde_json::Value> = if self.settings.game_art {
            self.icons.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect()
        } else {
            Default::default()
        };
        // Icons come from your Daeva server's copy, never straight from the
        // gaming.tools CDN (their condition: download, don't hotlink).
        let mirror = self.settings.names_mirror.trim().trim_end_matches('/');
        let base = format!("{mirror}/img");
        // Our own skill icons where gaming.tools has none (the daily
        // skill-icons.json from the same server), as full addresses.
        if let (true, Some(ours)) = (self.settings.game_art, mirror.strip_suffix("/gt").filter(|b| b.starts_with("https://"))) {
            let list: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(self.paths.data.join("aion2gg").join("skill-icons.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            for (id, file) in list {
                if let Some(file) = file.as_str().filter(|f| f.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')) {
                    icons.entry(format!("s{id}")).or_insert_with(|| serde_json::json!(format!("{ours}/aion2gg/skill-icons/{file}")));
                }
            }
        }
        let on = self.settings.game_art && base.starts_with("https://");
        serde_json::json!({"base": base, "on": on, "icons": icons}).to_string()
    }

    pub fn names_view(&self) -> Value {
        let mut npcs: Vec<_> = self.wanted.npcs.iter().collect();
        npcs.sort_by(|a, b| b.1 .3.total_cmp(&a.1 .3));
        let mut skills: Vec<_> = self.wanted.skills.iter().collect();
        skills.sort_by(|a, b| b.1 .1.total_cmp(&a.1 .1));
        let mine: Option<serde_json::Value> = std::fs::read_to_string(self.my_names_path()).ok().and_then(|s| serde_json::from_str(&s).ok());
        let count = |k: &str| mine.as_ref().and_then(|m| m[k].as_object().map(|o| o.len())).unwrap_or(0);
        let named: Vec<Value> = mine
            .as_ref()
            .map(|m| {
                let mut out = Vec::new();
                let names = &self.pipeline.engine.names;
                for (sec, kind) in [("npcs", "npc"), ("skills", "skill")] {
                    if let Some(o) = m[sec].as_object() {
                        for (k, v) in o {
                            let key = k.parse::<u32>().unwrap_or(0);
                            let (official, shown) = if kind == "npc" {
                                (self.official.npcs.contains(&key), names.npcs.get(&key))
                            } else {
                                (self.official.skills.contains(&key), names.skills.get(&key))
                            };
                            out.push(
                                Obj::new()
                                    .set("kind", kind)
                                    .set("id", k.parse::<u64>().unwrap_or(0))
                                    .set("name", v.as_str().unwrap_or(""))
                                    .set("official", official)
                                    .set("shown", if official { shown.cloned() } else { None })
                                    .build(),
                            );
                        }
                    }
                }
                out
            })
            .unwrap_or_default();
        Obj::new()
            .set(
                "npcs",
                Value::Arr(
                    npcs.into_iter()
                        .map(|(id, (hp, zone, first, last))| {
                            Obj::new()
                                .set("id", *id as u64)
                                .set("max_hp", *hp)
                                .set("zone", zone.clone())
                                .set("first", *first)
                                .set("last", *last)
                                .set("lookup", self.lookup_state(&format!("n{id}")))
                                .build()
                        })
                        .collect(),
                ),
            )
            .set(
                "skills",
                Value::Arr(
                    skills
                        .into_iter()
                        .map(|(id, (class, last))| Obj::new().set("id", *id as u64).set("class", class.clone()).set("last", *last).set("lookup", self.lookup_state(&format!("s{id}"))).build())
                        .collect(),
                ),
            )
            .set("lookup_on", self.settings.lookup_names)
            .set("site_bulk", self.site_bulk.map(|(n, s, at)| Obj::new().set("npcs", n).set("skills", s).set("at", at).build()))
            .set("lookup_status", self.lookup_status.clone())
            .set("site_loading", self.site_loading)
            .set("site_count", {
                let site: Option<serde_json::Value> = std::fs::read_to_string(self.site_names_path()).ok().and_then(|s| serde_json::from_str(&s).ok());
                site.map_or(0, |d| d["npcs"].as_object().map_or(0, |o| o.len()) + d["skills"].as_object().map_or(0, |o| o.len()))
            })
            .set("mine_npcs", count("npcs"))
            .set("mine_skills", count("skills"))
            .set("named", Value::Arr(named))
            .set("file", self.my_names_path().display().to_string())
            .build()
    }

    pub fn save_settings(&mut self) {
        self.settings_dirty = false;
        self.last_settings_save = Instant::now();
        self.status.last_save_error = self.settings.save(&self.paths.settings).err().map(|e| e.to_string());
    }

    /// A fight is running (not yet ended by the idle timeout or a kill).
    pub fn in_fight(&self) -> bool {
        self.pipeline.engine.current.as_ref().map_or(false, |f| !f.finished)
    }

    /// Dropped packets plus decoder resyncs so far (both mean missed data).
    fn gap_counter(&self) -> u64 {
        // A stall restart is lost data too.
        self.status.dropped + self.pipeline.decoder.resyncs() + self.pipeline.stall_heals
    }

    /// Capture gaps since the given fight started (0 when unknown).
    pub fn fight_gaps(&self, fight_id: u64) -> u64 {
        match self.gap_base {
            Some((id, base)) if id == fight_id => self.gap_counter().saturating_sub(base),
            _ => 0,
        }
    }

    fn track_gaps(&mut self) {
        if let Some(id) = self.pipeline.engine.current.as_ref().filter(|f| !f.finished).map(|f| f.id) {
            if self.gap_base.map_or(true, |(b, _)| b != id) {
                self.gap_base = Some((id, self.gap_counter()));
            }
        }
    }

    /// Personal bests from the fights saved on disk (oldest first).
    fn load_bests(&mut self) {
        let mut files: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(&self.paths.history)
            .map(|rd| rd.flatten().filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?))).collect())
            .unwrap_or_default();
        files.retain(|(p, _)| p.extension().map_or(false, |x| x == "json"));
        files.sort_by(|a, b| a.1.cmp(&b.1));
        for (p, _) in files {
            if let Some(v) = std::fs::read_to_string(&p).ok().and_then(|s| json::parse(&s).ok()) {
                self.bests.add(&meter_core::engine::history_row(&v));
            }
        }
    }

    pub fn fight_finished(&mut self, f: Fight) {
        let mut rec = self.pipeline.engine.fight_record(&f);
        {
            let e = &self.pipeline.engine;
            let main = e.boss_target(&f).or(f.focus).or_else(|| f.main_target());
            let name = main.map(|t| e.fname(&f, t)).unwrap_or_else(|| if f.pvp.is_empty() { "?".into() } else { format!("PvP, {} players", f.pvp.len()) });
            let note = format!("fight #{} ended: {name}, {:.0} s{}", f.id, f.duration(), if f.boss_killed { ", killed" } else { "" });
            self.auto_note(now_unix(), note);
        }
        let gaps = self.fight_gaps(f.id);
        if let Value::Obj(m) = &mut rec {
            m.insert("gaps".into(), Value::from(gaps));
        }
        if self.sim.is_none() {
            let row = meter_core::engine::history_row(&rec);
            if let Some(prev) = self.bests.add(&row) {
                if self.settings.personal_bests {
                    self.pb_notice = Some(
                        Obj::new()
                            .set("fight_id", f.id)
                            .set("target", row.str_or("target", "the boss"))
                            .set("dps", row.f64_or("mine", 0.0))
                            // null on a first kill: "First record", nothing to beat yet.
                            .set("prev", prev)
                            .set("at", now_unix())
                            .build(),
                    );
                }
            }
        }
        if self.sim.is_none() {
            let name = format!("fight-{}-{}.json", f.started_unix as i64, f.id);
            if let Err(e) = std::fs::write(self.paths.history.join(name), rec.to_string()) {
                self.status.last_save_error = Some(e.to_string());
            }
        }
        // PvP stays on this PC: a PvP fight isn't shared, and a fight on
        // monsters goes without the players you fought.
        let pvp_fight = rec.get("pvp").map_or(false, |p| p.bool_or("show", false));
        if let Value::Obj(m) = &mut rec {
            m.remove("pvp");
            // The world boss rank lists strangers: it stays here too.
            m.remove("wb_rank");
        }
        let sh = &self.settings.share;
        if sh.enabled && !sh.endpoint.is_empty() && sh.scope != "none" && f.duration() >= sh.min_duration && !pvp_fight {
            if let Some(filtered) = scope_filter(rec, &sh.scope) {
                let body = Obj::new()
                    .set("type", "fight")
                    .set("meter", "daeva-meter")
                    .set("meter_version", self.app_version.as_str())
                    .set("demo", self.sim.is_some())
                    .set("record", filtered)
                    .build();
                self.push_upload(body.to_string());
            }
        }
    }

    fn queue_loot(&mut self) {
        let e = &self.pipeline.engine;
        let new = e.loot_seen.saturating_sub(self.loot_uploaded) as usize;
        if new == 0 {
            return;
        }
        self.loot_uploaded = e.loot_seen;
        let sh = &self.settings.share;
        if !(sh.enabled && sh.share_loot && !sh.endpoint.is_empty()) {
            return;
        }
        let items: Vec<Value> = e
            .loot_log
            .iter()
            .rev()
            .take(new)
            .filter(|l| l.grade.unwrap_or(0) >= sh.min_loot_grade)
            .filter(|l| sh.scope == "party" || Some(l.who) == e.self_id)
            .map(|l| e.loot_json(l))
            .collect();
        let demo = self.sim.is_some();
        for it in items {
            let body = Obj::new().set("type", "loot").set("meter", "daeva-meter").set("meter_version", self.app_version.as_str()).set("demo", demo).set("record", it).build();
            self.push_upload(body.to_string());
        }
    }

    fn push_upload(&mut self, body: String) {
        self.uploads.push(body);
        if self.uploads.len() > 100 {
            self.uploads.remove(0);
        }
    }

    fn prune_history(&self) {
        let keep = self.settings.history_keep_days.max(1) as u64 * 86400;
        if let Ok(rd) = std::fs::read_dir(&self.paths.history) {
            for e in rd.flatten() {
                let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|m| m.elapsed().ok()).map_or(false, |el| el.as_secs() > keep);
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    /// Notices when game traffic keeps arriving but nothing decodes any more
    /// (the usual sign of a game patch changing the protocol).
    fn check_health(&mut self, now: f64) {
        // The game holds a server connection but none of its packets reach
        // us (keep-alives come every few seconds even when idle): a VPN or
        // ping booster sends them through an adapter Npcap doesn't see.
        let connected = self.status.game_process.is_some() && !self.status.servers.is_empty();
        if !connected {
            self.connected_at = 0.0;
        } else if self.connected_at == 0.0 {
            self.connected_at = now;
        }
        let last = self.pipeline.last_game_ts.max(self.connected_at);
        self.no_traffic = self.sim.is_none() && connected && now - last > 20.0;
        // Fast self-heal for a stuck server stream (see Pipeline::heal_stall).
        if self.sim.is_none() {
            self.pipeline.heal_stall(now);
        }
        if self.sim.is_some() || now - self.health_at < 60.0 {
            return;
        }
        let d = &self.pipeline.decoder;
        let decoded: u64 = d.stats.values().map(|s| s.decoded).sum();
        let frames = d.frames_total;
        // Keep-alives (0x3600) and other players' movement (0x371x in a busy
        // town, also 0x3728-0x372a in an Abyss crowd: 2026-10-27, warning while
        // idle there) never decode, so they don't count towards "messages
        // arrive but none decode". Unknown opcodes still do: after a patch
        // that renumbers messages, that's all there is.
        let content: u64 = d.stats.iter().filter(|((_, op), _)| *op != KEEPALIVE_OPCODE && !(0x3710..=0x372f).contains(op)).map(|(_, s)| s.count).sum();
        let resyncs = d.resyncs();
        if self.health_at > 0.0 {
            let new_content = content.saturating_sub(self.health_content);
            let new_frames = frames.saturating_sub(self.health_frames);
            let new_decoded = decoded.saturating_sub(self.health_decoded);
            let new_resyncs = resyncs.saturating_sub(self.health_resyncs);
            let bytes_flowing = self.pipeline.last_game_ts > 0.0 && now - self.pipeline.last_game_ts < 30.0;
            self.warning = if bytes_flowing && new_frames == 0 {
                Some("Game traffic arrives but can't be read (the game may have been patched). Press Ctrl+Shift+P to send a report.".into())
            } else if new_content > 1500 && new_decoded == 0 {
                Some("Game messages arrive but none decode any more (the game may have been patched). Press Ctrl+Shift+P to send a report.".into())
            } else if new_resyncs > 50 {
                Some("Lots of unreadable game traffic. If damage looks wrong, press Ctrl+Shift+P to send a report.".into())
            } else {
                None
            };
        }
        self.health_at = now;
        self.health_decoded = decoded;
        self.health_frames = frames;
        self.health_content = content;
        self.health_resyncs = resyncs;
    }

    /// Writes one zip with the last few minutes of game traffic plus what the
    /// meter showed. Returns the file path.
    pub fn report_problem(&mut self, note: &str) -> Result<String, String> {
        let mut pcap = Vec::new();
        {
            let mut w = PcapWriter::new(&mut pcap, net::DLT_EN10MB).map_err(|e| e.to_string())?;
            for (ts, f) in &self.rolling {
                w.write(*ts, f).map_err(|e| e.to_string())?;
            }
        }
        let e = &self.pipeline.engine;
        let fights: Vec<Value> = e.history.iter().rev().take(5).map(|f| e.fight_record(f)).chain(e.current.as_ref().map(|f| e.fight_record(f))).collect();
        let mut settings = self.settings.clone();
        settings.share.token = String::new();
        let info = Obj::new()
            .set("meter_version", self.app_version.as_str())
            .set("definitions", self.status.defs_name.as_str())
            .set("created_unix", now_unix())
            .set("note", note)
            .set("seconds_of_traffic", self.rolling.back().zip(self.rolling.front()).map(|(b, a)| b.0 - a.0).unwrap_or(0.0))
            .set("self_id", e.self_id)
            .set("self_name", e.self_name.clone())
            .set("snapshot", self.snapshot(None))
            .set("decoder", self.research_view())
            .set("fights", Value::Arr(fights))
            .set("diary", Value::Arr(self.diary.events.iter().map(|(t, s)| Obj::new().set("t", *t).set("what", s.as_str()).build()).collect()))
            .set("screen", Value::Arr(self.diary.screen.iter().map(|(t, v)| Obj::new().set("t", *t).set("shown", v.clone()).build()).collect()))
            .set("settings", json::parse(&serde_json::to_string(&settings).unwrap_or_else(|_| "{}".into())).unwrap_or(Value::Null))
            .build();
        let stamp = now_unix() as i64;
        let zip = meter_core::zipstore::build(&[("capture.pcap", &pcap), ("report.json", info.to_string_pretty().as_bytes())]);
        let desktop = std::env::var("USERPROFILE").ok().map(|h| PathBuf::from(h).join("Desktop")).filter(|d| d.is_dir());
        let dir = desktop.unwrap_or_else(|| self.paths.data.join("reports"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("DaevaMeter-report-{stamp}.zip"));
        std::fs::write(&path, zip).map_err(|e| e.to_string())?;
        self.diary.saved = Some((now_unix(), path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
        Ok(path.display().to_string())
    }

    /// A note for problem reports, and an automatic marker in a research
    /// The meter had to wake or reload the alert window: count it, and note
    /// a reload in reports and recordings.
    pub fn toast_fix(&mut self, now: f64, what: &str, reload: bool) {
        let h = &mut self.toast_health;
        if reload { h.reloads += 1 } else { h.nudges += 1 }
        h.last_fix = Some((now, what.to_string()));
        if reload {
            self.auto_note(now, format!("alert window reloaded: {what}"));
        }
    }

    /// recording (so testers don't have to type what they're doing).
    fn auto_note(&mut self, ts: f64, what: String) {
        if let Some(r) = self.recorder.as_mut() {
            r.markers.push((ts, format!("auto: {what}")));
            r.save_markers();
        }
        self.diary.note(ts, what);
    }

    /// Once a second: what the overlay shows, and a note whenever the fight,
    /// the card, the party, the zone or the layout changes.
    fn sample_diary(&mut self, now: f64) {
        let snap = self.snapshot(None);
        let f = snap.get("fight").filter(|f| f.as_obj().is_some());
        let t = f.and_then(|f| f.get("target")).filter(|t| t.as_obj().is_some());
        let card = t.map(|t| format!("{} ({})", t.str_or("name", "?"), t.str_or("rank", "no rank"))).unwrap_or_default();
        let rows: Vec<Value> = f
            .and_then(|f| f.get("players"))
            .and_then(|p| p.as_arr())
            .map(|ps| {
                ps.iter()
                    .take(8)
                    .map(|p| {
                        let tag = if p.bool_or("is_self", false) { " (me)" } else if p.bool_or("in_party", false) { " *" } else { "" };
                        Value::from(format!("{}{tag} {:.0}/s", p.str_or("name", "?"), p.f64_or("dps", 0.0)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let shown = Obj::new()
            .set("layout", self.settings.layout.as_str())
            .set("fight", f.and_then(|f| f.get("fight_id")).cloned().unwrap_or(Value::Null))
            .set("live", f.map_or(false, |f| f.bool_or("live", false)))
            .set("secs", f.map(|f| (f.f64_or("duration", 0.0) * 10.0).round() / 10.0))
            .set("card", card.as_str())
            .set("hp", t.and_then(|t| t.get("hp_pct")).cloned().unwrap_or(Value::Null))
            .set("rows", Value::Arr(rows))
            .build();
        self.diary.screen.push_back((now, shown));
        while self.diary.screen.front().map_or(false, |(t0, _)| now - t0 > ROLLING_SECS) {
            self.diary.screen.pop_front();
        }
        let e = &self.pipeline.engine;
        let mut members: Vec<&String> = e.party.iter().collect();
        members.sort();
        let party = members.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        let fight = f.and_then(|f| f.get("fight_id")).and_then(|v| v.as_f64()).map(|v| v as u64);
        let live = f.map_or(false, |f| f.bool_or("live", false));
        let (self_id, self_name) = (e.self_id, e.self_name.clone());
        let mut notes = Vec::new();
        if party != self.diary.party {
            notes.push(if party.is_empty() { "party: none (left)".to_string() } else { format!("party: {party}") });
        }
        if self_id != self.diary.self_id && self.diary.self_id.is_some() {
            notes.push("zone change or relog (new character id)".to_string());
        }
        if self_name != self.diary.self_name {
            notes.push(format!("you are {}", self_name.as_deref().unwrap_or("unknown")));
        }
        if fight != self.diary.fight && fight.is_some() && live {
            notes.push(format!("fight #{} starts", fight.unwrap_or(0)));
        }
        if card != self.diary.card && !card.is_empty() {
            notes.push(format!("card shows {card}"));
        }
        if self.settings.layout != self.diary.layout {
            notes.push(format!("layout {}", self.settings.layout));
        }
        self.diary.party = party;
        self.diary.fight = fight;
        self.diary.card = card;
        self.diary.self_id = self_id;
        self.diary.self_name = self_name;
        self.diary.layout = self.settings.layout.clone();
        for n in notes {
            self.auto_note(now, n);
        }
    }

    // ------------------------------------------------------------ research

    pub fn research_start(&mut self, label: &str) -> Result<String, String> {
        if self.recorder.is_some() {
            return Err("already recording".into());
        }
        std::fs::create_dir_all(&self.paths.research).map_err(|e| e.to_string())?;
        let safe: String = label.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(40).collect();
        let stamp = now_unix() as i64;
        let path = self.paths.research.join(format!("capture-{stamp}-{safe}.pcap"));
        let f = File::create(&path).map_err(|e| e.to_string())?;
        let writer = PcapWriter::new(BufWriter::new(f), net::DLT_EN10MB).map_err(|e| e.to_string())?;
        self.recorder = Some(Recorder { writer, path: path.clone(), markers: Vec::new(), started: now_unix(), packets: 0, bytes: 0, label: label.to_string() });
        self.pipeline.decoder.keep_recent = 400;
        Ok(path.display().to_string())
    }

    pub fn research_marker(&mut self, label: &str) -> Result<usize, String> {
        let r = self.recorder.as_mut().ok_or("not recording")?;
        r.markers.push((now_unix(), label.to_string()));
        r.save_markers();
        Ok(r.markers.len())
    }

    pub fn research_stop(&mut self) -> Result<String, String> {
        let mut r = self.recorder.take().ok_or("not recording")?;
        let _ = r.writer.flush();
        r.save_markers();
        self.pipeline.decoder.keep_recent = 200;
        Ok(r.path.display().to_string())
    }

    // ------------------------------------------------------------ views

    pub fn snapshot(&self, target: Option<u64>) -> Value {
        let e = &self.pipeline.engine;
        let fight = e.display_fight().map(|f| e.fight_table(f, target));
        let s = &self.settings;
        let st = &self.status;
        let p = &self.pipeline;
        let now = now_unix();
        let rec = self.recorder.as_ref().map(|r| {
            Obj::new()
                .set("path", r.path.display().to_string())
                .set("elapsed", now - r.started)
                .set("packets", r.packets)
                .set("bytes", r.bytes)
                .set("markers", r.markers.len())
                .set("label", r.label.as_str())
                .build()
        });
        let status = Obj::new()
            .set("demo", self.sim.is_some())
            .set("npcap", st.npcap_version.clone())
            .set("npcap_error", st.npcap_error.clone())
            .set("devices", Value::Arr(st.devices.iter().map(|d| Value::from(d.as_str())).collect()))
            .set("devices_skipped", Value::Arr(st.devices_skipped.iter().map(|d| Value::from(d.as_str())).collect()))
            .set("device_errors", Value::Arr(st.device_errors.iter().map(|d| Value::from(d.as_str())).collect()))
            .set("game_process", st.game_process.as_ref().map(|(pid, n)| format!("{n} (pid {pid})")))
            .set("servers", Value::Arr(st.servers.iter().map(|d| Value::from(d.as_str())).collect()))
            .set("packets", p.packets)
            .set("game_packets", p.game_packets)
            .set("game_bytes", p.game_bytes)
            // The game reaches its server through a nearby Cloudflare relay,
            // which answers at once, so the traffic's own timing would show
            // the relay (a few ms). Only the game server's timing is shown.
            .set("ping_ms", self.game_rtt.as_ref().filter(|(_, at, _)| now - at < 180.0).map(|(ms, _, _)| *ms))
            .set("ping_where", self.game_rtt.as_ref().filter(|(_, at, _)| now - at < 180.0).map(|(_, _, w)| w.as_str()))
            .set("ping_note", self.ping_note)
            .set("game_idle", if p.last_game_ts > 0.0 { Some(now - p.last_game_ts) } else { None })
            .set("frames", p.decoder.frames_total)
            .set("resyncs", p.decoder.resyncs())
            .set("stall_heals", p.stall_heals)
            .set("events", e.events_seen)
            .set("dropped", st.dropped)
            .set("defs", st.defs_name.as_str())
            .set("defs_error", st.defs_error.clone())
            .set("names_error", st.names_error.clone())
            .set("defs_messages", p.decoder.defs.messages.len())
            .set("hotkey_errors", Value::Arr(st.hotkey_errors.iter().map(|d| Value::from(d.as_str())).collect()))
            .set("save_error", st.last_save_error.clone())
            .set("self", e.self_id.map(|id| e.actor_name(id)))
            .set("self_class", e.self_id.and_then(|id| e.actor_class(id)))
            .set("self_cp", e.self_cp)
            .set("odyle", e.self_odyle.or(self.settings.last_odyle).map(|o| o.0))
            .set("odyle_bonus", e.self_odyle.or(self.settings.last_odyle).map(|o| o.1))
            // Remembered from an earlier session (Odyle regenerates while
            // you're away, so it's shown as "last seen" until the game sends it).
            .set("needs_zone", p.last_game_ts > 0.0 && e.needs_zone())
            // "loading" while a fresh install downloads the monster data, so
            // the boss card can say so instead of looking incomplete.
            .set("monster_data", if self.site_bulk.is_some() { None } else if self.site_loading { Some("loading") } else { None })
            .set("odyle_stale", e.self_odyle.is_none() && self.settings.last_odyle.is_some())
            .set("server", {
                let manual = self.settings.server_name.trim();
                if !manual.is_empty() {
                    Some(manual.to_string())
                } else {
                    e.self_server.or(self.settings.last_server).map(|id| e.names.server(id))
                }
            })
            .set("uploads_pending", self.uploads.len())
            .set("update_version", self.update_available().map(|(v, _)| v))
            .set("update_url", self.update_available().map(|(_, u)| u))
            .set("self_update", self.self_update.as_ref().and_then(|u| u.view()))
            .set("update_note", self.self_update.as_ref().and_then(|u| u.note()))
            .set("version", self.app_version.as_str())
            .set("warning", if self.no_traffic { Some(NO_TRAFFIC.to_string()) } else { self.warning.clone() })
            .set("pb", self.pb_notice.clone())
            // "Report saved" for a few seconds after Ctrl+Shift+P.
            .set("opened_again", self.opened_again.filter(|t| now_unix() - t < 6.0))
            .set("report_saved", self.diary.saved.as_ref().filter(|(t, _)| now_unix() - t < 6.0).map(|(_, n)| n.as_str()))
            .set("fight_gaps", e.display_fight().map(|f| self.fight_gaps(f.id)))
            .build();
        let ui = Obj::new()
            .set("opacity", s.opacity)
            .set("scale", s.scale)
            .set("accent", s.accent.as_str())
            .set("rows", s.rows)
            .set("show_heal", s.show_heal)
            .set("show_target", s.show_target)
            .set("show_players", s.show_players.as_str())
            .set("wb_rank", s.wb_rank)
            .set("primary_metric", s.primary_metric.as_str())
            .set("anonymize_others", s.anonymize_others)
            .set("click_through", s.click_through)
            .set("show_research", s.show_research)
            .set("columns", Value::Arr(s.columns.iter().take(3).map(|c| Value::from(c.as_str())).collect()))
            .set("dps_clock", s.dps_clock.as_str())
            .set("layout", s.layout.as_str())
            .set("solo", Obj::new().set("rank", s.solo.rank).set("chips", s.solo.chips).set("footer", s.solo.footer).build())
            .build();
        let alerts = self.alerts_view(now);
        let party = self.party_view();
        Obj::new().set("status", status).set("ui", ui).set("fight", fight).set("research", rec).set("alerts", alerts).set("party", party).build()
    }

    /// The party panel: who is in your party (class, server, gear score,
    /// combat power, leader, Ready), and whether to show it now.
    pub fn party_view(&self) -> Value {
        let e = &self.pipeline.engine;
        let Some(p) = &e.party_info else { return Value::Null };
        let room = p.members.iter().any(|m| m.status.is_some());
        let members: Vec<Value> = p
            .members
            .iter()
            .map(|m| {
                let server = m.server.and_then(|s| e.names.servers.get(&s).cloned());
                let tag = server.as_ref().map(|n| n.chars().take(3).collect::<String>().to_uppercase()).or(m.server.map(|s| s.to_string()));
                Obj::new()
                    .set("name", m.name.as_str())
                    .set("class", m.code.and_then(|c| e.names.party_class(c).cloned()))
                    .set("server", server)
                    .set("server_tag", tag)
                    .set("gs", m.gs)
                    .set("cp", m.cp)
                    .set("leader", p.leader.is_some() && m.id == p.leader)
                    .set("ready", m.status.map(|s| s & 0x02 != 0))
                    // The game's bit, or seen fighting here (the bit stays off
                    // for some members who joined mid-run).
                    .set("in_dungeon", m.status.map(|s| s & meter_core::engine::IN_DUNGEON != 0 || (p.inside && e.fought_here(&m.name))))
                    .set("new", e.party_is_new(&m.name))
                    .set("is_self", e.self_name.as_deref() == Some(m.name.as_str()))
                    .set("faction", e.faction_by_name(&m.name))
                    .build()
            })
            .collect();
        // Left while you were inside: shown faded for a few seconds.
        let left: Vec<Value> = e.party_recently_left().into_iter().map(|m| Obj::new().set("name", m.name.as_str()).set("class", m.code.and_then(|c| e.names.party_class(c).cloned())).build()).collect();
        Obj::new()
            .set("title", p.title.clone())
            .set("room", room)
            // Queueing for a dungeon: the full panel while you're idle (even
            // with a fight nearby), a slim bar over the rows while you fight.
            .set("lobby", e.party_lobby() || e.party_queue_panel())
            .set("queue_bar", e.party_queueing() && e.self_active())
            .set("entering", p.entering)
            .set("rerun", p.rerun)
            .set("ready", e.party_ready())
            .set("inside", p.inside)
            .set("waiting", p.waiting)
            .set("size", p.size)
            .set("members", Value::Arr(members))
            .set("left", Value::Arr(left))
            .build()
    }

    /// Event alerts for the overlay's next-events line and the alert window.
    pub fn alerts_view(&self, now: f64) -> Value {
        let a = &self.settings.alerts;
        Obj::new()
            .set("enabled", a.enabled)
            .set("sound", a.sound)
            .set("show_next", a.show_next)
            .set("banner_secs", a.banner_secs)
            .set("position", a.position.as_str())
            .set("size", a.size.as_str())
            .set("held", self.held.len())
            .set("movable", !self.settings.click_through)
            .set("anchor", self.toast_anchor)
            .set("error", self.events.error.clone())
            // Keys per event id, once the game's key counter is decoded.
            .set("keys", match self.pipeline.engine.self_keys.or(self.settings.last_keys) {
                Some(k) => Obj::new().set("shugo_festival", k).build(),
                None => Value::Null,
            })
            .set(
                "next",
                Value::Arr(if a.enabled {
                    schedule::upcoming(&self.events.occurrences, now, &self.events.notifier.muted, 8).into_iter().map(|o| schedule::occurrence_json(o, now)).collect()
                } else {
                    vec![]
                }),
            )
            .set(
                "recent",
                Value::Arr(
                    self.events
                        .recent
                        .iter()
                        .filter(|x| self.events.listed(x, now, a.banner_secs))
                        .map(|x| {
                            let mut j = x.to_json();
                            if let Value::Obj(m) = &mut j {
                                m.insert("shown".into(), Value::from(self.events.acked.contains(&x.key)));
                            }
                            j
                        })
                        .collect(),
                ),
            )
            // For reports: is the alert window answering?
            .set("window", {
                let h = &self.toast_health;
                Obj::new()
                    .set("last_poll_ago", if h.seen > 0.0 { Some(((now - h.seen) * 10.0).round() / 10.0) } else { None })
                    .set("nudges", h.nudges)
                    .set("reloads", h.reloads)
                    .set("last_fix", h.last_fix.as_ref().map(|(t, w)| format!("{} ({:.0} s ago)", w, now - t)))
                    .build()
            })
            .build()
    }

    /// Everything the settings page needs about events.
    pub fn events_view(&self) -> Value {
        let now = now_unix();
        let mut seen = HashSet::new();
        let catalog: Vec<Value> = self
            .events
            .occurrences
            .iter()
            .filter(|o| seen.insert(o.id.clone()))
            .map(|o| Obj::new().set("id", o.id.as_str()).set("name", o.name.as_str()).set("emoji", o.emoji.as_str()).build())
            .collect();
        let none = HashSet::new();
        Obj::new()
            .set("catalog", Value::Arr(catalog))
            .set("upcoming", Value::Arr(schedule::upcoming(&self.events.occurrences, now, &none, 12).into_iter().map(|o| schedule::occurrence_json(o, now)).collect()))
            .set("fetched_at", if self.events.fetched_at > 0.0 { Some(self.events.fetched_at) } else { None })
            .set("error", self.events.error.clone())
            .build()
    }

    pub fn test_alert(&mut self) {
        let now = now_unix();
        self.events.push(Alert {
            key: format!("test|{now}"),
            id: "test".into(),
            title: "Spacetime Rift".into(),
            body: "Starts in 5 min · test alert".into(),
            emoji: "🌀".into(),
            kind: "soon",
            starts: now + 300.0,
            raised: now,
        });
        if self.settings.alerts.sound {
            winsys::play_wav(meter_core::chime::chime_wav("soon", (self.settings.alerts.volume / 100.0) as f32));
        }
    }

    pub fn research_view(&self) -> Value {
        let d = &self.pipeline.decoder;
        let mut stats: Vec<_> = d.stats.iter().collect();
        stats.sort_by(|a, b| b.1.count.cmp(&a.1.count));
        let ops: Vec<Value> = stats
            .iter()
            .take(60)
            .map(|((srv, op), s)| {
                Obj::new()
                    .set("dir", if *srv { "S" } else { "C" })
                    .set("opcode", format!("0x{op:x}"))
                    .set("name", d.defs.messages.get(op).and_then(|m| m.first()).map(|m| m.name.clone()))
                    .set("count", s.count)
                    .set("avg_len", s.bytes as f64 / s.count.max(1) as f64)
                    .set("decoded", s.decoded)
                    .set("errors", s.errors)
                    .set("sample", meter_core::wire::hex(&s.last[..s.last.len().min(48)]))
                    .build()
            })
            .collect();
        let recent: Vec<Value> = d
            .recent
            .iter()
            .rev()
            .take(150)
            .map(|f| {
                Obj::new()
                    .set("ts", f.ts)
                    .set("dir", if f.from_server { "S" } else { "C" })
                    .set("opcode", format!("0x{:x}", f.opcode))
                    .set("name", f.name.clone())
                    .set("len", f.len)
                    .set("fields", f.fields.as_ref().map(meter_core::protocol::scope_json))
                    .set("hex", meter_core::wire::hex(&f.raw_head))
                    .build()
            })
            .collect();
        let markers: Vec<Value> = self
            .recorder
            .as_ref()
            .map(|r| r.markers.iter().map(|(t, l)| Obj::new().set("t", *t).set("label", l.as_str()).build()).collect())
            .unwrap_or_default();
        Obj::new().set("ops", Value::Arr(ops)).set("recent", Value::Arr(recent)).set("markers", Value::Arr(markers)).set("folder", self.paths.research.display().to_string()).build()
    }

    pub fn saved_fights(&self) -> Value {
        let mut files: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(&self.paths.history)
            .map(|rd| rd.flatten().filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?))).collect())
            .unwrap_or_default();
        files.retain(|(p, _)| p.extension().map_or(false, |x| x == "json"));
        files.sort_by(|a, b| b.1.cmp(&a.1));
        let items: Vec<Value> = files
            .iter()
            .take(300)
            .filter_map(|(p, _)| {
                let src = fix_names(&std::fs::read_to_string(p).ok()?, &self.pipeline.engine.names);
                let v = json::parse(&src).ok()?;
                let mut row = meter_core::engine::history_row(&v);
                let pb = self.bests.is_best(&row);
                if let Value::Obj(m) = &mut row {
                    m.insert("file".into(), Value::from(p.file_name()?.to_string_lossy().to_string()));
                    m.insert("pb".into(), Value::from(pb));
                }
                Some(row)
            })
            .collect();
        Value::Arr(items)
    }
}

/// "0.6.14" → [0, 6, 14, MAX] for comparing versions. A test version
/// sorts just below its release: "0.5.2-test.2" → [0, 5, 2, 2].
pub fn version_key(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches('v');
    let (main, pre) = v.split_once('-').map_or((v, None), |(m, p)| (m, Some(p)));
    let mut key: Vec<u64> = main.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect();
    key.push(pre.map_or(u64::MAX, |p| p.split(|c: char| !c.is_ascii_digit()).filter_map(|d| d.parse().ok()).next_back().unwrap_or(0)));
    key
}

/// Monster templates and skill name keys that have an official name.
#[derive(Default)]
pub struct Official {
    pub npcs: HashSet<u32>,
    pub skills: HashSet<u32>,
}

fn load_definitions(paths: &Paths, status: &mut Status) -> (Arc<Definitions>, Names, Official) {
    status.defs_error = None;
    status.names_error = None;
    // Your copy in the definitions folder overrides the bundled one only
    // while it is at least as new; an old copy left behind after an update
    // must not hide the fixes the update ships.
    let load = |p: &std::path::Path| std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|s| Definitions::from_json(&s)).map_err(|e| format!("{}: {e}", p.display()));
    let user = paths.data.join("definitions").join("aion2.json");
    let bundled = paths.resources.join("definitions").join("aion2.json");
    let user = user.exists().then(|| load(&user));
    let bundled = bundled.exists().then(|| load(&bundled));
    let defs = match (user, bundled) {
        (Some(Ok(u)), Some(Ok(b))) => {
            if version_key(&u.version) >= version_key(&b.version) { u } else { b }
        }
        (Some(Ok(d)), _) | (_, Some(Ok(d))) => d,
        (Some(Err(e)), _) | (None, Some(Err(e))) => {
            status.defs_error = Some(e);
            Definitions::from_json(FALLBACK_DEFS).unwrap()
        }
        (None, None) => {
            status.defs_error = Some("aion2.json not found".into());
            Definitions::from_json(FALLBACK_DEFS).unwrap()
        }
    };
    status.defs_name = format!("{} v{}", defs.name, defs.version);
    // The bundled names.json is the base; one dropped into the data folder's
    // definitions adds to it and wins where both have an entry. (It used to
    // replace it outright, so an older copy there hid newer data such as
    // enrage timers and party class numbers.)
    // A2Tools' monster names (by permission, in their own folder) come first: they
    // only fill ids gaming.tools doesn't list, so everything else wins.
    let mut names = Names::default();
    // aion2.gg enrage times: the daily download, else the copy in the
    // installer. Our own list ("berserk" in names.json) wins over them.
    for p in [paths.data.join("aion2gg").join("boss-fights.json"), paths.resources.join("definitions").join("aion2gg").join("boss-fights.json")] {
        if let Ok(gg) = std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Names::from_aion2gg(&s)) {
            names.merge(gg);
            break;
        }
    }
    for p in [paths.resources.join("definitions").join("a2tools").join("names.json"), paths.resources.join("definitions").join("names.json"), paths.data.join("definitions").join("names.json")] {
        if !p.exists() {
            continue;
        }
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Names::from_json(&s)) {
            Ok(n) => names.merge(n),
            Err(e) => status.names_error = Some(format!("{}: {e}", p.display())),
        }
    }
    // gaming.tools names (used with permission) take precedence over the
    // bundled names.json; your own names (my-names.json) only fill the gaps,
    // so a name typed before the official one was known doesn't hide it.
    if let Ok(site) = std::fs::read_to_string(paths.data.join("gaming-tools-names.json")).map_err(|e| e.to_string()).and_then(|s| Names::from_json(&s)) {
        names.npcs.extend(site.npcs);
        names.skills.extend(site.skills);
        names.ranks.extend(site.ranks);
        names.families.extend(site.families);
    }
    let official = Official { npcs: names.npcs.keys().copied().collect(), skills: names.skills.keys().copied().collect() };
    let mine = paths.data.join("my-names.json");
    if mine.exists() {
        match std::fs::read_to_string(&mine).map_err(|e| e.to_string()).and_then(|s| Names::from_json(&s)) {
            Ok(m) => {
                for (k, v) in m.npcs {
                    names.npcs.entry(k).or_insert(v);
                }
                for (k, v) in m.skills {
                    names.skills.entry(k).or_insert(v);
                }
            }
            Err(e) => status.names_error = Some(format!("{}: {e}", mine.display())),
        }
    }
    (Arc::new(defs), names, official)
}

/// Restricts a fight record to what the user agreed to share.
fn scope_filter(mut rec: Value, scope: &str) -> Option<Value> {
    if scope == "party" {
        return Some(rec);
    }
    // "self": keep only rows flagged is_self (details have no flag, match ids)
    let Value::Obj(m) = &mut rec else { return None };
    let self_ids: HashSet<u64> = m
        .get("players")
        .and_then(|p| p.as_arr())
        .map(|a| a.iter().filter(|r| r.bool_or("is_self", false)).filter_map(|r| r.get("id").and_then(|i| i.as_u64())).collect())
        .unwrap_or_default();
    if self_ids.is_empty() {
        return None;
    }
    for key in ["players", "details"] {
        if let Some(Value::Arr(a)) = m.get_mut(key) {
            a.retain(|r| r.get("id").and_then(|i| i.as_u64()).map_or(false, |i| self_ids.contains(&i)));
        }
    }
    m.remove("deaths");
    if let Some(Value::Arr(a)) = m.get_mut("loot") {
        a.retain(|l| l.bool_or("is_self", false));
    }
    Some(rec)
}

impl Shared {
    pub fn new(data: PathBuf, resources: PathBuf) -> Arc<Shared> {
        Arc::new(Shared { core: Mutex::new(Core::new(data, resources)), capture_gen: AtomicU64::new(0), hotkeys: Mutex::new(None) })
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ---------------------------------------------------------------- threads

pub fn start_threads(shared: Arc<Shared>) {
    let (tx, rx) = sync_channel::<Pkt>(65536);
    {
        let s = shared.clone();
        std::thread::Builder::new().name("processor".into()).spawn(move || processor(s, rx)).ok();
    }
    {
        let s = shared.clone();
        std::thread::Builder::new().name("detector".into()).spawn(move || detector(s)).ok();
    }
    {
        let s = shared.clone();
        std::thread::Builder::new().name("event-feed".into()).spawn(move || event_feed(s)).ok();
        let s = shared.clone();
        std::thread::Builder::new().name("latency-probe".into()).spawn(move || latency_probe(s)).ok();
    }
    {
        let s = shared.clone();
        std::thread::Builder::new().name("update-check".into()).spawn(move || update_check(s)).ok();
    }
    // Page-by-page look-ups on gaming.tools are retired: names come only
    // from the Daeva server's mirror. (name_lookup kept for reference.)
    let _ = name_lookup;
    {
        let s = shared.clone();
        std::thread::Builder::new().name("site-sync".into()).spawn(move || site_sync(s)).ok();
    }
    {
        let s = shared.clone();
        std::thread::Builder::new().name("boss-data-sync".into()).spawn(move || boss_data_sync(s)).ok();
    }
    {
        let s = shared.clone();
        std::thread::Builder::new().name("capture-supervisor".into()).spawn(move || supervisor(s, tx)).ok();
    }
}

fn processor(shared: Arc<Shared>, rx: Receiver<Pkt>) {
    let mut last_tick = Instant::now();
    loop {
        let mut batch = Vec::new();
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(p) => {
                batch.push(p);
                while batch.len() < 4096 {
                    match rx.try_recv() {
                        Ok(p) => batch.push(p),
                        Err(_) => break,
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        // A full batch means packets are still queued: catch up before the
        // clock-based checks run, or the idle check ends a fight whose hits
        // are only waiting in the queue (Shugo, 2026-10-28: a wave split in
        // two after the meter fell 10 s behind).
        let behind = batch.len() >= 4096;
        let mut core = shared.lock();
        for p in batch {
            core.ingest(p);
        }
        if !behind && last_tick.elapsed() >= Duration::from_millis(250) {
            last_tick = Instant::now();
            core.periodic(now_unix());
        }
    }
}

/// Polls the Daeva schedule feed once a minute (sooner after a settings change).
fn event_feed(shared: Arc<Shared>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("DaevaMeter/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut last_wake = u64::MAX;
    let mut next = Instant::now();
    let mut failures = 0u32;
    loop {
        let (enabled, url, wake) = {
            let c = shared.lock();
            (c.settings.alerts.enabled, c.settings.alerts.feed_url.trim().to_string(), c.events.wake)
        };
        if wake != last_wake {
            last_wake = wake;
            next = Instant::now();
        }
        if enabled && !url.is_empty() && Instant::now() >= next {
            let result = agent
                .get(&url)
                .call()
                .map_err(|e| format!("Event feed unreachable: {e}"))
                .and_then(|r| r.into_string().map_err(|e| format!("Event feed read failed: {e}")))
                .and_then(|body| {
                    // Where to time the ping, per server number; the feed can
                    // move a server or add a region (addresses scrambled like
                    // GAME_SERVER_X).
                    let probe = meter_core::json::parse(&body).ok().and_then(|v| v.get("latency_probes")?.as_arr().map(|list| list.iter().filter_map(Probe::from_feed).collect::<Vec<_>>()));
                    schedule::parse_overview(&body).map(|o| (o, probe))
                });
            let mut c = shared.lock();
            match result {
                Ok((occs, probe)) => {
                    failures = 0;
                    if let Some(list) = probe {
                        c.probes = list;
                    }
                    c.events.occurrences = occs;
                    c.events.fetched_at = now_unix();
                    c.events.error = None;
                    next = Instant::now() + Duration::from_secs(60);
                }
                Err(e) => {
                    failures += 1;
                    // Keep the last good schedule; it stays valid for hours.
                    c.events.error = Some(e);
                    next = Instant::now() + Duration::from_secs((30 * failures as u64).min(300));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A game server to time for the players of some servers.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    /// Server numbers as inclusive ranges (Beritra is 2508).
    servers: Vec<(u32, u32)>,
    name: String,
    host: String,
    port: u16,
}

impl Probe {
    /// {"servers": "1501-1510,2501-2510", "name": "Tokyo", "code": "..."}
    fn from_feed(v: &meter_core::json::Value) -> Option<Probe> {
        let servers = parse_ranges(v.get("servers")?.as_str()?);
        let (host, port) = decode_server(v.get("code")?.as_str()?)?;
        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").chars().filter(|c| c.is_alphanumeric() || " -".contains(*c)).take(24).collect();
        (!servers.is_empty()).then_some(Probe { servers, name, host, port })
    }

    fn covers(&self, server: u32) -> bool {
        self.servers.iter().any(|(a, b)| (*a..=*b).contains(&server))
    }
}

/// "1501-1510,2501-2510" (single numbers too).
fn parse_ranges(s: &str) -> Vec<(u32, u32)> {
    s.split(',')
        .filter_map(|part| {
            let mut it = part.trim().splitn(2, '-').map(|x| x.trim().parse::<u32>());
            let a = it.next()?.ok()?;
            let b = it.next().map_or(Some(a), |x| x.ok())?;
            (a <= b).then_some((a, b))
        })
        .collect()
}

/// The Asia game servers (Tokyo), where the bot measures latency too; used
/// when the feed hasn't answered. Kept scrambled so the address of the server
/// behind Cloudflare isn't spelled out in the source.
const GAME_SERVER_X: ([u8; 4], u16) = ([0x97, 0x7d, 0xc1, 0x22], 0x93b7);

fn built_in_probe() -> Probe {
    let ([a, b, c, d], p) = GAME_SERVER_X;
    Probe {
        servers: vec![(1501, 1510), (2501, 2510)],
        name: "Tokyo".into(),
        host: format!("{}.{}.{}.{}", a ^ 0xa7, b ^ 0xa7, c ^ 0xa7, d ^ 0xa7),
        port: p ^ 0xa7a7,
    }
}

/// The feed's form: 12 hex digits, the four address bytes and the port, each
/// byte scrambled the same way.
fn decode_server(code: &str) -> Option<(String, u16)> {
    if code.len() != 12 || !code.is_ascii() {
        return None;
    }
    let b: Vec<u8> = (0..6).map(|i| u8::from_str_radix(&code[i * 2..i * 2 + 2], 16).map(|x| x ^ 0xa7)).collect::<Result<_, _>>().ok()?;
    let port = u16::from_be_bytes([b[4], b[5]]);
    (port > 0).then(|| (format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]), port))
}

/// The game server for a player's server number: the feed's list first.
fn probe_for(list: &[Probe], server: u32) -> Option<Probe> {
    list.iter().find(|p| p.covers(server)).cloned().or_else(|| Some(built_in_probe()).filter(|p| p.covers(server)))
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn the_feed_code_and_the_built_in_server_agree() {
        // A documentation address, as in the bot's test.
        assert_eq!(decode_server("67a7a5ad93b7"), Some(("192.0.2.10".to_string(), 13328)));
        let p = built_in_probe();
        let ip: Vec<u8> = p.host.split('.').map(|x| x.parse().unwrap()).collect();
        let code: String = ip.iter().chain(p.port.to_be_bytes().iter()).map(|b| format!("{:02x}", b ^ 0xa7)).collect();
        assert_eq!(decode_server(&code), Some((p.host.clone(), p.port)));
        assert_eq!(decode_server("not-a-code!!"), None);
    }

    #[test]
    fn each_player_gets_the_game_server_of_their_region() {
        let feed = meter_core::json::parse(r#"{"latency_probes": [
            {"servers": "3001-3010", "name": "Seoul", "code": "67a7a5ad93b7"},
            {"servers": "nonsense", "code": "67a7a5ad93b7"}]}"#).unwrap();
        let list: Vec<Probe> = feed.get("latency_probes").unwrap().as_arr().unwrap().iter().filter_map(Probe::from_feed).collect();
        assert_eq!(list.len(), 1);
        // A server the feed lists.
        assert_eq!(probe_for(&list, 3005).map(|p| (p.name, p.host)), Some(("Seoul".into(), "192.0.2.10".into())));
        // Beritra: not in the feed's list, the built-in Asia entry.
        assert_eq!(probe_for(&list, 2508).map(|p| p.name), Some("Tokyo".into()));
        // Nobody knows this region's server yet: no ping rather than a wrong one.
        assert_eq!(probe_for(&list, 9001), None);
        assert_eq!(parse_ranges("1501-1510, 2508,9-1"), vec![(1501, 1510), (2508, 2508)]);
    }
}

/// Times a connection to the player's game server once a minute while the
/// game runs, like a ping: the median of four tries, nothing sent. This is the
/// latency players feel (about 80 ms from Malaysia to Tokyo), not the relay's.
fn latency_probe(shared: Arc<Shared>) {
    use std::net::{TcpStream, ToSocketAddrs};
    loop {
        let (running, list, server) = {
            let c = shared.lock();
            let e = &c.pipeline.engine;
            (c.status.game_process.is_some(), c.probes.clone(), e.self_server.or(c.settings.last_server))
        };
        if running {
            match server.and_then(|s| probe_for(&list, s)) {
                // Server not known yet: wait for the game to say.
                None if server.is_none() => {}
                None => {
                    let mut c = shared.lock();
                    c.game_rtt = None;
                    c.ping_note = Some("region");
                }
                Some(p) => {
                    let mut times = Vec::new();
                    if let Some(addr) = (p.host.as_str(), p.port).to_socket_addrs().ok().and_then(|mut a| a.next()) {
                        for i in 0..4 {
                            if i > 0 {
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            let start = Instant::now();
                            if TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
                                times.push(start.elapsed().as_secs_f64() * 1000.0);
                            }
                        }
                    }
                    let mut c = shared.lock();
                    if times.is_empty() {
                        c.ping_note = Some("unreachable");
                    } else {
                        times.sort_by(|a, b| a.total_cmp(b));
                        c.game_rtt = Some((times[times.len() / 2], now_unix(), p.name.clone()));
                        c.ping_note = None;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Downloads the gaming.tools NPC and skill data files at most once a day
/// (conditional requests, so an unchanged file costs almost nothing) and
/// keeps a copy so the meter works offline.
fn site_sync(shared: Arc<Shared>) {
    use meter_core::gamingtools::{entries_from_data, names_from_data, NPCS_DATA, SKILLS_DATA};
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("DaevaMeter/", env!("CARGO_PKG_VERSION"), " (+https://aion2.bajubesi.org)"))
        .build();
    let dir = shared.lock().paths.data.join("gaming-tools");
    let _ = std::fs::create_dir_all(&dir);
    let meta_path = dir.join("meta.json");
    let mut first = true;
    loop {
        if !first {
            // Check every few seconds for "Update names now"; fetch only when due.
            for _ in 0..12 {
                std::thread::sleep(Duration::from_secs(5));
                if shared.lock().site_wake {
                    break;
                }
            }
        }
        let wake = std::mem::take(&mut shared.lock().site_wake);
        let enabled = shared.lock().settings.lookup_names;
        if !enabled {
            first = false;
            continue;
        }
        let mut meta: serde_json::Value = std::fs::read_to_string(&meta_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({}));
        if !meta.is_object() {
            meta = serde_json::json!({});
        }
        let now = now_unix();
        let checked = meta["checked_at"].as_f64().unwrap_or(0.0);
        let retry_at = meta["retry_at"].as_f64().unwrap_or(0.0);
        let due = wake || (now - checked > 24.0 * 3600.0 && now >= retry_at);
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok().and_then(|s| entries_from_data(&s).ok());
        // Use the saved copy straight away on start, before any download, so
        // boss cards get their type and picture even when the copy is old.
        if first {
            let (npcs, skills) = (read("npcs.d.json"), read("skills.d.json"));
            if npcs.is_some() || skills.is_some() {
                let at = meta["checked_at"].as_f64().unwrap_or(now);
                shared.lock().apply_site_bulk(npcs.unwrap_or_default(), skills.unwrap_or_default(), at);
            }
        }
        let have_data = shared.lock().site_bulk.is_some();
        let mut err = None;
        let mut changed = false;
        if due {
            if !have_data {
                shared.lock().site_loading = true;
            }
            let mirror = shared.lock().settings.names_mirror.trim().trim_end_matches('/').to_string();
            let mut problems: Vec<String> = Vec::new();
            for (cdn, file) in [(NPCS_DATA, "npcs.d.json"), (SKILLS_DATA, "skills.d.json")] {
                // Only the Daeva server's mirror: the meter never contacts
                // gaming.tools itself (the server fetches once a day for everyone).
                let _ = cdn;
                let mut sources = Vec::new();
                if mirror.starts_with("https://") {
                    sources.push(format!("{mirror}/{file}"));
                }
                let mut got = false;
                for url in sources {
                    let host = url.split('/').nth(2).unwrap_or("?").to_string();
                    let mut req = agent.get(&url);
                    if meta["etag"][file]["url"].as_str() == Some(url.as_str()) && dir.join(file).exists() {
                        if let Some(tag) = meta["etag"][file]["tag"].as_str() {
                            req = req.set("If-None-Match", tag);
                        }
                    }
                    match req.call() {
                        Ok(r) if r.status() == 304 => got = true,
                        Ok(r) => {
                            let tag = r.header("ETag").map(String::from);
                            let mut body = String::new();
                            use std::io::Read;
                            match r.into_reader().take(64 << 20).read_to_string(&mut body) {
                                Ok(_) => match names_from_data(&body) {
                                    Ok(_) => {
                                        let _ = std::fs::write(dir.join(file), &body);
                                        if !meta["etag"].is_object() {
                                            meta["etag"] = serde_json::json!({});
                                        }
                                        meta["etag"][file] = serde_json::json!({"url": url, "tag": tag});
                                        got = true;
                                        changed = true;
                                    }
                                    Err(e) => problems.push(format!("{host}: {e}")),
                                },
                                Err(e) => problems.push(format!("{host}: {e}")),
                            }
                        }
                        Err(ureq::Error::Status(304, _)) => got = true,
                        Err(ureq::Error::Status(code, _)) => problems.push(format!("{host}: HTTP {code}")),
                        Err(e) => problems.push(format!("{host}: {e}")),
                    }
                    if got {
                        break;
                    }
                }
                if !got {
                    err = Some(format!("Names download failed ({})", problems.join("; ")));
                }
            }
            if err.is_none() {
                meta["checked_at"] = serde_json::json!(now);
                meta["retry_at"] = serde_json::json!(0);
            } else {
                // No data at all yet (fresh install): try again soon.
                meta["retry_at"] = serde_json::json!(now + if have_data { 1800.0 } else { 60.0 });
            }
            let _ = std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap_or_default());
        }
        if due {
            // Parse outside the lock; apply only when something new arrived.
            let fresh = if changed || !have_data { Some((read("npcs.d.json"), read("skills.d.json"))) } else { None };
            let mut c = shared.lock();
            if let Some((npcs, skills)) = fresh {
                if npcs.is_some() || skills.is_some() {
                    let at = meta["checked_at"].as_f64().unwrap_or(now);
                    c.apply_site_bulk(npcs.unwrap_or_default(), skills.unwrap_or_default(), at);
                }
            }
            c.site_loading = false;
            c.lookup_status = err;
        }
        first = false;
    }
}

/// Downloads aion2.gg's boss data (enrage times, boss skill names) from the
/// Daeva server once a day; a downloaded copy replaces the one in the
/// installer. The meter never contacts aion2.gg itself.
fn boss_data_sync(shared: Arc<Shared>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("DaevaMeter/", env!("CARGO_PKG_VERSION"), " (+https://aion2.bajubesi.org)"))
        .build();
    let dir = shared.lock().paths.data.join("aion2gg");
    let meta_path = dir.join("meta.json");
    loop {
        let mirror = shared.lock().settings.names_mirror.trim().trim_end_matches('/').to_string();
        let mut meta: serde_json::Value = std::fs::read_to_string(&meta_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({}));
        if !meta.is_object() {
            meta = serde_json::json!({});
        }
        let now = now_unix();
        // Also due while the skill icon list hasn't arrived yet (meters that
        // fetched the boss data before the server had it), once an hour.
        let due = (now - meta["checked_at"].as_f64().unwrap_or(0.0) > 24.0 * 3600.0 || !dir.join("skill-icons.json").exists()) && now >= meta["retry_at"].as_f64().unwrap_or(0.0);
        // Same server as the names mirror: ".../api/gt" -> ".../api/aion2gg".
        let base = mirror.strip_suffix("/gt").filter(|b| b.starts_with("https://")).map(|b| format!("{b}/aion2gg"));
        if let (true, Some(base)) = (due, base) {
            let _ = std::fs::create_dir_all(&dir);
            let mut ok = true;
            let mut fights_changed = false;
            // skill-icons.json: our icons for skills gaming.tools has none of
            // (a missing list doesn't hold up the boss data).
            for file in ["boss-fights.json", "boss-skills.json", "skill-icons.json"] {
                let optional = file == "skill-icons.json";
                let url = format!("{base}/{file}");
                let mut req = agent.get(&url);
                if dir.join(file).exists() {
                    if let Some(tag) = meta["etag"][file].as_str() {
                        req = req.set("If-None-Match", tag);
                    }
                }
                match req.call() {
                    Ok(r) if r.status() == 304 => {}
                    Ok(r) => {
                        let tag = r.header("ETag").map(String::from);
                        let mut body = String::new();
                        use std::io::Read;
                        let valid = r.into_reader().take(16 << 20).read_to_string(&mut body).is_ok()
                            && if file == "boss-fights.json" { Names::from_aion2gg(&body).is_ok() } else { serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&body).is_ok() };
                        let tmp = dir.join(format!("{file}.part"));
                        if valid && std::fs::write(&tmp, &body).is_ok() && std::fs::rename(&tmp, dir.join(file)).is_ok() {
                            if !meta["etag"].is_object() {
                                meta["etag"] = serde_json::json!({});
                            }
                            meta["etag"][file] = serde_json::json!(tag);
                            fights_changed |= file == "boss-fights.json";
                        } else if !optional {
                            ok = false;
                        }
                    }
                    Err(ureq::Error::Status(304, _)) => {}
                    Err(_) => ok &= optional,
                }
            }
            if ok {
                meta["checked_at"] = serde_json::json!(now);
                meta["retry_at"] = serde_json::json!(0);
            } else {
                meta["retry_at"] = serde_json::json!(now + 1800.0);
            }
            let _ = std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap_or_default());
            if fights_changed {
                let mut c = shared.lock();
                if c.sim.is_none() {
                    c.reload_definitions();
                }
            }
        }
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Looks up unknown monster and skill names on aion2.gaming.tools, one page
/// at a time and never faster than one request every 3 seconds.
fn name_lookup(shared: Arc<Shared>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("DaevaMeter/", env!("CARGO_PKG_VERSION"), " (+https://aion2.bajubesi.org)"))
        .build();
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let Some((key, url, section)) = shared.lock().next_lookup() else { continue };
        let result = match agent.get(&url).call() {
            Ok(r) => r
                .into_string()
                .map(|html| meter_core::gamingtools::parse_name(&html, section))
                .map_err(|e| format!("gaming.tools read failed: {e}")),
            Err(ureq::Error::Status(404, _)) | Err(ureq::Error::Status(410, _)) => Ok(None),
            Err(e) => Err(format!("gaming.tools unreachable: {e}")),
        };
        shared.lock().lookup_result(&key, result);
    }
}

/// Finds the game process and its server connections.
fn detector(shared: Arc<Shared>) {
    loop {
        let (names, ignore): (Vec<String>, Vec<u16>) = {
            let c = shared.lock();
            let mut n: Vec<String> = c.settings.process_names.iter().map(|s| s.to_ascii_lowercase()).collect();
            n.extend(c.pipeline.decoder.defs.process_names.iter().map(|s| s.to_ascii_lowercase()));
            (n, c.settings.ignore_ports.clone())
        };
        let procs: Vec<(u32, String)> = winsys::processes().into_iter().filter(|(_, exe)| {
            let l = exe.to_ascii_lowercase();
            names.iter().any(|n| !n.is_empty() && l.contains(n.as_str()))
        }).collect();
        let pids: HashSet<u32> = procs.iter().map(|p| p.0).collect();
        let mut servers: HashSet<Endpoint> = HashSet::new();
        if !pids.is_empty() {
            for c in winsys::tcp_connections() {
                if pids.contains(&c.pid) && !ignore.contains(&c.remote.port) && !c.remote.ip.is_loopback() {
                    servers.insert(c.remote);
                }
            }
        }
        {
            let mut c = shared.lock();
            c.status.game_process = procs.first().cloned();
            let mut list: Vec<String> = servers.iter().map(|e| e.to_string()).collect();
            list.sort();
            c.status.servers = list;
            if !servers.is_empty() || procs.is_empty() {
                c.pipeline.server_endpoints = servers;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Opens every suitable adapter; restarts when `capture_gen` changes.
fn supervisor(shared: Arc<Shared>, tx: SyncSender<Pkt>) {
    let np = match Npcap::load() {
        Ok(n) => n,
        Err(e) => {
            shared.lock().status.npcap_error = Some(e);
            return;
        }
    };
    shared.lock().status.npcap_version = Some(np.version());
    let mut gen_seen = u64::MAX;
    loop {
        let gen = shared.capture_gen.load(Ordering::SeqCst);
        if gen != gen_seen {
            gen_seen = gen;
            let (filter_dev, ignore) = {
                let c = shared.lock();
                (c.settings.device_filter.to_ascii_lowercase(), c.settings.ignore_ports.clone())
            };
            let mut bpf = String::from("tcp");
            if !ignore.is_empty() {
                let ports: Vec<String> = ignore.iter().map(|p| format!("port {p}")).collect();
                bpf = format!("tcp and not ({})", ports.join(" or "));
            }
            let devices = match np.devices() {
                Ok(d) => d,
                Err(e) => {
                    shared.lock().status.npcap_error = Some(format!("cannot list adapters: {e}"));
                    std::thread::sleep(Duration::from_secs(5));
                    gen_seen = u64::MAX;
                    continue;
                }
            };
            let mut names = Vec::new();
            let mut skipped = Vec::new();
            {
                let mut c = shared.lock();
                c.status.device_errors.clear();
            }
            for d in devices {
                let label = if d.description.is_empty() { d.name.clone() } else { d.description.clone() };
                // VPN tunnels can show no IPv4 address in Npcap's list, so an
                // adapter without one is opened too (filtering is by "tcp").
                let why = if d.loopback {
                    Some("loopback")
                } else if d.disconnected {
                    Some("disconnected")
                } else if !filter_dev.is_empty() && !label.to_ascii_lowercase().contains(&filter_dev) && !d.name.to_ascii_lowercase().contains(&filter_dev) {
                    Some("not the chosen adapter")
                } else {
                    None
                };
                if let Some(why) = why {
                    skipped.push(format!("{label}: {why}"));
                    continue;
                }
                let ips = if d.ipv4.is_empty() { "no IPv4".to_string() } else { d.ipv4.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ") };
                names.push(format!("{label} ({ips})"));
                let np2 = np.clone();
                let tx2 = tx.clone();
                let s2 = shared.clone();
                let bpf2 = bpf.clone();
                std::thread::Builder::new()
                    .name("capture".into())
                    .spawn(move || capture_loop(np2, d.name, label, bpf2, tx2, s2, gen))
                    .ok();
            }
            let mut c = shared.lock();
            c.status.devices = names;
            c.status.devices_skipped = skipped;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn capture_loop(np: Arc<Npcap>, dev: String, label: String, bpf: String, tx: SyncSender<Pkt>, shared: Arc<Shared>, gen: u64) {
    let mut cap = match Capture::open(np, &dev, &bpf) {
        Ok(c) => c,
        Err(e) => {
            shared.lock().status.device_errors.push(format!("{label}: {e}"));
            return;
        }
    };
    let lt = cap.linktype;
    let mut dropped = 0u64;
    loop {
        if shared.capture_gen.load(Ordering::Relaxed) != gen {
            return;
        }
        match cap.next() {
            Ok(Some((ts, data))) => match tx.try_send((lt, ts, data.to_vec())) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    dropped += 1;
                    if dropped % 1000 == 1 {
                        shared.lock().status.dropped += 1000;
                    }
                }
                Err(TrySendError::Disconnected(_)) => return,
            },
            Ok(None) => {}
            Err(e) => {
                shared.lock().status.device_errors.push(format!("{label}: {e}"));
                return;
            }
        }
    }
}

pub fn read_saved_fight(dir: &Path, file: &str, names: &Names) -> Result<String, String> {
    if file.contains('/') || file.contains('\\') || file.contains("..") {
        return Err("bad file name".into());
    }
    std::fs::read_to_string(dir.join(file)).map(|s| fix_names(&s, names)).map_err(|e| e.to_string())
}

/// Replaces "NPC 123" / "Skill 456" strings in a saved fight with names
/// given since it was saved.
pub fn fix_names(src: &str, names: &Names) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(q) = rest.find('"') {
        out.push_str(&rest[..q + 1]);
        rest = &rest[q + 1..];
        let (prefix, is_npc) = if rest.starts_with("NPC ") {
            ("NPC ", true)
        } else if rest.starts_with("Skill ") {
            ("Skill ", false)
        } else {
            continue;
        };
        let digits = rest[prefix.len()..].bytes().take_while(|b| b.is_ascii_digit()).count();
        let after = &rest[prefix.len() + digits..];
        let dot = after.starts_with(" (DOT)\"");
        if digits == 0 || !(after.starts_with('"') || dot) {
            continue;
        }
        let Ok(id) = rest[prefix.len()..prefix.len() + digits].parse::<u32>() else { continue };
        let name = if is_npc { names.npcs.get(&id).cloned() } else { names.skill_named(id).then(|| names.skill(id)) };
        if let Some(n) = name {
            out.push_str(&n.replace('\\', "\\\\").replace('"', "\\\""));
            rest = &rest[prefix.len() + digits..];
        }
    }
    out.push_str(rest);
    out
}

/// The project's GitHub releases. Only this page is ever opened from the
/// update notice.
pub const RELEASES_URL: &str = "https://github.com/fhazal/DAEVA-dps-meter/releases";
const LATEST_API: &str = "https://api.github.com/repos/fhazal/DAEVA-dps-meter/releases/latest";
pub const RELEASES_API: &str = "https://api.github.com/repos/fhazal/DAEVA-dps-meter/releases?per_page=10";

/// Asks GitHub for the latest published release (drafts and pre-releases are
/// never "latest"): shortly after start, then every 6 hours, or on "Check now".
/// Sends nothing about the user; one read-only request.
fn update_check(shared: Arc<Shared>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("DaevaMeter/", env!("CARGO_PKG_VERSION"), " (+https://github.com/fhazal/DAEVA-dps-meter)"))
        .build();
    std::thread::sleep(Duration::from_secs(10));
    let mut next = Instant::now();
    loop {
        let (enabled, wake) = {
            let mut c = shared.lock();
            (c.settings.check_updates, std::mem::take(&mut c.update_wake))
        };
        if wake || (enabled && Instant::now() >= next) {
            let res = agent
                .get(LATEST_API)
                .set("Accept", "application/vnd.github+json")
                .call()
                .map_err(|e| match e {
                    ureq::Error::Status(code, _) => format!("GitHub answered HTTP {code}"),
                    e => e.to_string(),
                })
                .and_then(|r| r.into_string().map_err(|e| e.to_string()))
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| e.to_string()));
            let mut c = shared.lock();
            c.update_checked = now_unix();
            match res {
                Ok(v) => {
                    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
                    let url = v["html_url"].as_str().unwrap_or("").to_string();
                    if !tag.is_empty() && url.starts_with(RELEASES_URL) {
                        c.update_latest = Some((tag, url));
                        c.update_error = None;
                    } else {
                        c.update_error = Some("unexpected answer from GitHub".into());
                    }
                }
                Err(e) => c.update_error = Some(e),
            }
            drop(c);
            next = Instant::now() + Duration::from_secs(6 * 3600);
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod names_tests {
    use super::*;

    #[test]
    fn test_versions_sort_below_their_release() {
        assert!(version_key("0.5.2-test.2") > version_key("0.5.2-test.1"));
        assert!(version_key("v0.5.2") > version_key("0.5.2-test.2"));
        assert!(version_key("0.5.2-test.1") > version_key("v0.5.1"));
        assert!(version_key("0.6.23") > version_key("0.6.9"));
    }

    #[test]
    fn aion2gg_download_replaces_the_bundled_copy_and_ours_wins() {
        let dir = std::env::temp_dir().join(format!("daeva-gg-test-{}", std::process::id()));
        let (res, data) = (dir.join("res"), dir.join("data"));
        std::fs::create_dir_all(res.join("definitions").join("aion2gg")).unwrap();
        std::fs::create_dir_all(data.join("aion2gg")).unwrap();
        std::fs::write(res.join("definitions").join("names.json"), r#"{"npcs": {"2300582": "Vakron"}, "berserk": {"2300582": 300}}"#).unwrap();
        std::fs::write(res.join("definitions").join("aion2gg").join("boss-fights.json"), r#"{"2300582": {"name": "Vakron", "enrage": 210}, "2400001": {"name": "A", "enrage": 300}}"#).unwrap();
        let paths = Paths { data: data.clone(), resources: res, settings: data.join("settings.json"), history: data.join("history"), research: data.join("research") };
        let bundled = load_definitions(&paths, &mut Status::default()).1;
        // A broken download is skipped; a good one replaces the bundled copy.
        std::fs::write(data.join("aion2gg").join("boss-fights.json"), "{").unwrap();
        let broken = load_definitions(&paths, &mut Status::default()).1;
        std::fs::write(data.join("aion2gg").join("boss-fights.json"), r#"{"2400002": {"name": "B", "enrage": 600}}"#).unwrap();
        let fresh = load_definitions(&paths, &mut Status::default()).1;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(bundled.enrage_for(2300582, "Vakron"), Some((300, "community")));
        assert_eq!(bundled.enrage_for(2400001, "A"), Some((300, "aion2.gg")));
        assert_eq!(broken.enrage_for(2400001, "A"), Some((300, "aion2.gg")));
        assert_eq!(fresh.enrage_for(2400001, "A"), None);
        assert_eq!(fresh.enrage_for(2400002, "B"), Some((600, "aion2.gg")));
    }

    #[test]
    fn official_names_win_over_yours() {
        let dir = std::env::temp_dir().join(format!("daeva-names-test-{}", std::process::id()));
        let res = dir.join("res");
        let data = dir.join("data");
        std::fs::create_dir_all(res.join("definitions")).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(res.join("definitions").join("names.json"), r#"{"npcs": {"2300812": "Vakron"}}"#).unwrap();
        std::fs::create_dir_all(res.join("definitions").join("a2tools")).unwrap();
        std::fs::write(res.join("definitions").join("a2tools").join("names.json"), r#"{"npcs": {"2300812": "Other", "2300809": "Eroded Kalgolem"}}"#).unwrap();
        std::fs::write(data.join("gaming-tools-names.json"), r#"{"npcs": {}, "skills": {"17010000": "Earth's Retribution"}}"#).unwrap();
        std::fs::write(data.join("my-names.json"), r#"{"npcs": {"2300812": "Vakorn", "2300899": "Sky Golem"}, "skills": {"17010000": "Earth Hit"}}"#).unwrap();
        let paths = Paths { data: data.clone(), resources: res, settings: data.join("settings.json"), history: data.join("history"), research: data.join("research") };
        let (_, names, official) = load_definitions(&paths, &mut Status::default());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(names.npcs.get(&2300812).map(String::as_str), Some("Vakron"));
        assert_eq!(names.npcs.get(&2300899).map(String::as_str), Some("Sky Golem"), "yours fills the gap");
        assert_eq!(names.skills.get(&17010000).map(String::as_str), Some("Earth's Retribution"));
        assert!(official.npcs.contains(&2300812) && !official.npcs.contains(&2300899));
        // A2Tools only fills ids nobody else names.
        assert_eq!(names.npcs.get(&2300809).map(String::as_str), Some("Eroded Kalgolem"));
        assert!(official.npcs.contains(&2300809));
    }
}

#[cfg(test)]
mod alert_tests {
    use super::*;

    fn alert(key: &str, kind: &'static str, raised: f64, starts: f64) -> Alert {
        Alert { key: key.into(), id: "x".into(), title: "X".into(), body: String::new(), emoji: String::new(), kind, starts, raised }
    }

    #[test]
    fn an_alert_waits_until_the_alert_window_shows_it() {
        let mut ev = EventFeed::new(&crate::settings::Alerts::default());
        ev.push(alert("soon", "soon", 0.0, 300.0));
        ev.push(alert("start", "start", 0.0, 0.0));
        // Not shown yet: still listed long after the old 17 s window.
        assert!(ev.listed(&ev.recent[0], 60.0, 12.0) && ev.listed(&ev.recent[1], 60.0, 12.0));
        assert!(ev.any_pending(60.0));
        // "Starts in" alerts give up once the event starts; starts after 10 min.
        assert!(!ev.listed(&ev.recent[0], 301.0, 12.0));
        assert!(!ev.listed(&ev.recent[1], 601.0, 12.0));
        // Shown: listed only for the banner's own time, nothing pending.
        ev.acked.extend(["soon".to_string(), "start".to_string()]);
        assert!(ev.listed(&ev.recent[0], 10.0, 12.0) && !ev.listed(&ev.recent[0], 18.0, 12.0));
        assert!(!ev.any_pending(60.0));
    }
}
