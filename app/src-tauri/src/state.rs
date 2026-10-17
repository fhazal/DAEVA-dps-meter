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

impl Paths {
    /// User override in the data folder wins over the bundled file, so the
    /// community can ship updated definitions without a new release.
    pub fn definition(&self, file: &str) -> Option<PathBuf> {
        [self.data.join("definitions").join(file), self.resources.join("definitions").join(file)].into_iter().find(|p| p.exists())
    }
}

#[derive(Default)]
pub struct Status {
    pub npcap_version: Option<String>,
    pub npcap_error: Option<String>,
    pub devices: Vec<String>,
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
    health_at: f64,
    health_decoded: u64,
    health_frames: u64,
    /// Frame count last seen and when it last moved (fast stall check).
    stall_frames: u64,
    stall_since: f64,
    health_resyncs: u64,
    pub warning: Option<String>,
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
    /// Picture paths from gaming.tools: "n<template>" / "s<skill>" -> path.
    pub icons: HashMap<String, String>,
    /// Set by "Update names now".
    pub site_wake: bool,
    /// Which edge of the alert window the banners stack from ("top"/"bottom").
    pub toast_anchor: &'static str,
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
    }
}

const ROLLING_SECS: f64 = 180.0;
const ROLLING_MAX_BYTES: usize = 48 << 20;

pub struct Shared {
    pub core: Mutex<Core>,
    pub capture_gen: AtomicU64,
    pub hotkeys: Mutex<Option<winsys::HotkeyThread>>,
}

