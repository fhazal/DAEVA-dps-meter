//! Combat engine: turns game events into fights, DPS tables and skill
//! breakdowns.

use crate::events::{flags, ActorKind, GameEvent};
use crate::json::{self, Obj, Value};
use std::collections::{HashMap, VecDeque};

// ---------------------------------------------------------------- names

/// Skill keys with this bit set are the periodic (DoT/HoT) part of a skill,
/// listed separately like "Chain of Torment (DOT)".
pub const DOT_KEY: u32 = 1 << 31;

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
    /// Game server (world) number -> name, e.g. Beritra.
    pub servers: HashMap<u32, String>,
    /// NPC template ids that are bosses (fights against them survive pauses).
    pub bosses: std::collections::HashSet<u32>,
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
        for (id, x) in ids("servers") {
            if let Some(s) = x.as_str() {
                n.servers.insert(id, s.to_string());
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
        if let Some(a) = v.get("bosses").and_then(|a| a.as_arr()) {
            n.bosses = a.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).collect();
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
        if id & DOT_KEY != 0 {
            return format!("{} (DOT)", self.skill(id & !DOT_KEY));
        }
        if id == 0 {
            return "Auto attack".into();
        }
        // exact id, then the same skill at its base rank (last digits zeroed),
        // then coarser ids
        self.known_skill(id).unwrap_or_else(|| format!("Skill {id}"))
    }

    fn known_skill(&self, id: u32) -> Option<String> {
        // Player skills are CCSSxxxx: class, skill, then variant digits, so
        // the CCSS0000 entry names every variant.
        let base = if Self::is_player_skill(id) { id / 10000 * 10000 } else { id };
        [id, id / 10 * 10, id / 100 * 100, base, id / 10, id / 100, id / 1000, id / 10000]
            .into_iter()
            .find_map(|c| self.skills.get(&c).cloned())
    }

    pub fn is_player_skill(id: u32) -> bool {
        (10_000_000..20_000_000).contains(&id)
    }

    /// True when `skill()` would return a real name.
    pub fn skill_named(&self, id: u32) -> bool {
        id & DOT_KEY != 0 && self.skill_named(id & !DOT_KEY) || id == 0 || self.known_skill(id).is_some()
    }

    /// The id a user-given name is stored under (covers all variants).
    pub fn skill_name_key(id: u32) -> u32 {
        let id = id & !DOT_KEY;
        if Self::is_player_skill(id) { id / 10000 * 10000 } else { id }
    }

    /// Adds (and overrides with) every name in `other`.
    pub fn merge(&mut self, other: Names) {
        self.skills.extend(other.skills);
        self.npcs.extend(other.npcs);
        self.zones.extend(other.zones);
        self.servers.extend(other.servers);
        self.items.extend(other.items);
        self.bosses.extend(other.bosses);
        for (k, v) in other.classes {
            self.classes.entry(k).or_insert(v);
        }
    }
    /// Name of a game server (world). Built-in ones are seen in real
    /// captures; names.json "servers" can add or override.
    pub fn server(&self, id: u32) -> String {
        const KNOWN: &[(u32, &str)] = &[(2508, "Beritra")];
        self.servers
            .get(&id)
            .cloned()
            .or_else(|| KNOWN.iter().find(|(k, _)| *k == id).map(|(_, n)| n.to_string()))
            .unwrap_or_else(|| format!("Server {id}"))
    }

    pub fn class(&self, c: &str) -> String {
        self.classes.get(c).cloned().unwrap_or_else(|| c.to_string())
    }
}

// ---------------------------------------------------------------- stats

#[derive(Debug, Clone, Default)]
pub struct SkillStat {
    pub dmg: u64,
    /// Damage results (one per swing); rates use this as the denominator.
    pub swings: u64,
    /// Individual hits, counting multi-hit splits.
    pub hits: u64,
    pub crits: u64,
    pub back: u64,
    pub front: u64,
    pub perfect: u64,
    pub double: u64,
    pub parry: u64,
    pub block: u64,
    pub max: u64,
    pub min: u64,
    pub dot: u64,
    /// Number of casts (results grouped by cast id).
    pub casts: u64,
    /// Casts that landed more than one hit.
    pub multi: u64,
    last_cast: u32,
    cast_hits: u32,
    cast_multi: bool,
}

