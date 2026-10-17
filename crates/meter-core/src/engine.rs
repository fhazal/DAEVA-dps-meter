//! Combat engine: turns game events into fights, DPS tables and skill
//! breakdowns.

use crate::events::{flags, ActorKind, GameEvent};
use crate::json::{self, Obj, Value};
use std::collections::{HashMap, VecDeque};

// ---------------------------------------------------------------- names

/// Lookup tables for skills, NPCs, classes and items. Loaded from
/// `names.json` so the community can maintain them.
#[derive(Default, Debug, Clone)]
pub struct Names {
    pub skills: HashMap<u32, String>,
    pub npcs: HashMap<u32, String>,
    pub classes: HashMap<String, String>,
    pub items: HashMap<u32, (String, Option<u32>)>,
    pub grades: HashMap<u32, String>,
    pub zones: HashMap<u32, String>,
}

impl Names {
    pub fn from_json(src: &str) -> Result<Names, String> {
        let v = json::parse(src).map_err(|e| e.to_string())?;
        let mut n = Names::default();
        let ids = |key: &str| -> Vec<(u32, &Value)> {
            v.get(key)
                .and_then(|o| o.as_obj())
                .map(|o| o.iter().filter_map(|(k, x)| json::parse_number_str(k).map(|id| (id as u32, x))).collect())
                .unwrap_or_default()
        };
        for (id, x) in ids("skills") {
            if let Some(s) = x.as_str() {
                n.skills.insert(id, s.to_string());
            }
        }
        for (id, x) in ids("npcs") {
            let name = x.as_str().map(String::from).or_else(|| x.get("name").and_then(|s| s.as_str()).map(String::from));
            if let Some(s) = name {
                n.npcs.insert(id, s);
            }
        }
        for (id, x) in ids("zones") {
            if let Some(s) = x.as_str() {
                n.zones.insert(id, s.to_string());
            }
        }
        for (id, x) in ids("grades") {
            if let Some(s) = x.as_str() {
                n.grades.insert(id, s.to_string());
            }
        }
        for (id, x) in ids("items") {
            match x {
                Value::Str(s) => {
                    n.items.insert(id, (s.clone(), None));
                }
                Value::Obj(_) => {
                    n.items.insert(id, (x.str_or("name", "?").to_string(), x.get("grade").and_then(|g| g.as_u64()).map(|g| g as u32)));
                }
                _ => {}
            }
        }
        if let Some(o) = v.get("classes").and_then(|o| o.as_obj()) {
            for (k, x) in o {
                if let Some(s) = x.as_str() {
                    n.classes.insert(k.clone(), s.to_string());
                }
            }
        }
        Ok(n)
    }

    /// Skill ids often encode a level/rank suffix; fall back to coarser ids.
    pub fn skill(&self, id: u32) -> String {
        if id == 0 {
            return "Auto attack".into();
        }
        for div in [1u32, 10, 100, 1000, 10000] {
            if let Some(s) = self.skills.get(&(id / div)) {
                return s.clone();
            }
        }
        format!("Skill {id}")
    }
    pub fn class(&self, c: &str) -> String {
        self.classes.get(c).cloned().unwrap_or_else(|| c.to_string())
    }
}

// ---------------------------------------------------------------- stats