type Pkt = (u32, f64, Vec<u8>);

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
    EngineConfig { idle_timeout: s.idle_timeout.clamp(2.0, 120.0), merge_summons: s.merge_summons, end_on_target_death: s.end_on_target_death, ..Default::default() }
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
        let (defs, names) = load_definitions(&paths, &mut status);
        let mut pipeline = Pipeline::new(Decoder::new(defs, Some(decompressor())), Engine::new(engine_cfg(&settings), names));
        pipeline.server_ports.extend(settings.manual_ports.iter().copied());
        if !settings.demo {
            if let Some((id, name, at)) = settings.last_self.clone() {
                if now_unix() - at < 12.0 * 3600.0 {
                    pipeline.engine.handle(&meter_core::events::GameEvent::SelfId { id, name });
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
            health_at: 0.0,
            health_decoded: 0,
            health_frames: 0,
            stall_frames: 0,
            stall_since: 0.0,
            health_resyncs: 0,
            warning: None,
            events: EventFeed::new(&Default::default()),
            wanted: Wanted::default(),
            lookup_retry: HashMap::new(),
            lookup_missing: HashSet::new(),
            lookup_status: None,
            site_bulk: None,
            icons: HashMap::new(),
            site_wake: false,
            toast_anchor: "bottom",
        };
        c.load_wanted();
        c.load_site_missing();
        c.events = EventFeed::new(&c.settings.alerts);
        c.pipeline.decoder.keep_recent = 200;
        c.prune_history();
        c
    }

    pub fn reload_definitions(&mut self) {
        let (defs, names) = load_definitions(&self.paths, &mut self.status);
        // A fresh decoder must not be fed the middle of a stream as if it
        // were a frame boundary: restart reassembly so every flow resyncs,
        // and restart the health counters (they count from zero again).
        self.pipeline.reasm.reset();
        self.pipeline.decoder = Decoder::new(defs, Some(decompressor()));
        self.health_frames = 0;
        self.health_decoded = 0;
        self.health_resyncs = 0;
        self.stall_frames = 0;
        self.pipeline.decoder.keep_recent = if self.recorder.is_some() { 400 } else { 200 };
        self.pipeline.server_ports = self.pipeline.decoder.defs.ports.iter().copied().chain(self.settings.manual_ports.iter().copied()).collect();
        if !self.settings.demo {
            self.pipeline.engine.names = names;
        }
    }

    pub fn apply_settings(&mut self, s: Settings) {
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
            let (_, names) = load_definitions(&self.paths, &mut self.status);
            self.pipeline.engine.names = names;
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
        if let Some(sim) = self.sim.as_mut() {
            let evs = sim.step(now);
            for f in self.pipeline.push_events(&evs) {
                self.fight_finished(f);
            }
        }
        if let Some(f) = self.pipeline.tick(now) {
            self.fight_finished(f);
        }
        self.queue_loot();
        self.check_health(now);
        if self.settings.alerts.enabled && !self.events.occurrences.is_empty() {
            let fresh = self.events.notifier.poll(now, &self.events.occurrences);
            if !fresh.is_empty() && self.settings.alerts.sound {
                let kind = if fresh.iter().any(|a| a.kind == "start") { "start" } else { "soon" };
                winsys::play_wav(meter_core::chime::chime_wav(kind, (self.settings.alerts.volume / 100.0) as f32));
            }
            for a in fresh {
                self.events.push(a);
            }
        }
        if self.sim.is_none() {
            let e = &self.pipeline.engine;
            if e.self_server.is_some() && e.self_server != self.settings.last_server {
                self.settings.last_server = e.self_server;
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
            "npc" => {
                e.rename_npc(key, &name);
                self.wanted.npcs.remove(&key);
            }
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

    pub fn icons_view(&self) -> String {
        let icons: serde_json::Map<String, serde_json::Value> = if self.settings.game_art {
            self.icons.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect()
        } else {
            Default::default()
        };
        // Icons come from your Daeva server's copy, never straight from the
        // gaming.tools CDN (their condition: download, don't hotlink).
        let base = format!("{}/img", self.settings.names_mirror.trim().trim_end_matches('/'));
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
                for (sec, kind) in [("npcs", "npc"), ("skills", "skill")] {
                    if let Some(o) = m[sec].as_object() {
                        for (k, v) in o {
                            out.push(Obj::new().set("kind", kind).set("id", k.parse::<u64>().unwrap_or(0)).set("name", v.as_str().unwrap_or("")).build());
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

    pub fn fight_finished(&mut self, f: Fight) {
        let rec = self.pipeline.engine.fight_record(&f);
        if self.sim.is_none() {
            let name = format!("fight-{}-{}.json", f.started_unix as i64, f.id);
            if let Err(e) = std::fs::write(self.paths.history.join(name), rec.to_string()) {
                self.status.last_save_error = Some(e.to_string());
            }
        }
        let sh = &self.settings.share;
        if sh.enabled && !sh.endpoint.is_empty() && sh.scope != "none" && f.duration() >= sh.min_duration {
            if let Some(filtered) = scope_filter(rec, &sh.scope) {
                let body = Obj::new()
                    .set("type", "fight")
                    .set("meter", "daeva-meter")
                    .set("meter_version", env!("CARGO_PKG_VERSION"))
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
            let body = Obj::new().set("type", "loot").set("meter", "daeva-meter").set("meter_version", env!("CARGO_PKG_VERSION")).set("demo", demo).set("record", it).build();
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
        // Fast self-heal: bytes keep arriving but no frame for 15 s means a
        // stream is stuck (lost sync); restart reading from the next packet.
        if self.sim.is_none() {
            let frames = self.pipeline.decoder.frames_total;
            let flowing = self.pipeline.last_game_ts > 0.0 && now - self.pipeline.last_game_ts < 5.0;
            if frames != self.stall_frames || !flowing {
                self.stall_frames = frames;
                self.stall_since = now;
            } else if now - self.stall_since > 15.0 {
                self.pipeline.reasm.reset();
                self.pipeline.decoder.reset_all();
                self.stall_since = now;
            }
        }
        if self.sim.is_some() || now - self.health_at < 60.0 {
            return;
        }
        let d = &self.pipeline.decoder;
        let decoded: u64 = d.stats.values().map(|s| s.decoded).sum();
        let frames = d.frames_total;
        let resyncs = d.resyncs();
        if self.health_at > 0.0 {
            let new_frames = frames.saturating_sub(self.health_frames);
            let new_decoded = decoded.saturating_sub(self.health_decoded);
            let new_resyncs = resyncs.saturating_sub(self.health_resyncs);
            let bytes_flowing = self.pipeline.last_game_ts > 0.0 && now - self.pipeline.last_game_ts < 30.0;
            self.warning = if bytes_flowing && new_frames == 0 {
                Some("Game traffic arrives but can't be read (the game may have been patched). Press Ctrl+Shift+P to send a report.".into())
            } else if new_frames > 1500 && new_decoded == 0 {
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
            .set("meter_version", env!("CARGO_PKG_VERSION"))
            .set("definitions", self.status.defs_name.as_str())
            .set("created_unix", now_unix())
            .set("note", note)
            .set("seconds_of_traffic", self.rolling.back().zip(self.rolling.front()).map(|(b, a)| b.0 - a.0).unwrap_or(0.0))
            .set("self_id", e.self_id)
            .set("self_name", e.self_name.clone())
            .set("snapshot", self.snapshot(None))
            .set("decoder", self.research_view())
            .set("fights", Value::Arr(fights))
            .set("settings", json::parse(&serde_json::to_string(&settings).unwrap_or_else(|_| "{}".into())).unwrap_or(Value::Null))
            .build();
        let stamp = now_unix() as i64;
        let zip = meter_core::zipstore::build(&[("capture.pcap", &pcap), ("report.json", info.to_string_pretty().as_bytes())]);
        let desktop = std::env::var("USERPROFILE").ok().map(|h| PathBuf::from(h).join("Desktop")).filter(|d| d.is_dir());
        let dir = desktop.unwrap_or_else(|| self.paths.data.join("reports"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("DaevaMeter-report-{stamp}.zip"));
        std::fs::write(&path, zip).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
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
            .set("device_errors", Value::Arr(st.device_errors.iter().map(|d| Value::from(d.as_str())).collect()))
            .set("game_process", st.game_process.as_ref().map(|(pid, n)| format!("{n} (pid {pid})")))
            .set("servers", Value::Arr(st.servers.iter().map(|d| Value::from(d.as_str())).collect()))
            .set("packets", p.packets)
            .set("game_packets", p.game_packets)
            .set("game_bytes", p.game_bytes)
            .set("ping_ms", if now - p.ping.last_sample_ts < 10.0 { p.ping.ms() } else { None })
            .set("game_idle", if p.last_game_ts > 0.0 { Some(now - p.last_game_ts) } else { None })
            .set("frames", p.decoder.frames_total)
            .set("resyncs", p.decoder.resyncs())
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
            .set("odyle", e.self_odyle.map(|o| o.0))
            .set("odyle_bonus", e.self_odyle.map(|o| o.1))
            .set("server", {
                let manual = self.settings.server_name.trim();
                if !manual.is_empty() {
                    Some(manual.to_string())
                } else {
                    e.self_server.or(self.settings.last_server).map(|id| e.names.server(id))
                }
            })
            .set("uploads_pending", self.uploads.len())
            .set("version", env!("CARGO_PKG_VERSION"))
            .set("warning", self.warning.clone())
            .build();
        let ui = Obj::new()
            .set("opacity", s.opacity)
            .set("scale", s.scale)
            .set("accent", s.accent.as_str())
            .set("rows", s.rows)
            .set("show_heal", s.show_heal)
            .set("show_target", s.show_target)
            .set("primary_metric", s.primary_metric.as_str())
            .set("anonymize_others", s.anonymize_others)
            .set("click_through", s.click_through)
            .build();
        let alerts = self.alerts_view(now);
        Obj::new().set("status", status).set("ui", ui).set("fight", fight).set("research", rec).set("alerts", alerts).build()
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
            .set("anchor", self.toast_anchor)
            .set("error", self.events.error.clone())
            // Keys per event id, once the game's key counter is decoded.
            .set("keys", match self.pipeline.engine.self_keys {
                Some(k) => Obj::new().set("shugo_festival", k).build(),
                None => Value::Null,
            })
            .set(
                "next",
                Value::Arr(if a.enabled {
                    schedule::upcoming(&self.events.occurrences, now, &self.events.notifier.muted, 2).into_iter().map(|o| schedule::occurrence_json(o, now)).collect()
                } else {
                    vec![]
                }),
            )
            .set("recent", Value::Arr(self.events.recent.iter().filter(|x| now - x.raised < a.banner_secs.max(3.0) + 5.0).map(|x| x.to_json()).collect()))
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
            title: "Spacetime Rift in 5 min".into(),
            body: "Test alert · this is how events will look".into(),
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
                let top = v.get("players").and_then(|a| a.as_arr()).and_then(|a| a.first()).map(|r| r.str_or("name", "?").to_string());
                Some(
                    Obj::new()
                        .set("file", p.file_name()?.to_string_lossy().to_string())
                        .set("started_unix", v.f64_or("started_unix", 0.0))
                        .set("duration", v.f64_or("duration", 0.0))
                        .set("target", v.get("target").and_then(|t| t.get("name")).and_then(|n| n.as_str()).map(String::from))
                        .set("total", v.f64_or("total", 0.0))
                        .set("dps", v.f64_or("dps", 0.0))
                        .set("players", v.get("players").and_then(|a| a.as_arr()).map_or(0, |a| a.len()))
                        .set("top", top)
                        .set("boss_killed", v.bool_or("boss_killed", false))
                        .build(),
                )
            })
            .collect();
        Value::Arr(items)
    }
}

fn load_definitions(paths: &Paths, status: &mut Status) -> (Arc<Definitions>, Names) {
    status.defs_error = None;
    status.names_error = None;
    let defs = match paths.definition("aion2.json") {
        Some(p) => match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Definitions::from_json(&s)) {
            Ok(d) => d,
            Err(e) => {
                status.defs_error = Some(format!("{}: {e}", p.display()));
                Definitions::from_json(FALLBACK_DEFS).unwrap()
            }
        },
        None => {
            status.defs_error = Some("aion2.json not found".into());
            Definitions::from_json(FALLBACK_DEFS).unwrap()
        }
    };
    status.defs_name = format!("{} v{}", defs.name, defs.version);
    let names = match paths.definition("names.json") {
        Some(p) => match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Names::from_json(&s)) {
            Ok(n) => n,
            Err(e) => {
                status.names_error = Some(format!("{}: {e}", p.display()));
                Names::default()
            }
        },
        None => Names::default(),
    };
    let mut names = names;
    // gaming.tools names (used with permission) take precedence over the
    // bundled names.json; your own names (my-names.json) win over both.
    if let Ok(site) = std::fs::read_to_string(paths.data.join("gaming-tools-names.json")).map_err(|e| e.to_string()).and_then(|s| Names::from_json(&s)) {
        names.npcs.extend(site.npcs);
        names.skills.extend(site.skills);
        names.ranks.extend(site.ranks);
    }
    let mine = paths.data.join("my-names.json");
    if mine.exists() {
        match std::fs::read_to_string(&mine).map_err(|e| e.to_string()).and_then(|s| Names::from_json(&s)) {
            Ok(m) => names.merge(m),
            Err(e) => status.names_error = Some(format!("{}: {e}", mine.display())),
        }
    }
    (Arc::new(defs), names)
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
        let mut core = shared.lock();
        for p in batch {
            core.ingest(p);
        }
        if last_tick.elapsed() >= Duration::from_millis(250) {
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
                .and_then(|body| schedule::parse_overview(&body));
            let mut c = shared.lock();
            match result {
                Ok(occs) => {
                    failures = 0;
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
        // Use the saved copy straight away on start; fetch only when due.
        let mut err = None;
        if due {
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
                meta["retry_at"] = serde_json::json!(now + 1800.0);
            }
            let _ = std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap_or_default());
        }
        if due || first {
            let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok().and_then(|s| entries_from_data(&s).ok());
            let (npcs, skills) = (read("npcs.d.json"), read("skills.d.json"));
            let mut c = shared.lock();
            if npcs.is_some() || skills.is_some() {
                let at = meta["checked_at"].as_f64().unwrap_or(now);
                c.apply_site_bulk(npcs.unwrap_or_default(), skills.unwrap_or_default(), at);
            }
            c.lookup_status = err;
        }
        first = false;
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
            {
                let mut c = shared.lock();
                c.status.device_errors.clear();
            }
            for d in devices {
                if d.loopback || d.disconnected || d.ipv4.is_empty() {
                    continue;
                }
                let label = if d.description.is_empty() { d.name.clone() } else { d.description.clone() };
                if !filter_dev.is_empty() && !label.to_ascii_lowercase().contains(&filter_dev) && !d.name.to_ascii_lowercase().contains(&filter_dev) {
                    continue;
                }
                names.push(format!("{label} ({})", d.ipv4.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")));
                let np2 = np.clone();
                let tx2 = tx.clone();
                let s2 = shared.clone();
                let bpf2 = bpf.clone();
                std::thread::Builder::new()
                    .name("capture".into())
                    .spawn(move || capture_loop(np2, d.name, label, bpf2, tx2, s2, gen))
                    .ok();
            }
            shared.lock().status.devices = names;
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
