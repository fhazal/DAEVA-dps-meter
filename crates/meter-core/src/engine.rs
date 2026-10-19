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
    /// Boss enrage ("berserk") time in seconds from the first hit, by template.
    pub berserk: HashMap<u32, u32>,
    /// Monster rank by template ("Common", "Elite", "Heroic"...).
    pub ranks: HashMap<u32, String>,
    /// Monster family by template (Cogni, Natura, Fera, Varian...).
    pub families: HashMap<u32, String>,
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
        for (id, x) in ids("families") {
            if let Some(s) = x.as_str() {
                n.families.insert(id, s.to_string());
            }
        }
        for (id, x) in ids("ranks") {
            if let Some(s) = x.as_str() {
                n.ranks.insert(id, s.to_string());
            }
        }
        for (id, x) in ids("berserk") {
            if let Some(secs) = x.as_u64().filter(|s| *s > 0 && *s < 24 * 3600) {
                n.berserk.insert(id, secs as u32);
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
        self.berserk.extend(other.berserk);
        self.ranks.extend(other.ranks);
        self.families.extend(other.families);
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
    /// Max HP came from the game (spawn) or a learned template value,
    /// not estimated from health updates.
    pub max_sure: bool,
    /// Damage this actor has taken since the meter first saw it.
    pub taken: u64,
    /// `taken` when its health last went up (heal, reset): damage since
    /// then is what the current health reflects.
    pub taken_base: u64,
    /// Its first real health reading (0x8d00), to tell whether the meter
    /// watched it from (nearly) full health.
    pub first_hp: Option<u64>,
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
    /// The enemy you hit most recently: what the target card follows.
    pub focus: Option<u64>,
    /// First and last time each target was hit.
    pub tspan: HashMap<u64, (f64, f64)>,
    /// Damage on each target per second of the fight (for pausing the boss
    /// clock while nobody can hit the boss).
    pub tsec: HashMap<u64, Vec<u64>>,
    /// Targets' (hp, max_hp, template) frozen when the fight ends, so the
    /// card still shows them after a zone change clears the actors.
    pub tinfo: HashMap<u64, (Option<u64>, Option<u64>, Option<u32>)>,
    /// Frozen "watched from full health" per target (see `watched_whole`).
    pub twatch: HashMap<u64, bool>,
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
    /// Which time DPS is divided by (see [`DpsClock`]).
    pub dps_clock: DpsClock,
}

/// The time a DPS figure is divided by. Meters disagree mostly because they
/// use different clocks, so the overlay says which one is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpsClock {
    /// Damage on the fight's boss or elite ÷ time from the first hit on it to
    /// its last hit (the kill). Falls back to `Fight` when there is no boss.
    Boss,
    /// Each player's damage ÷ their own first-to-last hit.
    Personal,
    /// Everyone's damage ÷ the whole fight, trash and walk-ups included.
    Fight,
}

impl DpsClock {
    pub fn parse(s: &str) -> DpsClock {
        match s {
            "personal" => DpsClock::Personal,
            "fight" => DpsClock::Fight,
            _ => DpsClock::Boss,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            DpsClock::Boss => "boss",
            DpsClock::Personal => "personal",
            DpsClock::Fight => "fight",
        }
    }
}