#[derive(Debug, Clone, Default)]
pub struct SkillStat {
    pub dmg: u64,
    pub hits: u64,
    pub crits: u64,
    pub back: u64,
    pub perfect: u64,
    pub max: u64,
    pub min: u64,
    pub dot: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ActorStat {
    pub dmg: u64,
    pub hits: u64,
    pub crits: u64,
    pub back: u64,
    pub perfect: u64,
    pub max_hit: u64,
    pub max_hit_skill: u32,
    pub heal: u64,
    pub taken: u64,
    pub deaths: u32,
    pub first: f64,
    pub last: f64,
    pub skills: HashMap<u32, SkillStat>,
    pub by_target: HashMap<u64, u64>,
    /// Damage per second bucket from fight start.
    pub timeline: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct ActorInfo {
    pub id: u64,
    pub name: Option<String>,
    pub class: Option<String>,
    pub kind: ActorKind,
    pub npc_id: Option<u32>,
    pub owner: Option<u64>,
    pub max_hp: Option<u64>,
    pub hp: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct LootRecord {
    pub ts: f64,
    pub who: u64,
    pub item_id: u32,
    pub grade: Option<u32>,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct Fight {
    pub id: u64,
    pub start: f64,
    pub end: f64,
    /// Wall-clock unix seconds when the fight started.
    pub started_unix: f64,
    pub stats: HashMap<u64, ActorStat>,
    /// Total damage taken per target.
    pub targets: HashMap<u64, u64>,
    pub deaths: Vec<(f64, u64, Option<u64>)>,
    pub loot: Vec<LootRecord>,
    pub zone: Option<u32>,
    pub boss_killed: bool,
    pub finished: bool,
}

impl Fight {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(1.0)
    }
    pub fn main_target(&self) -> Option<u64> {
        self.targets.iter().max_by_key(|(_, d)| **d).map(|(id, _)| *id)
    }
    pub fn total(&self) -> u64 {
        self.stats.values().map(|s| s.dmg).sum()
    }
}

// ---------------------------------------------------------------- engine

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Seconds without damage before a fight ends.
    pub idle_timeout: f64,
    /// Fights below this total damage are discarded.
    pub min_fight_damage: u64,
    /// Credit summon/pet damage to its owner.
    pub merge_summons: bool,
    /// End the fight right after the main target dies.
    pub end_on_target_death: bool,
    pub history_size: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { idle_timeout: 8.0, min_fight_damage: 1, merge_summons: true, end_on_target_death: true, history_size: 50 }
    }
}

pub struct Engine {
    pub cfg: EngineConfig,
    pub names: Names,
    pub actors: HashMap<u64, ActorInfo>,
    pub self_id: Option<u64>,
    pub self_name: Option<String>,
    pub current: Option<Fight>,
    pub history: VecDeque<Fight>,
    pub zone: Option<u32>,
    pub loot_log: VecDeque<LootRecord>,
    next_id: u64,
    last_ts: f64,
    /// Offset that converts capture timestamps to unix time.
    pub unix_offset: f64,
    pub events_seen: u64,
    /// Total loot events ever seen (loot_log is capped).
    pub loot_seen: u64,
}

impl Engine {
    pub fn new(cfg: EngineConfig, names: Names) -> Self {
        Engine {
            cfg,
            names,
            actors: HashMap::new(),
            self_id: None,
            self_name: None,
            current: None,
            history: VecDeque::new(),
            zone: None,
            loot_log: VecDeque::new(),
            next_id: 1,
            last_ts: 0.0,
            unix_offset: 0.0,
            events_seen: 0,
            loot_seen: 0,
        }
    }

    fn actor(&mut self, id: u64) -> &mut ActorInfo {
        self.actors.entry(id).or_insert(ActorInfo {
            id,
            name: None,
            class: None,
            kind: ActorKind::Unknown,
            npc_id: None,
            owner: None,
            max_hp: None,
            hp: None,
        })
    }

    fn resolve_owner(&self, id: u64) -> u64 {
        if !self.cfg.merge_summons {
            return id;
        }
        let mut cur = id;
        for _ in 0..4 {
            match self.actors.get(&cur).and_then(|a| a.owner) {
                Some(o) if o != cur => cur = o,
                _ => break,
            }
        }
        cur
    }

    fn is_npc(&self, id: u64) -> bool {
        self.actors.get(&id).map_or(false, |a| a.kind == ActorKind::Npc)
    }