impl SkillStat {
    fn add(&mut self, amount: u64, fl: u32) {
        let n = ((fl >> flags::MULTI_SHIFT) & flags::MULTI_MASK).max(1) as u64;
        self.dmg += amount;
        self.swings += 1;
        self.hits += n;
        for (bit, c) in [
            (flags::CRIT, &mut self.crits),
            (flags::BACK, &mut self.back),
            (flags::FRONT, &mut self.front),
            (flags::PERFECT, &mut self.perfect),
            (flags::DOUBLE, &mut self.double),
            (flags::PARRY, &mut self.parry),
            (flags::BLOCK, &mut self.block),
            (flags::DOT, &mut self.dot),
        ] {
            if fl & bit != 0 {
                *c += 1;
            }
        }
        self.max = self.max.max(amount);
        self.min = if self.min == 0 { amount } else { self.min.min(amount) };
        let cast = fl >> flags::CAST_SHIFT;
        if cast == 0 || cast != self.last_cast {
            self.casts += 1;
            self.last_cast = cast;
            self.cast_hits = 0;
            self.cast_multi = false;
        }
        self.cast_hits += n as u32;
        if self.cast_hits >= 2 && !self.cast_multi {
            self.cast_multi = true;
            self.multi += 1;
        }
    }
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
    /// Same breakdown split per target (for the target selector).
    pub target_skills: HashMap<u64, HashMap<u32, SkillStat>>,
    pub by_target: HashMap<u64, u64>,
    /// Damage taken per attacker.
    pub taken_from: HashMap<u64, u64>,
    /// Healing done, per skill and per target.
    pub heal_skills: HashMap<u32, SkillStat>,
    /// Incoming hits and how they were defended.
    pub taken_hits: u64,
    pub taken_parry: u64,
    pub taken_block: u64,
    pub taken_perfect_block: u64,
    pub healed: HashMap<u64, u64>,
    pub doubles: u64,
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
    /// The local player's id during this fight (ids change per zone).
    pub self_id: Option<u64>,
    /// Names/classes frozen when the fight ends, so history stays readable
    /// after a zone change reuses entity ids.
    pub names: HashMap<u64, (String, Option<String>)>,
    /// Players who fought one of your targets in this fight.
    pub allies: std::collections::HashSet<u64>,
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
    /// Idle timeout while the main target is a living boss (phases,
    /// untargetable windows, wipes-and-rez).
    pub boss_idle_timeout: f64,
    /// Only count other players' damage on targets you have also hit, and
    /// only let your own damage start or extend a fight (open-world farming
    /// next to strangers stays a series of short fights, like Abyss).
    pub only_my_targets: bool,
    /// Seconds to wait after the main target dies before ending the fight.
    pub death_grace: f64,
    pub history_size: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { idle_timeout: 8.0, min_fight_damage: 1, merge_summons: true, end_on_target_death: true, only_my_targets: true, boss_idle_timeout: 60.0, death_grace: 3.0, history_size: 50 }
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
    /// Set when the main target dies: the fight ends at this time unless
    /// damage continues (chain-pulling packs stays one fight).
    pending_end: Option<f64>,
    pub self_cp: Option<u64>,
    /// Your game server number (from effect packets you cause).
    pub self_server: Option<u32>,
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
            pending_end: None,
            self_cp: None,
            self_server: None,
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
            let Some(a) = self.actors.get(&cur) else { break };
            match a.owner {
                Some(o) if o != cur => cur = o,
                _ => {
                    // Summons/objects that only carry their owner's name.
                    if a.kind == ActorKind::Summon {
                        if let Some(n) = &a.name {
                            if let Some(o) = self.owner_by_name(n) {
                                if o != cur {
                                    cur = o;
                                    continue;
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }
        cur
    }

    fn owner_by_name(&self, name: &str) -> Option<u64> {
        if self.self_name.as_deref() == Some(name) {
            return self.self_id;
        }
        match self.actors.values().find(|a| a.kind == ActorKind::Player && a.name.as_deref() == Some(name)) {
            Some(a) => Some(a.id),
            // Every other player in view announces their name when they
            // appear, but the game never announces you to yourself: a
            // named object whose owner isn't anyone visible is yours.
            None => self.self_id,
        }
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
                if src == tgt {
                    return finished;
                }
                // Learn sides from the local player: whatever we hit, or
                // whatever hits us, is an enemy.
                if let Some(me) = self.self_id {
                    let other = if *src == me { Some(*tgt) } else if *tgt == me { Some(*src) } else { None };
                    if let Some(o) = other {
                        let a = self.actor(o);
                        if a.kind == ActorKind::Unknown {
                            a.kind = ActorKind::Npc;
                        }
                    }
                }
                let attacker = self.resolve_owner(*src);
                self.learn_class(attacker, *skill);
                let key = if fl & flags::DOT != 0 { *skill | DOT_KEY } else { *skill };
                // Friendly on friendly: the game reports heals in the same
                // message as damage.
                if self.is_friendly(attacker) && self.is_friendly(self.resolve_owner(*tgt)) {
                    // (class already learned above)
                    self.record_heal(*ts, attacker, *tgt, key, *amount, *fl);
                    return finished;
                }
                // Damage dealt *by* NPCs only counts as damage taken.
                let attacker_is_npc = self.is_npc(attacker);
                let target_is_friendly = self.actors.get(tgt).map_or(false, |a| matches!(a.kind, ActorKind::Player | ActorKind::Summon));
                if *amount == 0 && fl & (flags::EVADE | flags::PARRY | flags::BLOCK) == 0 {
                    return finished;
                }
                if attacker_is_npc || target_is_friendly {
                    let t = self.resolve_owner(*tgt);
                    if let Some(f) = self.current.as_mut() {
                        let s = f.stats.entry(t).or_insert_with(|| ActorStat { first: *ts, last: *ts, ..Default::default() });
                        s.taken += amount;
                        *s.taken_from.entry(attacker).or_default() += amount;
                        if fl & flags::DOT == 0 {
                            s.taken_hits += 1;
                            if fl & flags::PARRY != 0 {
                                s.taken_parry += 1;
                            }
                            if fl & flags::BLOCK != 0 {
                                s.taken_block += 1;
                            }
                            if fl & flags::PERFECT_BLOCK != 0 {
                                s.taken_perfect_block += 1;
                            }
                        }
                    }
                    return finished;
                }
                // Scope: your damage, damage on targets you've hit, and all
                // damage from players who joined you on one of your targets
                // (your party in practice).
                let mut extends = true;
                if self.cfg.only_my_targets {
                    if let Some(me) = self.self_id {
                        let mine = attacker == me;
                        let live = self.current.as_ref().filter(|f| !f.finished);
                        let shared = live.map_or(false, |f| f.stats.get(&me).map_or(false, |s| s.by_target.contains_key(tgt)));
                        let ally = live.map_or(false, |f| f.allies.contains(&attacker));
                        if !mine && !shared && !ally {
                            return finished;
                        }
                        extends = mine || shared;
                        if shared && !mine {
                            if let Some(f) = self.current.as_mut() {
                                f.allies.insert(attacker);
                            }
                        }
                    }
                }
                if extends {
                    self.pending_end = None;
                }
                let fight = self.ensure_fight(*ts);
                if fight.boss_killed && fight.targets.contains_key(tgt) == false {
                    // moved on to a new mob: the kill flag described the old one
                    fight.boss_killed = false;
                }
                if extends {
                    fight.end = fight.end.max(*ts);
                }
                *fight.targets.entry(*tgt).or_default() += amount;
                let bucket = ((*ts - fight.start).max(0.0)) as usize;
                let s = fight.stats.entry(attacker).or_insert_with(|| ActorStat { first: *ts, ..Default::default() });
                s.last = *ts;
                s.dmg += amount;
                // Periodic ticks don't count as hits for the rate columns
                // (same as Abyss, which shows "-" for DoT casts).
                if fl & flags::DOT == 0 {
                    s.hits += 1;
                    if fl & flags::CRIT != 0 {
                        s.crits += 1;
                    }
                    if fl & flags::BACK != 0 {
                        s.back += 1;
                    }
                    if fl & flags::PERFECT != 0 {
                        s.perfect += 1;
                    }
                }
                if *amount > s.max_hit {
                    s.max_hit = *amount;
                    s.max_hit_skill = key;
                }
                *s.by_target.entry(*tgt).or_default() += amount;
                if bucket < 3600 {
                    if s.timeline.len() <= bucket {
                        s.timeline.resize(bucket + 1, 0);
                    }
                    s.timeline[bucket] += amount;
                }
                if fl & flags::DOUBLE != 0 {
                    s.doubles += 1;
                }
                s.skills.entry(key).or_default().add(*amount, *fl);
                s.target_skills.entry(*tgt).or_default().entry(key).or_default().add(*amount, *fl);
            }
            GameEvent::Heal { ts, src, tgt, skill, amount, flags: fl } => {
                let healer = self.resolve_owner(*src);
                self.learn_class(healer, *skill);
                let key = if fl & flags::DOT != 0 { *skill | DOT_KEY } else { *skill };
                self.record_heal(*ts, healer, *tgt, key, *amount, *fl);
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
                    self.pending_end = Some(*ts + self.cfg.death_grace);
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
                // A spawn (npc_id) is authoritative; otherwise only fill in an
                // unknown kind, so guesses never flip a known player/NPC.
                if *kind != ActorKind::Unknown && (a.kind == ActorKind::Unknown || npc_id.is_some() || *kind == ActorKind::Summon) {
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
                // Entity ids change on every zone change / relog, so the
                // latest self message always wins (definitions must only map
                // this event from messages that are about the local player).
                if self.self_id != Some(*id) {
                    self.self_name = None;
                    if self.self_id.is_some() {
                        // Zone change / relog: every entity id is reassigned.
                        finished = self.finish();
                        self.actors.clear();
                        self.pending_end = None;
                    }
                }
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
                } else if a.max_hp.map_or(true, |m| *hp > m) {
                    // Spawned before the meter started: best guess is the
                    // highest HP seen so far.
                    a.max_hp = Some(*hp);
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
            GameEvent::ActorServer { id, server } => {
                if Some(*id) == self.self_id {
                    self.self_server = Some(*server);
                }
            }
            GameEvent::SelfStats { cp } => {
                // Combat power is in the tens of thousands; ignore anything
                // implausible from look-alike messages in some zones.
                if let Some(c) = cp.filter(|c| (1..10_000_000).contains(c)) {
                    self.self_cp = Some(c);
                }
            }
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
                self_id: self.self_id,
                names: HashMap::new(),
                allies: Default::default(),
            });
        }
        self.current.as_mut().unwrap()
    }

    /// Call periodically with the current capture time.
    pub fn tick(&mut self, now: f64) -> Option<Fight> {
        if let Some(t) = self.pending_end {
            if now >= t {
                self.pending_end = None;
                return self.finish();
            }
        }
        let idle = match &self.current {
            Some(f) if !f.finished => {
                let limit = if self.boss_alive(f) { self.cfg.boss_idle_timeout } else { self.cfg.idle_timeout };
                now - f.end > limit
            }
            _ => false,
        };
        if idle {
            self.finish()
        } else {
            None
        }
    }

    /// Player class skills carry the class in their id (18xxxxxx = Chanter,
    /// ...); names.json maps the two-digit prefix to a class name.
    fn learn_class(&mut self, id: u64, skill: u32) {
        if !(10_000_000..20_000_000).contains(&skill) {
            return;
        }
        let key = (skill / 1_000_000).to_string();
        if !self.names.classes.contains_key(&key) {
            return;
        }
        let a = self.actor(id);
        if a.class.is_none() && matches!(a.kind, ActorKind::Player | ActorKind::Unknown) {
            a.class = Some(key);
        }
    }

    fn is_friendly(&self, id: u64) -> bool {
        Some(id) == self.self_id || self.actors.get(&id).map_or(false, |a| matches!(a.kind, ActorKind::Player | ActorKind::Summon))
    }

    fn record_heal(&mut self, ts: f64, healer: u64, tgt: u64, key: u32, amount: u64, fl: u32) {
        if let Some(f) = self.current.as_mut() {
            if !f.finished {
                let s = f.stats.entry(healer).or_insert_with(|| ActorStat { first: ts, last: ts, ..Default::default() });
                s.heal += amount;
                s.heal_skills.entry(key).or_default().add(amount, fl);
                *s.healed.entry(tgt).or_default() += amount;
            }
        }
    }

    /// True while the fight's main target is a known boss that is not dead.
    fn boss_alive(&self, f: &Fight) -> bool {
        let Some(t) = f.main_target() else { return false };
        let Some(a) = self.actors.get(&t) else { return false };
        let is_boss = a.npc_id.map_or(false, |n| self.names.bosses.contains(&n));
        is_boss && !f.boss_killed && a.hp != Some(0)
    }

    /// Ends the current fight (also used by the "reset" hotkey).
    pub fn finish(&mut self) -> Option<Fight> {
        let mut f = self.current.take()?;
        if f.finished {
            return None;
        }
        f.finished = true;
        let ids: Vec<u64> = f
            .stats
            .iter()
            .flat_map(|(id, st)| std::iter::once(*id).chain(st.by_target.keys().copied()).chain(st.taken_from.keys().copied()))
            .chain(f.targets.keys().copied())
            .collect();
        for id in ids {
            let n = (self.actor_name(id), self.actor_class(id));
            f.names.entry(id).or_insert(n);
        }
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
                match a.kind {
                    ActorKind::Player => format!("Player #{id}"),
                    ActorKind::Npc => format!("Mob #{id}"),
                    ActorKind::Summon => format!("Summon #{id}"),
                    ActorKind::Unknown => format!("#{id}"),
                }
            }
            None if Some(id) == self.self_id => "You".into(),
            None => format!("#{id}"),
        }
    }