/// Boss and elite ranks (same rule as the Daeva bot); Common/Normal aren't.
pub fn rank_is_boss_or_elite(rank: &str) -> bool {
    let r: String = rank.to_ascii_lowercase().trim_end_matches("monster").chars().filter(|c| c.is_ascii_lowercase()).collect();
    !(r.is_empty() || r == "common" || r == "normal")
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { idle_timeout: 8.0, min_fight_damage: 1, merge_summons: true, end_on_target_death: true, only_my_targets: true, boss_idle_timeout: 60.0, death_grace: 3.0, history_size: 50, dps_clock: DpsClock::Boss }
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
    /// Your Odyle (base, bonus); only sent by the game when it changes.
    pub self_odyle: Option<(u64, u64)>,
    /// Shugo Festival Reward Keys held.
    pub self_keys: Option<u64>,
    /// Your group members' names (from the game's party list).
    pub party: std::collections::HashSet<String>,
    /// True once the meter has seen a login or zone change, i.e. the game
    /// has introduced everything around you (names, monsters, health).
    pub fresh_view: bool,
    /// The current self id came from the game this run (not restored from
    /// the last session), so a different id means a real zone change.
    self_live: bool,
    /// Highest max HP seen per monster template. A monster that appears
    /// already hurt reports its current HP as "max"; the template's real
    /// maximum is the highest one seen.
    pub npc_max_hp: HashMap<u32, u64>,
    /// Monsters that died, and when: later hits (a damage-over-time tick
    /// after the kill) don't count, as in the game's own Combat Analysis.
    dead_at: HashMap<u64, f64>,
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
            self_odyle: None,
            self_keys: None,
            party: Default::default(),
            fresh_view: false,
            self_live: false,
            npc_max_hp: HashMap::new(),
            dead_at: HashMap::new(),
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
            max_sure: false,
            taken: 0,
            taken_base: 0,
            first_hp: None,
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
            // The game never announces you to yourself, so a named object
            // whose owner isn't visible is yours only while your own name
            // is still unknown. Once it is known, an unmatched name belongs
            // to another player the game hasn't introduced (common in
            // instances): leave it unowned rather than crediting you.
            None if self.self_name.is_none() => self.self_id,
            None => None,
        }
    }

    fn is_npc(&self, id: u64) -> bool {
        self.actors.get(&id).map_or(false, |a| a.kind == ActorKind::Npc)
    }

    /// Feeds one event. Returns a fight if this event finished one.
    /// Your character as remembered from the last session: shows your name
    /// at once, but never counts as having seen a zone load.
    pub fn restore_self(&mut self, id: u64, name: Option<String>) {
        self.handle(&GameEvent::SelfId { id, name });
        self.self_live = false;
    }

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
                // Everything that hit this target, in scope or not (the HP
                // check), except boss mechanics and ticks after a kill.
                let not_damage = !self.is_friendly(*tgt) && (fl & flags::NPC_ONLY != 0 || self.dead_at.get(tgt).map_or(false, |d| *ts > *d));
                if !not_damage {
                    let t = self.actor(*tgt);
                    t.taken = t.taken.saturating_add(*amount);
                }
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
                // A boss mechanic reported like a damage-over-time tick (it
                // heals the boss), or a tick that lands after the kill.
                if fl & flags::NPC_ONLY != 0 || self.dead_at.get(tgt).map_or(false, |d| *ts > *d) {
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
                let boss_down = self.current.as_ref().filter(|f| f.boss_killed).and_then(|f| f.main_target()).map_or(false, |m| {
                    let npc = self.actors.get(&m).and_then(|a| a.npc_id);
                    npc.map_or(false, |n| self.npc_is_boss(n))
                });
                let fight = self.ensure_fight(*ts);
                if fight.boss_killed && extends && !fight.targets.contains_key(tgt) && !boss_down {
                    // moved on to a new mob: the kill flag described the old one
                    // (a boss kill stands, and so does one when only someone
                    // else's hit landed on another monster: a stray hit after
                    // a world boss died used to cancel "killed")
                    fight.boss_killed = false;
                }
                if extends {
                    fight.end = fight.end.max(*ts);
                }
                *fight.targets.entry(*tgt).or_default() += amount;
                let span = fight.tspan.entry(*tgt).or_insert((*ts, *ts));
                span.1 = span.1.max(*ts);
                let sec = ((*ts - fight.start).max(0.0)) as usize;
                if sec < 3600 {
                    let v = fight.tsec.entry(*tgt).or_default();
                    if v.len() <= sec {
                        v.resize(sec + 1, 0);
                    }
                    v[sec] += amount;
                }
                if fight.self_id.is_some() && fight.self_id == Some(attacker) {
                    fight.focus = Some(*tgt);
                }
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
                if !self.actors.get(target).map_or(false, |a| matches!(a.kind, ActorKind::Player | ActorKind::Summon)) {
                    self.dead_at.insert(*target, *ts);
                }
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
                if npc_id.is_some() {
                    self.dead_at.remove(id); // a new spawn
                }
                let a = self.actor(*id);
                if name.is_some() {
                    a.name = name.clone();
                }
                if class.is_some() {
                    a.class = class.clone();
                }
                // A spawn (npc_id) is authoritative; otherwise only fill in an
                // unknown kind, so guesses never flip a known player/NPC.
                // Exception: an actor guessed to be a monster (no spawn seen)
                // that turns out to be a player, e.g. a party member whose
                // first action was a 7-digit item or basic skill. Left as a
                // monster, their damage vanished and heals on them counted as
                // the healer's damage.
                let player_after_guess = *kind == ActorKind::Player && a.kind == ActorKind::Npc && a.npc_id.is_none();
                if *kind != ActorKind::Unknown && (a.kind == ActorKind::Unknown || npc_id.is_some() || *kind == ActorKind::Summon || player_after_guess) {
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
                    a.max_sure = true;
                }
                if let (Some(n), Some(m), true) = (a.npc_id, a.max_hp, a.max_sure) {
                    let best = self.npc_max_hp.entry(n).or_insert(0);
                    *best = (*best).max(m);
                    let best = *best;
                    let a = self.actor(*id);
                    if best > m {
                        // Seen at full health before: use the real maximum.
                        a.max_hp = Some(best);
                    }
                }
            }
            GameEvent::SelfId { id, name } => {
                // Entity ids change on every zone change / relog, so the
                // latest self message always wins (definitions must only map
                // this event from messages that are about the local player).
                if self.self_id != Some(*id) {
                    // Keep your name across zone changes (only the entity id
                    // changes); a different character's name replaces it as
                    // soon as the game sends it.
                    if !self.self_live {
                        // Replacing an id remembered from the last session:
                        // not a zone change, the game just hasn't reloaded.
                        if let Some(old) = self.self_id {
                            self.actors.remove(&old);
                            // A different id may be a different character:
                            // don't show the remembered name until the game
                            // says who you are (zone load, self-buff, party).
                            self.self_name = None;
                        }
                    } else if self.self_id.is_some() {
                        self.fresh_view = true;
                        // Zone change / relog: every entity id is reassigned.
                        finished = self.finish();
                        self.actors.clear();
                        self.pending_end = None;
                    }
                }
                self.self_id = Some(*id);
                self.self_live = true;
                if name.is_some() {
                    self.self_name = name.clone();
                }
                let keep = self.self_name.clone();
                let a = self.actor(*id);
                a.kind = ActorKind::Player;
                if name.is_some() {
                    a.name = name.clone();
                } else if a.name.is_none() {
                    a.name = keep;
                }
            }
            GameEvent::Hp { id, hp, max_hp, .. } => {
                let learned = {
                    let a = self.actors.get(id);
                    a.and_then(|a| a.npc_id).and_then(|n| self.npc_max_hp.get(&n).copied())
                };
                if *hp > 0 {
                    self.dead_at.remove(id);
                }
                let a = self.actor(*id);
                // Spawns report health as full whatever it really is, so
                // only a real health update counts as the first reading.
                if a.first_hp.is_none() && max_hp.is_none() {
                    a.first_hp = Some(*hp);
                }
                if a.hp.map_or(true, |old| *hp > old) {
                    // Healed or reset (training dummies refill at 1 HP).
                    a.taken_base = a.taken;
                }
                a.hp = Some(*hp);
                if let Some(m) = max_hp {
                    // A spawn of an already-hurt monster reports its health
                    // then; the type's highest value seen is the real maximum.
                    a.max_hp = Some((*m).max(learned.unwrap_or(0)));
                    a.max_sure = true;
                } else if !a.max_sure {
                    if let Some(l) = learned {
                        // This monster type's real maximum, seen at a spawn.
                        a.max_hp = Some(l);
                        a.max_sure = true;
                    } else if *hp <= 1 && a.max_hp.is_none() {
                        // "1 HP" is a floor (dummies, invulnerable phases):
                        // no basis for a maximum yet.
                    } else {
                        // Never seen at full health: its maximum is at least
                        // the health now plus the damage it has taken since
                        // we started watching it.
                        // (Only your own and nearby damage is visible, so this
                        // can only under-estimate: keep the highest.)
                        // Estimate once, from the first real health update;
                        // after that only a higher health reading raises it
                        // (later estimates drift up with update lag).
                        if a.max_hp.is_none() {
                            a.max_hp = Some(hp.saturating_add(a.taken - a.taken_base));
                        }
                    }
                }
                if let Some(m) = a.max_hp {
                    if *hp > m {
                        a.max_hp = Some(*hp);
                    }
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
            GameEvent::Keys { cur } => self.self_keys = Some(*cur),
            GameEvent::Party { names } => self.party = names.iter().cloned().collect(),
            GameEvent::Fresh => self.fresh_view = true,
            GameEvent::Odyle { cur, bonus } => {
                let (c0, b0) = self.self_odyle.unwrap_or((0, 0));
                let c = cur.unwrap_or(c0);
                let b = bonus.unwrap_or(b0);
                if c <= 100_000 && b <= 100_000 {
                    self.self_odyle = Some((c, b));
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
                focus: None,
                tspan: Default::default(),
                tsec: Default::default(),
                tinfo: Default::default(),
                twatch: Default::default(),
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

    /// HP check: once a monster is dead, the damage the meter saw on it as a
    /// share of its max HP (1.0 = every hit counted).
    pub fn hp_counted(&self, f: &Fight, t: u64) -> Option<f64> {
        let a = self.actors.get(&t);
        let frozen = f.tinfo.get(&t).copied();
        // Only bosses and elites: in the open world other players often hit
        // trash before you do, so a check there would only confuse.
        let npc = a.and_then(|a| a.npc_id).or(frozen.and_then(|x| x.2))?;
        if !self.npc_is_boss(npc) {
            return None;
        }
        let hp = a.and_then(|a| a.hp).or(frozen.and_then(|x| x.0));
        let max_hp = a.and_then(|a| a.max_hp).or(frozen.and_then(|x| x.1))?;
        let dead = hp == Some(0) || (f.boss_killed && Some(t) == f.main_target());
        // All damage the meter saw on it, including hits from before you
        // joined in (which don't count towards anyone's DPS here), so the
        // check measures what was captured, not who was counted.
        let seen = a.map(|a| a.taken).unwrap_or(0).max(*f.targets.get(&t).unwrap_or(&0));
        (dead && max_hp > 0).then(|| seen as f64 / max_hp as f64)
    }

    /// Whether the meter saw this monster from (nearly) full health: its
    /// first health reading was at least 95% of a known maximum.
    fn watched_from_full(a: &ActorInfo) -> Option<bool> {
        let (first, max) = (a.first_hp?, a.max_hp?);
        if !a.max_sure || max == 0 {
            return None;
        }
        Some(first as f64 >= 0.95 * max as f64)
    }

    /// Whether the meter watched this target's whole fight: seen from
    /// (nearly) full health. With a kill and no lost traffic that means
    /// every one of your hits on it was recorded, even in the open world
    /// where the game only sends the hits of players near you.
    pub fn watched_whole(&self, f: &Fight, t: u64) -> Option<bool> {
        f.twatch.get(&t).copied().or_else(|| self.actors.get(&t).and_then(Self::watched_from_full))
    }

    /// Whether an NPC id is a boss or elite (definitions or monster rank).
    pub fn npc_is_boss(&self, npc: u32) -> bool {
        self.names.bosses.contains(&npc) || self.names.ranks.get(&npc).map_or(false, |r| rank_is_boss_or_elite(r))
    }

    /// The boss or elite that took the most damage in this fight, if any.
    pub fn boss_target(&self, f: &Fight) -> Option<u64> {
        f.targets
            .iter()
            .filter(|(t, _)| {
                let npc = self.actors.get(t).and_then(|a| a.npc_id).or(f.tinfo.get(t).and_then(|x| x.2));
                npc.map_or(false, |n| self.npc_is_boss(n))
            })
            .max_by_key(|(_, d)| **d)
            .map(|(t, _)| *t)
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
        for t in f.targets.keys() {
            if let Some(a) = self.actors.get(t) {
                f.tinfo.insert(*t, (a.hp, a.max_hp, a.npc_id));
                if let Some(w) = Self::watched_from_full(a) {
                    f.twatch.insert(*t, w);
                }
            }
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
                    // "???" is how the game data marks unnamed objects.
                    let known = self.names.npcs.get(&nid).filter(|n| n.trim() != "???").cloned();
                    return known.unwrap_or_else(|| if a.kind == ActorKind::Summon { "Summon".to_string() } else { format!("NPC {nid}") });
                }
                if Some(id) == self.self_id {
                    return "You".into();
                }
                match a.kind {
                    ActorKind::Player => format!("Player #{id}"),
                    // Spawned before the meter started (or before you arrived),
                    // so the game never said which monster it is.
                    ActorKind::Npc => "Unknown monster".to_string(),
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
        // The clock. Boss fight (the default when a boss or elite is in the
        // fight): damage on every enemy from the first hit on the boss to the
        // kill, adds included, divided by that time, like an encounter in
        // Details! or ACT. Trash and walking before the pull are left out.
        let boss = if self.cfg.dps_clock == DpsClock::Boss && target.is_none() { self.boss_target(f) } else { None };
        let window = boss.and_then(|b| f.tspan.get(&b).copied());
        // Seconds of the fight timeline inside the boss fight, minus phases of
        // 10 s or more when nobody damaged the boss (barriers, untargetable
        // phases, run-backs): those count neither time nor damage.
        let span = window.map(|(a, z)| (((a - f.start).max(0.0)) as usize, ((z - f.start).max(0.0)) as usize));
        let live: Vec<bool> = match (boss, span) {
            (Some(b), Some((i, j))) => boss_live_seconds(f.tsec.get(&b).map(|v| v.as_slice()).unwrap_or(&[]), i, j),
            _ => vec![],
        };
        let paused = live.iter().filter(|x| !**x).count() as f64;
        let (clock, dur) = match (self.cfg.dps_clock, target, window) {
            (_, Some(t), _) => ("target", f.tspan.get(&t).map_or(f.duration(), |(a, z)| (z - a).max(1.0))),
            (DpsClock::Boss, None, Some((a, z))) => ("boss", (z - a - paused).max(1.0)),
            (DpsClock::Personal, None, _) => ("personal", f.duration()),
            _ => ("fight", f.duration()),
        };
        let personal = clock == "personal";
        let own_dur = |s: &ActorStat| if personal { (s.last - s.first).max(1.0) } else { dur };
        let dmg_of = |s: &ActorStat| match (target, span) {
            (Some(t), _) => *s.by_target.get(&t).unwrap_or(&0),
            (None, Some((i, _))) if clock == "boss" => live.iter().enumerate().filter(|(_, l)| **l).map(|(k, _)| s.timeline.get(i + k).copied().unwrap_or(0)).sum(),
            _ => s.dmg,
        };
        let total: u64 = f.stats.values().map(dmg_of).sum();
        let mut rows: Vec<(&u64, &ActorStat)> = f.stats.iter().filter(|(_, s)| dmg_of(s) > 0).collect();
        // Highest first by the number shown: damage under a shared clock (the
        // same order as DPS); under "your time" each player has their own
        // divisor, so rank by DPS or a late joiner's higher DPS sits below.
        if personal {
            rows.sort_by(|a, b| (dmg_of(b.1) as f64 / own_dur(b.1)).total_cmp(&(dmg_of(a.1) as f64 / own_dur(a.1))).then(b.1.heal.cmp(&a.1.heal)));
        } else {
            rows.sort_by(|a, b| dmg_of(b.1).cmp(&dmg_of(a.1)).then(b.1.heal.cmp(&a.1.heal)));
        }
        let players: Vec<Value> = rows
            .iter()
            .map(|(id, s)| {
                let d = dmg_of(s);
                Obj::new()
                    .set("id", **id)
                    .set("name", self.fname(f, **id))
                    .set("class", self.fclass(f, **id))
                    .set("is_self", Some(**id) == f.self_id)
                    .set("in_party", Some(**id) == f.self_id || self.party.contains(&self.fname(f, **id)))
                    .set("dmg", d)
                    .set("dps", d as f64 / own_dur(s))
                    .set("share", if total > 0 { d as f64 / total as f64 } else { 0.0 })
                    .set("hits", s.hits)
                    .set("crit_rate", if s.hits > 0 { s.crits as f64 / s.hits as f64 } else { 0.0 })
                    .set("back_rate", if s.hits > 0 { s.back as f64 / s.hits as f64 } else { 0.0 })
                    .set("perfect_rate", if s.hits > 0 { s.perfect as f64 / s.hits as f64 } else { 0.0 })
                    .set("max_hit", s.max_hit)
                    .set("max_hit_skill", self.names.skill(s.max_hit_skill))
                    .set("heal", s.heal)
                    .set("hps", s.heal as f64 / own_dur(s))
                    .set("taken", s.taken)
                    .set("deaths", s.deaths)
                    .build()
            })
            .collect();
        // The card follows the enemy you hit last; the biggest target only
        // when you haven't hit anything yourself (spectating a fight).
        let main = target.or(f.focus).or_else(|| f.main_target());
        let target_v = main.map(|t| {
            let a = self.actors.get(&t);
            let frozen = f.tinfo.get(&t).copied();
            let hp = a.map(|a| a.hp).or(frozen.map(|x| x.0)).flatten();
            let max_hp = a.map(|a| a.max_hp).or(frozen.map(|x| x.1)).flatten();
            let hp_pct = match (hp, max_hp) {
                (Some(h), Some(m)) if m > 0 => Some(h as f64 / m as f64),
                _ => None,
            };
            let npc_id = a.and_then(|a| a.npc_id).or(frozen.and_then(|x| x.2));
            // HP check: once it's dead, how much of its health the meter saw.
            let counted = self.hp_counted(f, t);
            Obj::new()
                .set("id", t)
                .set("counted", counted)
                .set("watched", if counted.is_some() { self.watched_whole(f, t) } else { None })
                .set("name", self.fname(f, t))
                .set("npc_id", npc_id)
                .set("named", npc_id.map_or(true, |n| self.names.npcs.contains_key(&n)))
                .set("dmg_taken", *f.targets.get(&t).unwrap_or(&0))
                .set("secs", f.tspan.get(&t).map(|(a, b)| (b - a).max(1.0)))
                .set("hp_pct", hp_pct)
                .set("max_hp", max_hp)
                .set("hp", hp)
                .set("berserk", npc_id.and_then(|n| self.names.berserk.get(&n).copied()))
                .set("rank", npc_id.and_then(|n| self.names.ranks.get(&n).cloned()))
                .set("family", npc_id.and_then(|n| self.names.families.get(&n).cloned()))
                .build()
        });
        Obj::new()
            .set("fight_id", f.id)
            .set("live", !f.finished)
            .set("duration", f.duration())
            .set("clock", clock)
            .set("clock_secs", dur)
            .set("clock_paused", if clock == "boss" { paused } else { 0.0 })
            .set("clock_target", if clock == "boss" { boss } else { None })
            .set("clock_npc", if clock == "boss" { boss.and_then(|t| self.actors.get(&t).and_then(|a| a.npc_id).or(f.tinfo.get(&t).and_then(|x| x.2))) } else { None })
            .set("clock_target_name", if clock == "boss" { boss.map(|t| self.fname(f, t)) } else { None })
            // HP check for the boss itself: its damage against its max HP, once dead.
            .set("clock_counted", if clock == "boss" { boss.and_then(|b| self.hp_counted(f, b)) } else { None })
            // ...and whether the meter watched it from (nearly) full health.
            .set("clock_watched", if clock == "boss" { boss.and_then(|b| self.watched_whole(f, b)) } else { None })
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
        // Same clock as the overlay (which opens this with its boss selected).
        let clock_dur = match (self.cfg.dps_clock, target) {
            (DpsClock::Boss, Some(t)) => f.tspan.get(&t).map_or(f.duration(), |(a, z)| (z - a).max(1.0)),
            (DpsClock::Personal, _) => (s.last - s.first).max(1.0),
            _ => f.duration(),
        };
        let empty = HashMap::new();
        let table = match target {
            Some(t) => s.target_skills.get(&t).unwrap_or(&empty),
            None => &s.skills,
        };
        let mut skills: Vec<(&u32, &SkillStat)> = table.iter().collect();
        skills.sort_by(|a, b| b.1.dmg.cmp(&a.1.dmg));
        let dur = clock_dur;
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
        let items: Vec<Value> = self.history.iter().rev().map(|f| history_row(&self.fight_table(f, None))).collect();
        Value::Arr(items)
    }

    /// Plain-text summary for pasting into Discord.
    pub fn summary_text(&self, f: &Fight, max_rows: usize) -> String {
        let t = self.fight_table(f, None);
        // The time shown is the one DPS was divided by.
        let dur = t.f64_or("clock_secs", f.duration()) as u64;
        let target = t.get("clock_target_name").and_then(|x| x.as_str()).or_else(|| t.get("target").and_then(|x| x.get("name")).and_then(|x| x.as_str())).unwrap_or("Fight").to_string();
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

/// Seconds stops counting after this long without damage on the boss.
pub const BOSS_PAUSE_SECS: usize = 10;

/// For each second from `i` to `j` of the fight: false inside a stretch of
/// [`BOSS_PAUSE_SECS`] or more with no damage on the boss.
pub fn boss_live_seconds(tsec: &[u64], i: usize, j: usize) -> Vec<bool> {
    let n = j.saturating_sub(i) + 1;
    let mut live = vec![true; n];
    let mut k = 0;
    while k < n {
        if tsec.get(i + k).copied().unwrap_or(0) == 0 {
            let start = k;
            while k < n && tsec.get(i + k).copied().unwrap_or(0) == 0 {
                k += 1;
            }
            if k - start >= BOSS_PAUSE_SECS {
                live[start..k].iter_mut().for_each(|x| *x = false);
            }
        } else {
            k += 1;
        }
    }
    live
}

/// One History row from a fight table (live or loaded from a saved file).
/// DPS figures follow the clock the fight was recorded with.
pub fn history_row(t: &Value) -> Value {
    let players = t.get("players").and_then(|a| a.as_arr()).cloned().unwrap_or_default();
    let me = players.iter().find(|p| p.bool_or("is_self", false));
    let target = t.get("clock_target_name").and_then(|x| x.as_str()).map(String::from).or_else(|| t.get("target").and_then(|x| x.get("name")).and_then(|x| x.as_str()).map(String::from));
    Obj::new()
        .set("fight_id", t.get("fight_id").and_then(|x| x.as_u64()))
        .set("started_unix", t.f64_or("started_unix", 0.0))
        .set("duration", t.f64_or("duration", 0.0))
        .set("target", target)
        .set("total", t.f64_or("total", 0.0))
        .set("dps", t.f64_or("dps", 0.0))
        .set("players", players.len())
        .set("top", players.first().map(|r| r.str_or("name", "?").to_string()))
        .set("mine", me.map(|r| r.f64_or("dps", 0.0)))
        .set("self_name", me.map(|r| r.str_or("name", "").to_string()))
        .set("clock", t.get("clock").and_then(|x| x.as_str()).map(String::from))
        .set("clock_npc", t.get("clock_npc").and_then(|x| x.as_u64()))
        .set("clock_secs", t.f64_or("clock_secs", t.f64_or("duration", 0.0)))
        .set("boss_killed", t.bool_or("boss_killed", false))
        .build()
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
    fn dps_clocks_and_hp_check() {
        let names = Names::from_json(r#"{"bosses": ["777"]}"#).unwrap();
        let run = |clock: DpsClock| {
            let mut e = Engine::new(EngineConfig { dps_clock: clock, ..Default::default() }, names.clone());
            e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
            e.handle(&GameEvent::Actor { id: 2, name: Some("Late".into()), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
            e.handle(&GameEvent::Actor { id: 40, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(5), owner: None, max_hp: Some(500) });
            e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(2000) });
            e.handle(&dmg(0.0, 1, 40, 18000000, 300, 0)); // trash first
            e.handle(&dmg(4.0, 1, 40, 18000000, 200, 0));
            e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(300) });
            e.handle(&dmg(10.0, 1, 50, 18000000, 1000, 0)); // boss: 10 s → 20 s
            e.handle(&dmg(12.0, 1, 60, 18000000, 200, 0)); // an add the boss summoned
            e.handle(&dmg(15.0, 2, 50, 18000000, 500, 0)); // joins at 15 s
            e.handle(&dmg(20.0, 1, 50, 18000000, 500, 0));
            e.handle(&GameEvent::Death { ts: 20.0, target: 50, killer: Some(1) });
            let f = e.current.as_ref().unwrap();
            e.fight_table(f, None)
        };
        let row = |t: &Value, n: &str| t.get("players").unwrap().as_arr().unwrap().iter().find(|r| r.str_or("name", "") == n).unwrap().f64_or("dps", 0.0);
        // Boss fight: boss + add damage from the pull to the kill (trash
        // before the pull left out), divided by 10 s.
        let b = run(DpsClock::Boss);
        assert_eq!(b.str_or("clock", ""), "boss");
        assert_eq!(row(&b, "Me"), 170.0);
        assert_eq!(row(&b, "Late"), 50.0);
        assert_eq!(b.f64_or("clock_counted", 0.0), 1.0);
        // Whole fight: everything ÷ 20 s.
        let w = run(DpsClock::Fight);
        assert_eq!(row(&w, "Me"), 110.0);
        // Your time: the late joiner's own 1 s, and ranked first by DPS
        // (500/s) although "Me" did more damage.
        let p = run(DpsClock::Personal);
        assert_eq!(row(&p, "Late"), 500.0);
        let first = p.get("players").unwrap().as_arr().unwrap()[0].str_or("name", "").to_string();
        assert_eq!(first, "Late");
        let b0 = b.get("players").unwrap().as_arr().unwrap()[0].str_or("name", "").to_string();
        assert_eq!(b0, "Me", "shared clocks rank by damage");
        // HP check: 2000 of 2000 counted on the dead boss.
        let tg = b.get("target").unwrap();
        assert_eq!(tg.f64_or("counted", 0.0), 1.0, "{}", b.to_string());
    }

    #[test]
    fn boss_mechanics_late_ticks_and_pauses() {
        let names = Names::from_json(r#"{"bosses": ["777"]}"#).unwrap();
        let mut e = Engine::new(EngineConfig::default(), names);
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(10_000) });
        e.handle(&dmg(0.0, 1, 50, 18000000, 1000, 0));
        // The boss heals itself off our hit: reported like a tick, not damage.
        e.handle(&dmg(1.0, 1, 50, 18000000, 4000, flags::DOT | flags::NPC_ONLY));
        e.handle(&dmg(2.0, 1, 50, 18000000, 1000, 0));
        // Barrier: nobody can hit the boss for 30 s (an add takes damage).
        e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
        e.handle(&dmg(10.0, 1, 60, 18000000, 500, 0));
        e.handle(&dmg(33.0, 1, 50, 18000000, 1000, 0));
        e.handle(&dmg(34.0, 1, 50, 18000000, 1000, 0));
        e.handle(&GameEvent::Death { ts: 34.0, target: 50, killer: Some(1) });
        e.handle(&dmg(34.5, 1, 50, 18000000, 700, flags::DOT)); // after the kill
        let f = e.current.as_ref().unwrap();
        assert_eq!(f.stats[&1].by_target[&50], 4000, "boss heal and late tick ignored");
        let t = e.fight_table(f, None);
        // 34 s on the boss minus the 30 s with no boss damage (3..32): 4 s,
        // and the add's damage inside that pause doesn't count either.
        assert_eq!(t.f64_or("clock_paused", 0.0), 30.0, "{}", t.to_string());
        let me = &t.get("players").unwrap().as_arr().unwrap()[0];
        assert_eq!(me.f64_or("dmg", 0.0), 4000.0);
        assert_eq!(me.f64_or("dps", 0.0), 1000.0);
    }

    /// World boss (Addicted Hardirun): the game only sends the hits of
    /// players near you, so the HP check reads a few percent. The meter
    /// watched it from full health to the kill, so the fight is complete
    /// for you. A spawn's "full" health doesn't count as watching: only a
    /// real health update does. And a stray hit by someone else on another
    /// monster after the kill no longer cancels "killed".
    #[test]
    fn open_world_boss_watched_from_full() {
        let run = |first_hp: u64| {
            let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
            e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
            e.handle(&GameEvent::Actor { id: 2, name: Some("Ally".into()), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
            e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(100_000) });
            e.handle(&GameEvent::Hp { ts: 0.0, id: 50, hp: 100_000, max_hp: Some(100_000) }); // spawn: always "full"
            e.handle(&dmg(0.5, 1, 50, 18000000, 1000, 0));
            e.handle(&GameEvent::Hp { ts: 0.5, id: 50, hp: first_hp, max_hp: None });
            e.handle(&dmg(5.0, 2, 50, 18000000, 1000, 0)); // the ally joins on the boss
            e.handle(&dmg(10.0, 1, 50, 18000000, 2000, 0));
            e.handle(&GameEvent::Hp { ts: 19.9, id: 50, hp: 0, max_hp: None });
            e.handle(&GameEvent::Death { ts: 20.0, target: 50, killer: None });
            e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
            e.handle(&dmg(21.0, 2, 60, 18000000, 100, 0)); // the ally hits another monster
            let f = e.current.as_ref().unwrap();
            assert!(f.boss_killed, "a stray hit after the kill must not cancel it");
            e.fight_table(f, None)
        };
        let t = run(99_000);
        assert_eq!(t.str_or("clock", ""), "boss");
        assert!((t.f64_or("clock_counted", 1.0) - 0.04).abs() < 1e-9, "{}", t.to_string());
        assert!(t.bool_or("clock_watched", false), "{}", t.to_string());
        assert!(t.get("target").unwrap().bool_or("watched", false));
        // First seen at 60%: the meter joined late, so not watched.
        let late = run(60_000);
        assert!(!late.bool_or("clock_watched", true), "{}", late.to_string());
    }

    #[test]
    fn ranks_count_as_boss() {
        assert!(rank_is_boss_or_elite("Elite"));
        assert!(rank_is_boss_or_elite("HeroicMonster"));
        assert!(!rank_is_boss_or_elite("Common"));
        assert!(!rank_is_boss_or_elite("NormalMonster"));
        assert!(!rank_is_boss_or_elite(""));
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
        assert_eq!(t.get("target").unwrap().str_or("name", ""), "Unknown monster");
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
        let mut f = Fight { id: 1, start: 0.0, end: 1.0, started_unix: 0.0, stats: Default::default(), targets: Default::default(), deaths: vec![], loot: vec![], zone: None, boss_killed: false, finished: true, self_id: None, names: Default::default(), allies: Default::default(), focus: None, tspan: Default::default(), tsec: Default::default(), tinfo: Default::default(), twatch: Default::default() };
        f.names.insert(9, ("NPC 2400002".into(), None));
        e.history.push_back(f);
        e.rename_npc(2400002, "Black Claw Manduri Fighter");
        assert_eq!(e.history[0].names[&9].0, "Black Claw Manduri Fighter");
        let b = Names::from_json(r#"{"berserk": {"2400050": 480}}"#).unwrap();
        assert_eq!(b.berserk.get(&2400050), Some(&480));
    }
    #[test]
    fn learns_template_max_hp() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::Actor { id: 1, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(2400249), owner: None, max_hp: Some(119_700) });
        // Same monster type, appears already hurt: spawn says 72,792.
        e.handle(&GameEvent::Actor { id: 2, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(2400249), owner: None, max_hp: Some(72_792) });
        e.handle(&GameEvent::Hp { ts: 1.0, id: 2, hp: 72_792, max_hp: Some(72_792) });
        let a = e.actors.get(&2).unwrap();
        assert_eq!((a.hp, a.max_hp), (Some(72_792), Some(119_700)));
    }

}

#[cfg(test)]
mod zone_notice_tests {
    use super::*;

    #[test]
    fn remembered_self_is_not_a_zone_change() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.restore_self(13745, Some("DewiHijau".into()));
        // Meter opened mid-zone: the game reports a new id (new session).
        e.handle(&GameEvent::SelfId { id: 6817, name: None });
        assert!(!e.fresh_view, "a remembered id must not clear the notice");
        // Could be another character: no remembered name until confirmed.
        assert_eq!(e.self_name, None);
        e.handle(&GameEvent::SelfId { id: 6817, name: Some("DewaPutih".into()) });
        assert_eq!(e.self_name.as_deref(), Some("DewaPutih"));
        // A real zone change afterwards does.
        e.handle(&GameEvent::SelfId { id: 7000, name: None });
        assert!(e.fresh_view);
    }

    #[test]
    fn zone_load_clears_notice() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.restore_self(6817, Some("DewiHijau".into()));
        // Same character, same id: the remembered name stays.
        e.handle(&GameEvent::SelfId { id: 6817, name: None });
        assert_eq!(e.self_name.as_deref(), Some("DewiHijau"));
        e.handle(&GameEvent::SelfId { id: 6817, name: Some("DewiHijau".into()) });
        assert!(!e.fresh_view);
        e.handle(&GameEvent::Fresh);
        assert!(e.fresh_view);
    }
}