    /// Feeds one event. Returns a fight if this event finished one.
    pub fn handle(&mut self, ev: &GameEvent) -> Option<Fight> {
        self.events_seen += 1;
        let mut finished = None;
        match ev {
            GameEvent::Damage { ts, src, tgt, skill, amount, flags: fl } => {
                finished = self.tick(*ts);
                self.last_ts = self.last_ts.max(*ts);
                let attacker = self.resolve_owner(*src);
                // Damage dealt *by* NPCs only counts as damage taken.
                let attacker_is_npc = self.is_npc(attacker);
                let target_is_friendly = self.actors.get(tgt).map_or(false, |a| matches!(a.kind, ActorKind::Player | ActorKind::Summon));
                if *amount == 0 && fl & (flags::EVADE | flags::PARRY | flags::RESIST) == 0 {
                    return finished;
                }
                if attacker_is_npc || target_is_friendly {
                    let t = self.resolve_owner(*tgt);
                    if let Some(f) = self.current.as_mut() {
                        let s = f.stats.entry(t).or_insert_with(|| ActorStat { first: *ts, last: *ts, ..Default::default() });
                        s.taken += amount;
                    }
                    return finished;
                }
                let fight = self.ensure_fight(*ts);
                fight.end = fight.end.max(*ts);
                *fight.targets.entry(*tgt).or_default() += amount;
                let bucket = ((*ts - fight.start).max(0.0)) as usize;
                let s = fight.stats.entry(attacker).or_insert_with(|| ActorStat { first: *ts, ..Default::default() });
                s.last = *ts;
                s.dmg += amount;
                s.hits += 1;
                let crit = fl & flags::CRIT != 0;
                if crit {
                    s.crits += 1;
                }
                if fl & flags::BACK != 0 {
                    s.back += 1;
                }
                if fl & flags::PERFECT != 0 {
                    s.perfect += 1;
                }
                if *amount > s.max_hit {
                    s.max_hit = *amount;
                    s.max_hit_skill = *skill;
                }
                *s.by_target.entry(*tgt).or_default() += amount;
                if bucket < 3600 {
                    if s.timeline.len() <= bucket {
                        s.timeline.resize(bucket + 1, 0);
                    }
                    s.timeline[bucket] += amount;
                }
                let sk = s.skills.entry(*skill).or_default();
                sk.dmg += amount;
                sk.hits += 1;
                if crit {
                    sk.crits += 1;
                }
                if fl & flags::BACK != 0 {
                    sk.back += 1;
                }
                if fl & flags::PERFECT != 0 {
                    sk.perfect += 1;
                }
                if fl & flags::DOT != 0 {
                    sk.dot += 1;
                }
                sk.max = sk.max.max(*amount);
                sk.min = if sk.min == 0 { *amount } else { sk.min.min(*amount) };
            }
            GameEvent::Heal { ts, src, amount, .. } => {
                let healer = self.resolve_owner(*src);
                if let Some(f) = self.current.as_mut() {
                    if !f.finished {
                        let s = f.stats.entry(healer).or_insert_with(|| ActorStat { first: *ts, last: *ts, ..Default::default() });
                        s.heal += amount;
                    }
                }
            }
            GameEvent::Death { ts, target, killer } => {
                let main = self.current.as_ref().and_then(|f| f.main_target());
                if let Some(f) = self.current.as_mut() {
                    f.deaths.push((*ts, *target, *killer));
                    if let Some(s) = f.stats.get_mut(target) {
                        s.deaths += 1;
                    }
                }
                if self.cfg.end_on_target_death && main == Some(*target) && !self.actors.get(target).map_or(false, |a| a.kind == ActorKind::Player) {
                    if let Some(f) = self.current.as_mut() {
                        f.boss_killed = true;
                        f.end = f.end.max(*ts);
                    }
                    finished = self.finish();
                }
            }
            GameEvent::Actor { id, name, class, kind, npc_id, owner, max_hp } => {
                let a = self.actor(*id);
                if name.is_some() {
                    a.name = name.clone();
                }
                if class.is_some() {
                    a.class = class.clone();
                }
                if *kind != ActorKind::Unknown {
                    a.kind = *kind;
                }
                if npc_id.is_some() {
                    a.npc_id = *npc_id;
                }
                if owner.is_some() {
                    a.owner = *owner;
                    if a.kind == ActorKind::Unknown {
                        a.kind = ActorKind::Summon;
                    }
                }
                if max_hp.is_some() {
                    a.max_hp = *max_hp;
                }
            }
            GameEvent::SelfId { id, name } => {
                self.self_id = Some(*id);
                if name.is_some() {
                    self.self_name = name.clone();
                }
                let a = self.actor(*id);
                a.kind = ActorKind::Player;
                if name.is_some() {
                    a.name = name.clone();
                }
            }
            GameEvent::Hp { id, hp, max_hp, .. } => {
                let a = self.actor(*id);
                a.hp = Some(*hp);
                if max_hp.is_some() {
                    a.max_hp = *max_hp;
                }
            }
            GameEvent::Zone { ts, zone_id } => {
                finished = self.finish();
                self.zone = Some(*zone_id);
                self.last_ts = self.last_ts.max(*ts);
            }
            GameEvent::Loot { ts, who, item_id, grade, count } => {
                let grade = grade.or_else(|| self.names.items.get(item_id).and_then(|i| i.1));
                let rec = LootRecord { ts: *ts, who: *who, item_id: *item_id, grade, count: *count };
                if let Some(f) = self.current.as_mut() {
                    f.loot.push(rec.clone());
                } else if let Some(f) = self.history.back_mut() {
                    if *ts - f.end < 30.0 {
                        f.loot.push(rec.clone());
                    }
                }
                self.loot_seen += 1;
                self.loot_log.push_back(rec);
                while self.loot_log.len() > 200 {
                    self.loot_log.pop_front();
                }
            }
            GameEvent::Buff { .. } => {}
        }
        finished
    }