    /// Name as of a given fight (frozen names win once the fight is over).
    pub fn fname(&self, f: &Fight, id: u64) -> String {
        match f.names.get(&id) {
            Some((n, _)) => n.clone(),
            None => self.actor_name(id),
        }
    }
    pub fn fclass(&self, f: &Fight, id: u64) -> Option<String> {
        match f.names.get(&id) {
            Some((_, c)) => c.clone(),
            None => self.actor_class(id),
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
    /// Names a monster template; fights already frozen with the number
    /// pick the name up too.
    pub fn rename_npc(&mut self, template: u32, name: &str) {
        let old = format!("NPC {template}");
        self.names.npcs.insert(template, name.to_string());
        for f in self.history.iter_mut().chain(self.current.iter_mut()) {
            for (n, _) in f.names.values_mut() {
                if *n == old {
                    *n = name.to_string();
                }
            }
        }
    }

    /// Monster templates and player skills currently on screen with no name.
    pub fn unnamed(&self) -> (Vec<(u32, Option<u64>)>, Vec<(u32, Option<String>)>) {
        let mut npcs: HashMap<u32, Option<u64>> = HashMap::new();
        for a in self.actors.values() {
            if let Some(n) = a.npc_id {
                if !self.names.npcs.contains_key(&n) {
                    let e = npcs.entry(n).or_insert(None);
                    *e = (*e).max(a.max_hp);
                }
            }
        }
        let mut skills: HashMap<u32, Option<String>> = HashMap::new();
        for f in self.history.iter().chain(self.current.iter()) {
            for (actor, st) in &f.stats {
                for &id in st.skills.keys().chain(st.heal_skills.keys()) {
                    let id = id & !DOT_KEY;
                    if Names::is_player_skill(id) && !self.names.skill_named(id) {
                        skills.entry(Names::skill_name_key(id)).or_insert_with(|| self.fclass(f, *actor));
                    }
                }
            }
        }
        (npcs.into_iter().collect(), skills.into_iter().collect())
    }

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
        let mut rows: Vec<(&u64, &ActorStat)> = f.stats.iter().filter(|(_, s)| dmg_of(s) > 0).collect();
        rows.sort_by(|a, b| dmg_of(b.1).cmp(&dmg_of(a.1)).then(b.1.heal.cmp(&a.1.heal)));
        let players: Vec<Value> = rows
            .iter()
            .map(|(id, s)| {
                let d = dmg_of(s);
                Obj::new()
                    .set("id", **id)
                    .set("name", self.fname(f, **id))
                    .set("class", self.fclass(f, **id))
                    .set("is_self", Some(**id) == f.self_id)
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
            let npc_id = a.and_then(|a| a.npc_id);
            Obj::new()
                .set("id", t)
                .set("name", self.fname(f, t))
                .set("npc_id", npc_id)
                .set("named", npc_id.map_or(true, |n| self.names.npcs.contains_key(&n)))
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
        self.actor_detail_for(f, actor, None)
    }

    /// Per-skill breakdown, rates, targets, damage taken and timeline for one
    /// actor. `target` restricts the skill table to hits on that target.
    pub fn actor_detail_for(&self, f: &Fight, actor: u64, target: Option<u64>) -> Option<Value> {
        let s = f.stats.get(&actor)?;
        let empty = HashMap::new();
        let table = match target {
            Some(t) => s.target_skills.get(&t).unwrap_or(&empty),
            None => &s.skills,
        };
        let mut skills: Vec<(&u32, &SkillStat)> = table.iter().collect();
        skills.sort_by(|a, b| b.1.dmg.cmp(&a.1.dmg));
        let dur = f.duration();
        let total: u64 = table.values().map(|k| k.dmg).sum();
        let rate = |n: u64, d: u64| if d > 0 { n as f64 / d as f64 } else { 0.0 };
        let sk: Vec<Value> = skills
            .iter()
            .map(|(id, k)| {
                Obj::new()
                    .set("id", **id)
                    .set("name", self.names.skill(**id))
                    .set("named", self.names.skill_named(**id))
                    .set("dmg", k.dmg)
                    .set("dps", k.dmg as f64 / dur)
                    .set("share", rate(k.dmg, total))
                    .set("hits", k.hits)
                    .set("swings", k.swings)
                    .set("casts", if **id & DOT_KEY != 0 { None } else { Some(k.casts) })
                    .set("multi_rate", rate(k.multi, k.casts))
                    .set("crit_rate", rate(k.crits, k.swings))
                    .set("back_rate", rate(k.back, k.swings))
                    .set("perfect_rate", rate(k.perfect, k.swings))
                    .set("double_rate", rate(k.double, k.swings))
                    .set("front_rate", rate(k.front, k.swings))
                    .set("parry_rate", rate(k.parry, k.swings))
                    .set("avg", if k.swings > 0 { k.dmg as f64 / k.swings as f64 } else { 0.0 })
                    .set("max", k.max)
                    .set("min", k.min)
                    .set("dot_hits", k.dot)
                    .build()
            })
            .collect();
        let sum = |g: fn(&SkillStat) -> u64| -> u64 { table.iter().filter(|(k, _)| **k & DOT_KEY == 0).map(|(_, v)| g(v)).sum() };
        let hits: u64 = table.values().map(|k| k.hits).sum(); // incl. DoT ticks, like Abyss
        let swings = sum(|k| k.swings);
        let mut targets: Vec<(&u64, &u64)> = s.by_target.iter().collect();
        targets.sort_by(|a, b| b.1.cmp(a.1));
        let tg: Vec<Value> = targets.iter().take(30).map(|(t, d)| Obj::new().set("id", **t).set("name", self.fname(f, **t)).set("dmg", **d).build()).collect();
        let mut taken: Vec<(&u64, &u64)> = s.taken_from.iter().collect();
        taken.sort_by(|a, b| b.1.cmp(a.1));
        let tk: Vec<Value> = taken.iter().take(30).map(|(t, d)| Obj::new().set("id", **t).set("name", self.fname(f, **t)).set("dmg", **d).build()).collect();
        let first_hit = target.and(Some(s.first)).unwrap_or(s.first);
        Some(
            Obj::new()
                .set("id", actor)
                .set("name", self.fname(f, actor))
                .set("class", self.fclass(f, actor))
                .set("is_self", Some(actor) == f.self_id)
                .set("cp", if Some(actor) == f.self_id { self.self_cp } else { None })
                .set("target", target)
                .set("dmg", total)
                .set("fight_dmg", s.dmg)
                .set("dps", total as f64 / dur)
                .set("active_time", (s.last - first_hit).max(1.0))
                .set("active_share", ((s.last - s.first).max(1.0) / dur).min(1.0))
                .set("hits", hits)
                .set("casts", sum(|k| k.casts))
                .set("crit_rate", rate(sum(|k| k.crits), swings))
                .set("back_rate", rate(sum(|k| k.back), swings))
                .set("perfect_rate", rate(sum(|k| k.perfect), swings))
                .set("double_rate", rate(sum(|k| k.double), swings))
                .set("front_rate", rate(sum(|k| k.front), swings))
                .set("parry_rate", rate(sum(|k| k.parry), swings))
                .set("taken_hits", s.taken_hits)
                .set("taken_parry_rate", rate(s.taken_parry, s.taken_hits))
                .set("taken_block_rate", rate(s.taken_block, s.taken_hits))
                .set("taken_perfect_block_rate", rate(s.taken_perfect_block, s.taken_hits))
                .set("multi_rate", rate(sum(|k| k.multi), sum(|k| k.casts)))
                .set("max_hit", table.values().map(|k| k.max).max().unwrap_or(0))
                .set("heal", s.heal)
                .set("taken", s.taken)
                .set("skills", Value::Arr(sk))
                .set("targets", Value::Arr(tg))
                .set("taken_from", Value::Arr(tk))
                .set("heal_skills", {
                    let mut hs: Vec<(&u32, &SkillStat)> = s.heal_skills.iter().collect();
                    hs.sort_by(|a, b| b.1.dmg.cmp(&a.1.dmg));
                    Value::Arr(
                        hs.iter()
                            .map(|(id, k)| {
                                Obj::new()
                                    .set("id", **id)
                                    .set("name", self.names.skill(**id))
                                    .set("named", self.names.skill_named(**id))
                                    .set("amount", k.dmg)
                                    .set("share", rate(k.dmg, s.heal))
                                    .set("casts", k.casts)
                                    .set("hits", k.hits)
                                    .set("crit_rate", rate(k.crits, k.swings))
                                    .set("max", k.max)
                                    .set("avg", if k.swings > 0 { k.dmg as f64 / k.swings as f64 } else { 0.0 })
                                    .build()
                            })
                            .collect(),
                    )
                })
                .set("healed", {
                    let mut h: Vec<(&u64, &u64)> = s.healed.iter().collect();
                    h.sort_by(|a, b| b.1.cmp(a.1));
                    Value::Arr(h.iter().take(30).map(|(t, d)| Obj::new().set("id", **t).set("name", self.fname(f, **t)).set("amount", **d).build()).collect())
                })
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
                    .set("target", self.fname(f, *t))
                    .set("killer", k.map(|k| self.fname(f, k)))
                    .build()
            })
            .collect();
        if let Value::Obj(m) = &mut table {
            m.insert("format".into(), Value::from("daeva-meter/fight@1"));
            m.insert("details".into(), Value::Arr(details));
            m.insert("loot".into(), Value::Arr(loot));
            m.insert("deaths".into(), Value::Arr(deaths));
            m.insert("self_name".into(), f.self_id.map(|id| self.fname(f, id)).into());
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
                let top = f.stats.iter().max_by_key(|(_, s)| s.dmg).map(|(id, _)| self.fname(f, *id));
                Obj::new()
                    .set("fight_id", f.id)
                    .set("started_unix", f.started_unix)
                    .set("duration", f.duration())
                    .set("target", f.main_target().map(|t| self.fname(f, t)))
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
    fn boss_fights_survive_pauses() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: None });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1000) });
        e.handle(&dmg(0.0, 1, 50, 18000000, 10, 0));
        assert!(e.tick(30.0).is_none(), "boss phase pause must not end the fight");
        e.handle(&dmg(31.0, 1, 50, 18000000, 10, 0));
        assert_eq!(e.current.as_ref().unwrap().total(), 20);
        assert!(e.tick(95.0).is_some());
    }

    #[test]
    fn sides_are_inferred() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 15203, name: None });
        e.handle(&dmg(1.0, 15203, 6618, 18010340, 1000, 0)); // we hit the mob
        e.handle(&dmg(1.5, 6618, 15203, 1231880, 600, 0)); // mob hits us
        e.handle(&dmg(2.0, 6618, 6618, 1910250, 5, 0)); // self-effect noise
        let f = e.current.as_ref().unwrap();
        let t = e.fight_table(f, None);
        let rows = t.get("players").unwrap().as_arr().unwrap();
        assert_eq!(rows.len(), 1, "{}", t.to_string());
        assert_eq!(rows[0].str_or("name", ""), "You");
        assert_eq!(t.get("target").unwrap().str_or("name", ""), "Mob #6618");
        // relog / zone change: new self id replaces the old one
        e.handle(&GameEvent::SelfId { id: 777, name: None });
        assert_eq!(e.self_id, Some(777));
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
        assert!(e.handle(&GameEvent::Death { ts: 31.0, target: 900, killer: Some(1) }).is_none());
        let f = e.tick(34.5).expect("ends after the death grace period");
        assert!(f.boss_killed);
        let txt = e.summary_text(&f, 5);
        assert!(txt.contains("Training Dummy"), "{txt}");
        let rec = e.fight_record(&f).to_string();
        assert!(json::parse(&rec).is_ok());
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;

    #[test]
    fn user_names_cover_skill_variants() {
        let mut n = Names::default();
        assert!(!n.skill_named(17010230));
        n.skills.insert(Names::skill_name_key(17010230 | DOT_KEY), "Earth's Retribution".into());
        assert_eq!(Names::skill_name_key(17010230), 17010000);
        assert_eq!(n.skill(17011450), "Earth's Retribution");
        assert!(n.skill_named(17010230 | DOT_KEY));
        assert_eq!(n.skill(17020000), "Skill 17020000");
        let mut e = Engine::new(EngineConfig::default(), n);
        let mut f = Fight { id: 1, start: 0.0, end: 1.0, started_unix: 0.0, stats: Default::default(), targets: Default::default(), deaths: vec![], loot: vec![], zone: None, boss_killed: false, finished: true, self_id: None, names: Default::default(), allies: Default::default() };
        f.names.insert(9, ("NPC 2400002".into(), None));
        e.history.push_back(f);
        e.rename_npc(2400002, "Black Claw Manduri Fighter");
        assert_eq!(e.history[0].names[&9].0, "Black Claw Manduri Fighter");
    }
}
