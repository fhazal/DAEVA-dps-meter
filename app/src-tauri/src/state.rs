//! Shared application state and the capture / processing threads.

use crate::npcap::{Capture, Npcap};
use crate::settings::Settings;
use crate::winsys;
use meter_core::engine::{Engine, EngineConfig, Fight, Names};
use meter_core::json::{self, Obj, Value};
use meter_core::net::{self, Endpoint};
use meter_core::pcapfile::PcapWriter;
use meter_core::protocol::{Decoder, Decompressor, Definitions};
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
}

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
        let sim = settings.demo.then(|| Simulator::new(now_unix() as u64));
        if settings.demo {
            pipeline.engine.names = Names::from_json(DEMO_NAMES).unwrap_or_default();
        }
        let c = Core {
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
        };
        c.prune_history();
        c
    }

    pub fn reload_definitions(&mut self) {
        let (defs, names) = load_definitions(&self.paths, &mut self.status);
        self.pipeline.decoder = Decoder::new(defs, Some(decompressor()));
        self.pipeline.decoder.keep_recent = if self.recorder.is_some() { 400 } else { 0 };
        self.pipeline.server_ports = self.pipeline.decoder.defs.ports.iter().copied().chain(self.settings.manual_ports.iter().copied()).collect();
        if !self.settings.demo {
            self.pipeline.engine.names = names;
        }
    }

    pub fn apply_settings(&mut self, s: Settings) {
        let demo_changed = s.demo != self.settings.demo;
        self.pipeline.engine.cfg = engine_cfg(&s);
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
        if let Some(rec) = self.recorder.as_mut() {
            if let Some(seg) = net::parse_frame(lt, &data) {
                if self.pipeline.classify(&seg.flow).is_some() {
                    // Re-encode as a clean Ethernet frame: consistent link type
                    // and no MAC addresses in shared captures.
                    let frame = net::build_frame_ack(&seg.flow, seg.seq, seg.ack, seg.flags, seg.payload);
                    if rec.writer.write(ts, &frame).is_ok() {
                        rec.packets += 1;
                        rec.bytes += seg.payload.len() as u64;
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
        if let Some(r) = self.recorder.as_mut() {
            let _ = r.writer.flush();
        }
        if self.settings_dirty && self.last_settings_save.elapsed() > Duration::from_secs(2) {
            self.save_settings();
        }
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
        self.pipeline.decoder.keep_recent = 0;
        self.pipeline.decoder.recent.clear();
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
            .set("self_cp", e.self_cp)
            .set("uploads_pending", self.uploads.len())
            .set("version", env!("CARGO_PKG_VERSION"))
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
        Obj::new().set("status", status).set("ui", ui).set("fight", fight).set("research", rec).build()
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
                let src = std::fs::read_to_string(p).ok()?;
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

pub fn read_saved_fight(dir: &Path, file: &str) -> Result<String, String> {
    if file.contains('/') || file.contains('\\') || file.contains("..") {
        return Err("bad file name".into());
    }
    std::fs::read_to_string(dir.join(file)).map_err(|e| e.to_string())
}