    fn ensure_fight(&mut self, ts: f64) -> &mut Fight {
        if self.current.as_ref().map_or(true, |f| f.finished) {
            let id = self.next_id;
            self.next_id += 1;
            self.current = Some(Fight {
                id,
                start: ts,
                end: ts,
                started_unix: ts + self.unix_offset,
                stats: HashMap::new(),
                targets: HashMap::new(),
                deaths: Vec::new(),
                loot: Vec::new(),
                zone: self.zone,
                boss_killed: false,
                finished: false,
            });
        }
        self.current.as_mut().unwrap()
    }

    /// Call periodically with the current capture time.
    pub fn tick(&mut self, now: f64) -> Option<Fight> {
        let idle = match &self.current {
            Some(f) if !f.finished => now - f.end > self.cfg.idle_timeout,
            _ => false,
        };
        if idle {
            self.finish()
        } else {
            None
        }
    }

    /// Ends the current fight (also used by the "reset" hotkey).
    pub fn finish(&mut self) -> Option<Fight> {
        let mut f = self.current.take()?;
        if f.finished {
            return None;
        }
        f.finished = true;
        // remove pure "taken" entries with no activity to keep history tidy
        f.stats.retain(|_, s| s.dmg > 0 || s.heal > 0 || s.taken > 0);
        if f.total() < self.cfg.min_fight_damage {
            return None;
        }
        self.history.push_back(f.clone());
        while self.history.len() > self.cfg.history_size {
            self.history.pop_front();
        }
        Some(f)
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    // ------------------------------------------------------------ naming

    pub fn actor_name(&self, id: u64) -> String {
        if Some(id) == self.self_id {
            if let Some(n) = &self.self_name {
                return n.clone();
            }
        }
        match self.actors.get(&id) {
            Some(a) => {
                if let Some(n) = &a.name {
                    return n.clone();
                }
                if let Some(nid) = a.npc_id {
                    return self.names.npcs.get(&nid).cloned().unwrap_or_else(|| format!("NPC {nid}"));
                }
                if Some(id) == self.self_id {
                    return "You".into();
                }
                format!("#{id}")
            }
            None if Some(id) == self.self_id => "You".into(),
            None => format!("#{id}"),
        }
    }

    pub fn actor_class(&self, id: u64) -> Option<String> {
        self.actors.get(&id).and_then(|a| a.class.as_ref()).map(|c| self.names.class(c))
    }

    // ------------------------------------------------------------ output

    pub fn fight(&self, id: u64) -> Option<&Fight> {
        if let Some(f) = &self.current {
            if f.id == id {
                return Some(f);
            }
        }
        self.history.iter().find(|f| f.id == id)
    }

    /// The fight the overlay should show: the live one, else the last one.
    pub fn display_fight(&self) -> Option<&Fight> {
        self.current.as_ref().or_else(|| self.history.back())
    }

    /// Overlay table for a fight. `target` restricts totals to one target.
    pub fn fight_table(&self, f: &Fight, target: Option<u64>) -> Value {
        let dur = f.duration();
        let dmg_of = |s: &ActorStat| match target {
            Some(t) => *s.by_target.get(&t).unwrap_or(&0),
            None => s.dmg,
        };
        let total: u64 = f.stats.values().map(dmg_of).sum();
        let mut rows: Vec<(&u64, &ActorStat)> = f.stats.iter().filter(|(_, s)| dmg_of(s) > 0 || s.heal > 0).collect();
        rows.sort_by(|a, b| dmg_of(b.1).cmp(&dmg_of(a.1)).then(b.1.heal.cmp(&a.1.heal)));
        let players: Vec<Value> = rows
            .iter()
            .map(|(id, s)| {
                let d = dmg_of(s);
                Obj::new()
                    .set("id", **id)
                    .set("name", self.actor_name(**id))
                    .set("class", self.actor_class(**id))
                    .set("is_self", Some(**id) == self.self_id)
                    .set("dmg", d)
                    .set("dps", d as f64 / dur)
                    .set("share", if total > 0 { d as f64 / total as f64 } else { 0.0 })
                    .set("hits", s.hits)
                    .set("crit_rate", if s.hits > 0 { s.crits as f64 / s.hits as f64 } else { 0.0 })
                    .set("back_rate", if s.hits > 0 { s.back as f64 / s.hits as f64 } else { 0.0 })
                    .set("perfect_rate", if s.hits > 0 { s.perfect as f64 / s.hits as f64 } else { 0.0 })
                    .set("max_hit", s.max_hit)
                    .set("max_hit_skill", self.names.skill(s.max_hit_skill))
                    .set("heal", s.heal)
                    .set("hps", s.heal as f64 / dur)
                    .set("taken", s.taken)
                    .set("deaths", s.deaths)
                    .build()
            })
            .collect();
        let main = target.or_else(|| f.main_target());
        let target_v = main.map(|t| {
            let a = self.actors.get(&t);
            let hp_pct = a.and_then(|a| match (a.hp, a.max_hp) {
                (Some(h), Some(m)) if m > 0 => Some(h as f64 / m as f64),
                _ => None,
            });
            Obj::new()
                .set("id", t)
                .set("name", self.actor_name(t))
                .set("dmg_taken", *f.targets.get(&t).unwrap_or(&0))
                .set("hp_pct", hp_pct)
                .set("max_hp", a.and_then(|a| a.max_hp))
                .build()
        });
        Obj::new()
            .set("fight_id", f.id)
            .set("live", !f.finished)
            .set("duration", dur)
            .set("started_unix", f.started_unix)
            .set("total", total)
            .set("dps", total as f64 / dur)
            .set("target", target_v)
            .set("boss_killed", f.boss_killed)
            .set("zone", f.zone.map(|z| self.names.zones.get(&z).cloned().unwrap_or_else(|| format!("Zone {z}"))))
            .set("players", Value::Arr(players))
            .build()
    }

    /// Per-skill breakdown and timeline for one actor in one fight.
    pub fn actor_detail(&self, f: &Fight, actor: u64) -> Option<Value> {
        let s = f.stats.get(&actor)?;
        let mut skills: Vec<(&u32, &SkillStat)> = s.skills.iter().collect();
        skills.sort_by(|a, b| b.1.dmg.cmp(&a.1.dmg));
        let dur = f.duration();
        let sk: Vec<Value> = skills
            .iter()
            .map(|(id, k)| {
                Obj::new()
                    .set("id", **id)
                    .set("name", self.names.skill(**id))
                    .set("dmg", k.dmg)
                    .set("dps", k.dmg as f64 / dur)
                    .set("share", if s.dmg > 0 { k.dmg as f64 / s.dmg as f64 } else { 0.0 })
                    .set("hits", k.hits)
                    .set("crit_rate", if k.hits > 0 { k.crits as f64 / k.hits as f64 } else { 0.0 })
                    .set("back_rate", if k.hits > 0 { k.back as f64 / k.hits as f64 } else { 0.0 })
                    .set("perfect_rate", if k.hits > 0 { k.perfect as f64 / k.hits as f64 } else { 0.0 })
                    .set("avg", if k.hits > 0 { k.dmg as f64 / k.hits as f64 } else { 0.0 })
                    .set("max", k.max)
                    .set("min", k.min)
                    .set("dot_hits", k.dot)
                    .build()
            })
            .collect();
        let mut targets: Vec<(&u64, &u64)> = s.by_target.iter().collect();
        targets.sort_by(|a, b| b.1.cmp(a.1));
        let tg: Vec<Value> = targets.iter().take(20).map(|(t, d)| Obj::new().set("id", **t).set("name", self.actor_name(**t)).set("dmg", **d).build()).collect();
        Some(
            Obj::new()
                .set("id", actor)
                .set("name", self.actor_name(actor))
                .set("class", self.actor_class(actor))
                .set("dmg", s.dmg)
                .set("dps", s.dmg as f64 / dur)
                .set("active_time", (s.last - s.first).max(1.0))
                .set("hits", s.hits)
                .set("max_hit", s.max_hit)
                .set("heal", s.heal)
                .set("taken", s.taken)
                .set("skills", Value::Arr(sk))
                .set("targets", Value::Arr(tg))
                .set("timeline", Value::Arr(s.timeline.iter().map(|x| Value::from(*x)).collect()))
                .build(),
        )
    }

    /// Full record of a fight: table, every actor's detail, loot and deaths.
    /// This is what history files and the bot upload contain.
    pub fn fight_record(&self, f: &Fight) -> Value {
        let mut table = self.fight_table(f, None);
        let details: Vec<Value> = f.stats.keys().filter_map(|id| self.actor_detail(f, *id)).collect();
        let loot: Vec<Value> = f.loot.iter().map(|l| self.loot_json(l)).collect();
        let deaths: Vec<Value> = f
            .deaths
            .iter()
            .map(|(ts, t, k)| {
                Obj::new()
                    .set("t", ts - f.start)
                    .set("target", self.actor_name(*t))
                    .set("killer", k.map(|k| self.actor_name(k)))
                    .build()
            })
            .collect();
        if let Value::Obj(m) = &mut table {
            m.insert("format".into(), Value::from("daeva-meter/fight@1"));
            m.insert("details".into(), Value::Arr(details));
            m.insert("loot".into(), Value::Arr(loot));
            m.insert("deaths".into(), Value::Arr(deaths));
            m.insert("self_name".into(), self.self_id.map(|id| self.actor_name(id)).into());
        }
        table
    }

    pub fn loot_json(&self, l: &LootRecord) -> Value {
        let item = self.names.items.get(&l.item_id);
        Obj::new()
            .set("unix", l.ts + self.unix_offset)
            .set("who", self.actor_name(l.who))
            .set("is_self", Some(l.who) == self.self_id)
            .set("item_id", l.item_id)
            .set("item", item.map(|i| i.0.clone()).unwrap_or_else(|| format!("Item {}", l.item_id)))
            .set("grade", l.grade)
            .set("grade_name", l.grade.and_then(|g| self.names.grades.get(&g).cloned()))
            .set("count", l.count)
            .build()
    }

    /// A list of past fights for the history window.
    pub fn history_list(&self) -> Value {
        let items: Vec<Value> = self
            .history
            .iter()
            .rev()
            .map(|f| {
                let top = f.stats.iter().max_by_key(|(_, s)| s.dmg).map(|(id, _)| self.actor_name(*id));
                Obj::new()
                    .set("fight_id", f.id)
                    .set("started_unix", f.started_unix)
                    .set("duration", f.duration())
                    .set("target", f.main_target().map(|t| self.actor_name(t)))
                    .set("total", f.total())
                    .set("dps", f.total() as f64 / f.duration())
                    .set("players", f.stats.values().filter(|s| s.dmg > 0).count())
                    .set("top", top)
                    .set("boss_killed", f.boss_killed)
                    .build()
            })
            .collect();
        Value::Arr(items)
    }

    /// Plain-text summary for pasting into Discord.
    pub fn summary_text(&self, f: &Fight, max_rows: usize) -> String {
        let t = self.fight_table(f, None);
        let dur = f.duration() as u64;
        let target = t.get("target").and_then(|x| x.get("name")).and_then(|x| x.as_str()).unwrap_or("Fight").to_string();
        let mut s = format!(
            "**{}** — {}:{:02} — {} total, {}/s{}\n",
            target,
            dur / 60,
            dur % 60,
            fmt_num(t.f64_or("total", 0.0)),
            fmt_num(t.f64_or("dps", 0.0)),
            if f.boss_killed { " — killed" } else { "" }
        );
        if let Some(rows) = t.get("players").and_then(|p| p.as_arr()) {
            for (i, r) in rows.iter().take(max_rows).enumerate() {
                let class = r.get("class").and_then(|c| c.as_str()).map(|c| format!(" ({c})")).unwrap_or_default();
                s.push_str(&format!(
                    "`{:>2}.` {}{} — {} ({}/s) {:.1}% · crit {:.0}%\n",
                    i + 1,
                    r.str_or("name", "?"),
                    class,
                    fmt_num(r.f64_or("dmg", 0.0)),
                    fmt_num(r.f64_or("dps", 0.0)),
                    r.f64_or("share", 0.0) * 100.0,
                    r.f64_or("crit_rate", 0.0) * 100.0
                ));
            }
        }
        s
    }
}

pub fn fmt_num(n: f64) -> String {
    let a = n.abs();
    if a >= 1e9 {
        format!("{:.2}B", n / 1e9)
    } else if a >= 1e6 {
        format!("{:.2}M", n / 1e6)
    } else if a >= 1e4 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{:.0}", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dmg(ts: f64, src: u64, tgt: u64, skill: u32, amount: u64, fl: u32) -> GameEvent {
        GameEvent::Damage { ts, src, tgt, skill, amount, flags: fl }
    }

    #[test]
    fn fight_lifecycle() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"skills": {"11": "Ferocious Strike"}, "npcs": {"500": "Training Dummy"}}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Dewa".into()) });
        e.handle(&GameEvent::Actor { id: 2, name: Some("Ally".into()), class: Some("Cleric".into()), kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
        e.handle(&GameEvent::Actor { id: 3, name: None, class: None, kind: ActorKind::Summon, npc_id: None, owner: Some(2), max_hp: None });
        e.handle(&GameEvent::Actor { id: 900, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(500), owner: None, max_hp: Some(10000) });
        e.handle(&dmg(10.0, 1, 900, 110, 1000, flags::CRIT));
        e.handle(&dmg(11.0, 1, 900, 110, 500, 0));
        e.handle(&dmg(12.0, 3, 900, 0, 300, 0)); // summon -> owner 2
        e.handle(&dmg(12.5, 900, 1, 0, 50, 0)); // npc hits us
        let f = e.current.as_ref().unwrap();
        assert_eq!(f.stats[&1].dmg, 1500);
        assert_eq!(f.stats[&1].taken, 50);
        assert_eq!(f.stats[&2].dmg, 300);
        let t = e.fight_table(f, None);
        assert_eq!(t.get("target").unwrap().str_or("name", ""), "Training Dummy");
        let rows = t.get("players").unwrap().as_arr().unwrap();
        assert_eq!(rows[0].str_or("name", ""), "Dewa");
        assert!((rows[0].f64_or("crit_rate", 0.0) - 0.5).abs() < 1e-9);
        let d = e.actor_detail(f, 1).unwrap();
        assert_eq!(d.get("skills").unwrap().as_arr().unwrap()[0].str_or("name", ""), "Ferocious Strike");
        // idle timeout ends the fight
        assert!(e.tick(15.0).is_none());
        let done = e.tick(21.0).expect("fight should end");
        assert_eq!(done.total(), 1800);
        assert_eq!(e.history.len(), 1);
        // new damage starts a new fight
        e.handle(&dmg(30.0, 1, 900, 110, 10, 0));
        assert_eq!(e.current.as_ref().unwrap().id, 2);
        // target death ends it immediately
        let f = e.handle(&GameEvent::Death { ts: 31.0, target: 900, killer: Some(1) }).unwrap();
        assert!(f.boss_killed);
        let txt = e.summary_text(&f, 5);
        assert!(txt.contains("Training Dummy"), "{txt}");
        let rec = e.fight_record(&f).to_string();
        assert!(json::parse(&rec).is_ok());
    }
}
