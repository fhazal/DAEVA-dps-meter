//! Combat engine: turns game events into fights, DPS tables and skill
//! breakdowns.

use crate::events::{flags, ActorKind, GameEvent};
use crate::json::{self, Obj, Value};
use std::collections::{HashMap, VecDeque};

// ---------------------------------------------------------------- names

/// Skill keys with this bit set are the periodic (DoT/HoT) part of a skill,
/// listed separately like "Chain of Torment (DOT)".
pub const DOT_KEY: u32 = 1 << 31;

/// A heal "amount" this big (i32::MAX) means "heal to full"; never counted.
pub const HEAL_TO_FULL: u64 = 0x7fff_ffff;
/// No monster comes near this much health (the biggest bosses have about
/// 100 million). A garbled health value above it was remembered as the
/// monster type's maximum (Drana Predator, 2026-10-27: "0 / 108728033.00B").
const MAX_PLAUSIBLE_HP: u64 = 100_000_000_000;

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
    /// Enrage times from aion2.gg (id -> seconds, name): used where our own
    /// community list (`berserk`) has no entry.
    pub berserk_gg: HashMap<u32, (u32, String)>,
    /// Monster rank by template ("Common", "Elite", "Heroic"...).
    pub ranks: HashMap<u32, String>,
    /// Monster family by template (Cogni, Natura, Fera, Varian...).
    pub families: HashMap<u32, String>,
    /// Party-list class number -> class name ("22" -> Elementalist). The
    /// number follows the class but isn't one per class (Chanters seen as 35
    /// and 36), so only numbers seen with a known class are trusted.
    pub party_codes: HashMap<u32, String>,
    /// Class -> its block of party-list numbers (Gladiator 5..=8): each class
    /// has a block of four. Only classes confirmed by real party lists.
    pub party_code_ranges: Vec<(u32, u32, String)>,
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
        for (code, x) in ids("party_class_codes") {
            if let Some(s) = x.as_str() {
                n.party_codes.insert(code, s.to_string());
            }
        }
        if let Some(o) = v.get("party_class_ranges").and_then(|o| o.as_obj()) {
            for (class, r) in o {
                let b: Vec<u32> = r.as_arr().map(|a| a.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).collect()).unwrap_or_default();
                if let [lo, hi] = b.as_slice() {
                    n.party_code_ranges.push((*lo, *hi, class.clone()));
                }
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

    /// The class a party-list number means: a number seen before, else the
    /// class whose block it falls in.
    pub fn party_class(&self, code: u32) -> Option<&String> {
        self.party_codes.get(&code).or_else(|| self.party_code_ranges.iter().find(|(lo, hi, _)| (*lo..=*hi).contains(&code)).map(|(_, _, c)| c))
    }

    /// The template whose rank, family and picture describe `npc`: itself when
    /// listed, else the one listed monster with the same name. Dungeon variants
    /// such as Conquest Vakron have ids of their own that the data doesn't
    /// list; a name shared by monsters of different rank or family gives None.
    /// A match with a family but no rank still counts (Vakron trash such as
    /// Giant Honeybee is listed without a rank).
    pub fn npc_ref(&self, npc: u32, name: &str) -> Option<u32> {
        if self.ranks.contains_key(&npc) || self.families.contains_key(&npc) {
            return Some(npc);
        }
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let mut found: Option<u32> = None;
        for (id, n) in &self.npcs {
            let described = self.ranks.contains_key(id) || self.families.contains_key(id);
            if *id == npc || !described || !n.trim().eq_ignore_ascii_case(name) {
                continue;
            }
            found = match found {
                None => Some(*id),
                Some(f) if self.ranks.get(&f) == self.ranks.get(id) && self.families.get(&f) == self.families.get(id) => Some(f.min(*id)),
                Some(_) => return None,
            };
        }
        found
    }

    /// Seconds from the first hit until this boss enrages ("berserk"): by its
    /// own id, else by a listed boss with the same name (dungeon variants),
    /// when every such entry agrees.
    pub fn berserk_for(&self, npc: u32, name: &str) -> Option<u32> {
        self.enrage_for(npc, name).map(|e| e.0)
    }

    /// Enrage time and where it comes from ("community" or "aion2.gg"). Our
    /// community list wins: by id, then aion2.gg by id, then each by name.
    pub fn enrage_for(&self, npc: u32, name: &str) -> Option<(u32, &'static str)> {
        if let Some(s) = self.berserk.get(&npc) {
            return Some((*s, "community"));
        }
        if let Some((s, _)) = self.berserk_gg.get(&npc) {
            return Some((*s, "aion2.gg"));
        }
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let same = |n: &str| n.trim().eq_ignore_ascii_case(name);
        let agree = |it: &mut dyn Iterator<Item = u32>| -> Result<Option<u32>, ()> {
            let mut found = None;
            for secs in it {
                match found {
                    None => found = Some(secs),
                    Some(f) if f == secs => {}
                    Some(_) => return Err(()),
                }
            }
            Ok(found)
        };
        let ours = agree(&mut self.berserk.iter().filter(|(id, _)| self.npcs.get(id).map_or(false, |n| same(n))).map(|(_, s)| *s));
        match ours {
            Ok(Some(s)) => return Some((s, "community")),
            Err(()) => return None,
            Ok(None) => {}
        }
        agree(&mut self.berserk_gg.values().filter(|(_, n)| same(n)).map(|(s, _)| *s)).ok().flatten().map(|s| (s, "aion2.gg"))
    }

    /// Reads aion2.gg's boss file ({"id": {"name", "enrage", "stagger"}}) into
    /// a Names that only carries their enrage times.
    pub fn from_aion2gg(src: &str) -> Result<Names, String> {
        let v = json::parse(src).map_err(|e| e.to_string())?;
        let o = v.as_obj().ok_or("not an object")?;
        let mut n = Names::default();
        for (k, x) in o {
            let Some(id) = json::parse_number_str(k) else { continue };
            if let Some(secs) = x.get("enrage").and_then(|s| s.as_u64()).filter(|s| *s > 0 && *s < 24 * 3600) {
                n.berserk_gg.insert(id as u32, (secs as u32, x.str_or("name", "").to_string()));
            }
        }
        if n.berserk_gg.is_empty() {
            return Err("no enrage times".into());
        }
        Ok(n)
    }

    /// Adds (and overrides with) every name in `other`.
    pub fn merge(&mut self, other: Names) {
        self.skills.extend(other.skills);
        self.npcs.extend(other.npcs);
        self.zones.extend(other.zones);
        self.servers.extend(other.servers);
        self.items.extend(other.items);
        self.grades.extend(other.grades);
        self.bosses.extend(other.bosses);
        self.berserk.extend(other.berserk);
        self.berserk_gg.extend(other.berserk_gg);
        self.ranks.extend(other.ranks);
        self.families.extend(other.families);
        self.party_codes.extend(other.party_codes);
        self.party_code_ranges.extend(other.party_code_ranges);
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

    /// Adds another breakdown of the same skill (one player under two ids).
    fn absorb(&mut self, o: &SkillStat) {
        for (a, b) in [
            (&mut self.dmg, o.dmg), (&mut self.swings, o.swings), (&mut self.hits, o.hits), (&mut self.crits, o.crits),
            (&mut self.back, o.back), (&mut self.front, o.front), (&mut self.perfect, o.perfect), (&mut self.double, o.double),
            (&mut self.parry, o.parry), (&mut self.block, o.block), (&mut self.dot, o.dot), (&mut self.casts, o.casts), (&mut self.multi, o.multi),
        ] {
            *a += b;
        }
        self.max = self.max.max(o.max);
        self.min = match (self.min, o.min) { (0, m) | (m, 0) => m, (a, b) => a.min(b) };
    }
}

#[derive(Debug, Clone, Default)]
pub struct ActorStat {
    pub dmg: u64,
    pub hits: u64,
    pub crits: u64,
    pub back: u64,
    pub front: u64,
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

impl ActorStat {
    /// Adds the stats of the same player under another id (died and came
    /// back, or named late): one row instead of two.
    fn absorb(&mut self, o: ActorStat) {
        for (a, b) in [
            (&mut self.dmg, o.dmg), (&mut self.hits, o.hits), (&mut self.crits, o.crits), (&mut self.back, o.back),
            (&mut self.front, o.front), (&mut self.perfect, o.perfect), (&mut self.heal, o.heal), (&mut self.taken, o.taken),
            (&mut self.taken_hits, o.taken_hits), (&mut self.taken_parry, o.taken_parry), (&mut self.taken_block, o.taken_block),
            (&mut self.taken_perfect_block, o.taken_perfect_block), (&mut self.doubles, o.doubles),
        ] {
            *a += b;
        }
        self.deaths += o.deaths;
        if o.max_hit > self.max_hit {
            self.max_hit = o.max_hit;
            self.max_hit_skill = o.max_hit_skill;
        }
        self.first = self.first.min(o.first);
        self.last = self.last.max(o.last);
        let skills = |a: &mut HashMap<u32, SkillStat>, b: HashMap<u32, SkillStat>| {
            for (k, v) in b {
                a.entry(k).or_default().absorb(&v);
            }
        };
        skills(&mut self.skills, o.skills);
        skills(&mut self.heal_skills, o.heal_skills);
        for (t, m) in o.target_skills {
            skills(self.target_skills.entry(t).or_default(), m);
        }
        for (a, b) in [(&mut self.by_target, o.by_target), (&mut self.taken_from, o.taken_from), (&mut self.healed, o.healed)] {
            for (k, v) in b {
                *a.entry(k).or_default() += v;
            }
        }
        if self.timeline.len() < o.timeline.len() {
            self.timeline.resize(o.timeline.len(), 0);
        }
        for (i, v) in o.timeline.into_iter().enumerate() {
            self.timeline[i] += v;
        }
    }
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
    /// `taken` at the last health update, and how much health each stretch
    /// of seen hits removed per point of damage (see `hp_scale`).
    pub taken_at_hp: u64,
    pub hp_ratios: Vec<f32>,
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
    /// Frozen health-per-damage factor per target (see `hp_scale`).
    pub tscale: HashMap<u64, f64>,
    /// When you or your party last used a skill on each target
    /// (damage-over-time ticks left out), to tell when you've moved on from
    /// a boss.
    pub own_last: HashMap<u64, f64>,
    /// You were in a dungeon party during this fight: the boss clock and a
    /// card that stays on the boss. On your own (or in an open-world party):
    /// the whole fight and your target.
    pub grouped: bool,
    /// Names in your party or Force while this fight ran (a world boss
    /// lists only them, also later in the history).
    pub group: std::collections::HashSet<String>,
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

/// What the party panel shows.
#[derive(Debug, Clone)]
pub struct PartyState {
    pub title: Option<String>,
    pub leader: Option<u64>,
    pub members: Vec<crate::events::PartyMember>,
    key: String,
    /// Game time the line-up last changed.
    pub since: f64,
    /// The leader clicked Enter dungeon.
    pub entering: bool,
    /// That click restarted the dungeon you're in (a rerun): the panel
    /// shows "Entering · Rerun" until the first pull.
    pub rerun: bool,
    /// You're inside the dungeon (the room list marks you 0x10, or you
    /// teleported after Enter): the panel shows who's inside, not Ready.
    pub inside: bool,
    /// Members who joined while you were inside, with the game time.
    pub joined: Vec<(String, f64)>,
    /// Members who left while you were inside, with the game time.
    pub left: Vec<(crate::events::PartyMember, f64)>,
}

/// Room status bit: the member is inside the dungeon.
pub const IN_DUNGEON: u8 = 0x10;

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

/// Boss and elite ranks (same list as the Daeva bot). Anything else, such as
/// Common, townspeople ("normalcitizen"), crafting tables ("object"), portals
/// and platforms ("vessel"), isn't.
pub fn rank_is_boss_or_elite(rank: &str) -> bool {
    let r: String = rank.to_ascii_lowercase().trim_end_matches("monster").chars().filter(|c| c.is_ascii_lowercase()).collect();
    matches!(r.as_str(), "elite" | "heroic" | "boss" | "legendary" | "legend")
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { idle_timeout: 8.0, min_fight_damage: 1, merge_summons: true, end_on_target_death: true, only_my_targets: true, boss_idle_timeout: 150.0, death_grace: 3.0, history_size: 50, dps_clock: DpsClock::Boss }
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
    /// ↺ / Ctrl+Shift+R: the last fight is off screen until the next one
    /// starts (it stays in history).
    cleared: bool,
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
    /// A player back under a new entity id (died and returned, Divine
    /// Auldor 2026-10-27): new id -> the id their row already has.
    player_alias: HashMap<u64, u64>,
    /// The party panel's details (title, leader, gear, ready), if in one.
    pub party_info: Option<PartyState>,
    /// Your Force (several parties at a field boss), by the game's member key.
    pub force: HashMap<u32, crate::events::ForceMember>,
    /// Each group member's class number from the party list.
    pub party_member_codes: HashMap<String, u32>,
    /// Class numbers that turned out to mean more than one class.
    bad_codes: std::collections::HashSet<u32>,
    /// Fights from this id on are in the current zone: entity ids are
    /// reassigned on a zone change, so a late name only fills these in.
    zone_first_fight: u64,
    /// When you last changed zone (teleport, relog): the party panel is for
    /// before that, the ready list for after.
    zone_at: f64,
    /// When you last came into a dungeon (or any zone outside one): the
    /// Ready line waits for the first hit after this. A respawn inside the
    /// same run is a zone change too, but not an entry.
    entry_at: f64,
    /// You left the party: the dungeon keeps listing who is still inside,
    /// which isn't your party. Cleared when you join one again.
    left_party: bool,
    /// When the leader last clicked Enter while you were inside a dungeon
    /// you had fought in (game time): a rerun if "party gone" follows.
    rerun_click_at: f64,
    /// When you last joined a room (0x9710) and the last single join entry
    /// (game time, name): together they name you before any zone change.
    room_entered_at: f64,
    /// Single join entries of the last few seconds (game time, name), and
    /// the name taken from one of them, if any.
    recent_joins: Vec<(f64, String)>,
    named_from_join: Option<String>,
    /// You left a dungeon and haven't entered one since. A rerun room keeps
    /// the "in dungeon" bit on everyone from the last run while you wait
    /// outside, so it's ignored until the leader clicks Enter.
    exited_dungeon: bool,
    /// The names list sent with an ignored room list.
    skip_party_names: bool,
    /// True once the meter has seen a login or zone change, i.e. the game
    /// has introduced everything around you (names, monsters, health).
    pub fresh_view: bool,
    /// The current self id came from the game this run (not restored from
    /// the last session), so a different id means a real zone change.
    self_live: bool,
    /// When the game last moved to a new connection (game time).
    new_conn_at: f64,
    /// Your name from last session, set aside until the game confirms it.
    remembered_name: Option<String>,
    /// When you last left a room while inside its dungeon (game time).
    room_exit_at: f64,
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
            cleared: false,
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
            player_alias: HashMap::new(),
            force: HashMap::new(),
            party_info: None,
            party_member_codes: HashMap::new(),
            bad_codes: Default::default(),
            zone_first_fight: 1,
            zone_at: f64::NEG_INFINITY,
            entry_at: f64::NEG_INFINITY,
            left_party: false,
            rerun_click_at: f64::NEG_INFINITY,
            room_entered_at: f64::NEG_INFINITY,
            recent_joins: Vec::new(),
            named_from_join: None,
            exited_dungeon: false,
            skip_party_names: false,
            fresh_view: false,
            self_live: false,
            new_conn_at: f64::NEG_INFINITY,
            remembered_name: None,
            room_exit_at: f64::NEG_INFINITY,
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
            taken_at_hp: 0,
            hp_ratios: Vec::new(),
        })
    }

    fn resolve_owner(&self, id: u64) -> u64 {
        let id = self.player_alias.get(&id).copied().unwrap_or(id);
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
        // A spirit found by its owner's name can land on the owner's new id.
        self.player_alias.get(&cur).copied().unwrap_or(cur)
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
                // A summon "hitting" itself is the game giving it its health
                // as it spawns (Earth Spirit 16990004, per aion2.gg), not a hit
                // or a heal: it counted as the Elementalist's healing.
                if src == tgt && self.actors.get(src).map_or(false, |a| a.kind == ActorKind::Summon) {
                    return finished;
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
                // A party in a dungeon room or inside a dungeon. An open-world
                // party (a festival, a field boss) counts like you on your own,
                // as 0.5.1 did: elites die every few seconds there.
                let dungeon_party = self.party.len() >= 2 && self.party_info.as_ref().map_or(false, |p| p.title.is_some() || p.inside);
                if self.cfg.only_my_targets {
                    if let Some(me) = self.self_id {
                        let mine = attacker == me;
                        let live = self.current.as_ref().filter(|f| !f.finished);
                        let shared = live.map_or(false, |f| f.stats.get(&me).map_or(false, |s| s.by_target.contains_key(tgt)));
                        let ally = live.map_or(false, |f| f.allies.contains(&attacker));
                        // In a dungeon party, every boss fight counts from the
                        // first hit, also when your group pulls it before you
                        // get there (the game sends it once you're in the area).
                        let party_boss = dungeon_party
                            // A party member: named in the party list, or not
                            // named yet but of a class a member still unmatched
                            // has (an Elementalist's spirits aren't members).
                            && self.actors.get(&attacker).map_or(false, |a| match &a.name {
                                Some(n) => self.party.contains(n),
                                None => Self::is_class_player(a) && self.actor_class(attacker).map_or(false, |c| self.party_has_unmatched(&c)),
                            })
                            && self.is_boss_actor(*tgt);
                        if !mine && !shared && !ally && !party_boss {
                            return finished;
                        }
                        // Someone who helped you, now hitting monsters you
                        // never touched after you stopped: not your fight any
                        // more (their DPS climbed while the clock stood still).
                        if !mine && !shared && !party_boss && live.map_or(true, |f| *ts > f.end + 2.0) {
                            return finished;
                        }
                        extends = mine || shared || party_boss;
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
                // The kill flag can be an add's that was the main target when
                // it died; the boss is only down when it really died
                // (Tetrata, 2026-10-26: split at 55M HP).
                let boss_down = self.current.as_ref().filter(|f| f.boss_killed).and_then(|f| f.main_target()).map_or(false, |m| self.is_real_boss(m) && self.dead_at.contains_key(&m));
                // The boss is down and you (or your group) start on another
                // monster: that's a new fight. The boss fight ends as it was,
                // and the card follows what you fight next.
                // On your own you left a boss (no skill of yours or your
                // party's on it for 10 s: damage-over-time ticks and other
                // players' hits don't count) and started on another monster:
                // a new fight. Only a real boss: leaving an Elite for the mobs
                // around it is the same fight (the daily dungeon split into 30
                // pieces, Shugo into three, 2026-10-27).
                // In a dungeon party, bosses have add phases, so their fight
                // goes on.
                let boss_left = !dungeon_party
                    && attacker == self.self_id.unwrap_or(u64::MAX)
                    && fl & flags::DOT == 0
                    && self.current.as_ref().map_or(false, |f| {
                        !f.finished && !f.targets.contains_key(tgt) && self.card_boss(f).filter(|b| self.is_real_boss(*b)).map_or(false, |b| f.own_last.get(&b).map_or(true, |t| *ts - t > 10.0))
                    });
                let group_hit = fl & flags::DOT == 0
                    && (attacker == self.self_id.unwrap_or(u64::MAX)
                        || (self.party.len() >= 2 && self.actors.get(&attacker).and_then(|a| a.name.as_ref()).map_or(false, |n| self.party.contains(n))));
                let dead_boss = self.current.as_ref().filter(|_| boss_down).and_then(|f| f.main_target());
                let moved_on = |f: &Fight| (boss_down && dead_boss != Some(*tgt)) || (boss_left && !f.targets.contains_key(tgt));
                if (boss_down || boss_left) && extends && self.current.as_ref().map_or(false, |f| !f.finished && moved_on(f)) {
                    if let Some(f) = self.finish() {
                        finished = Some(f);
                    }
                }
                // Outside a dungeon party, a boss other players are already
                // on (a world boss) showing up in a fight on small mobs starts
                // its own fight: the mobs before it stay a fight of their own
                // (Tetrata, 2026-10-27: the soldiers you killed first went
                // into the boss fight, whose card shows only boss damage).
                // Also when you hit it first in a crowd (3+ players outside
                // your party in the fight). A boss you or your party pull on
                // your own keeps the trash before it; the boss clock leaves
                // that out.
                let crowd = |f: &Fight| {
                    f.stats.keys().filter(|id| Some(**id) != f.self_id && self.actors.get(id).map_or(false, |a| a.kind == ActorKind::Player && a.name.as_ref().map_or(true, |n| !self.party.contains(n)))).count() >= 3
                };
                let boss_arrives = !dungeon_party
                    && fl & flags::DOT == 0
                    && self.current.as_ref().map_or(false, |f| {
                        !f.finished && !f.targets.is_empty() && !f.targets.contains_key(tgt) && self.is_real_boss(*tgt) && !f.targets.keys().any(|t| self.is_real_boss(*t)) && (!group_hit || crowd(f))
                    });
                if boss_arrives {
                    if let Some(f) = self.finish() {
                        finished = Some(f);
                    }
                }
                let grouped = dungeon_party;
                let fight = self.ensure_fight(*ts);
                fight.grouped |= grouped;
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
                // Your skills set what the card follows; your damage-over-time
                // ticks on something you left don't pull it back.
                if fight.self_id.is_some() && fight.self_id == Some(attacker) && fl & flags::DOT == 0 {
                    fight.focus = Some(*tgt);
                }
                if group_hit {
                    fight.own_last.insert(*tgt, *ts);
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
                    if fl & flags::FRONT != 0 {
                        s.front += 1;
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
                // 0x7fffffff is the game's "heal to full", not an amount
                // (Elementalist 16170031 at Watcher Kaira, 2026-10-27: three
                // of them showed as 6.4 billion healing).
                if *amount >= HEAL_TO_FULL {
                    return None;
                }
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
                // A mob dying next to a living boss (it took more of your
                // damage than the boss, say) doesn't end the fight or make it
                // a kill: that would read as a kill of the boss.
                let boss_lives = self.current.as_ref().and_then(|f| self.card_boss(f)).map_or(false, |b| b != *target && !self.dead_at.contains_key(&b));
                let main = main.filter(|_| !boss_lives);
                if let Some(f) = self.current.as_mut() {
                    f.deaths.push((*ts, *target, *killer));
                    if let Some(s) = f.stats.get_mut(target) {
                        s.deaths += 1;
                    }
                }
                if self.cfg.end_on_target_death && main == Some(*target) && !self.actors.get(target).map_or(false, |a| a.kind == ActorKind::Player) {
                    // An Elite's death is just a kill: festivals and wave
                    // dungeons kill them every few seconds, and each one
                    // ended the fight 3 s later (Shugo, 2026-10-27). With
                    // other players in the fight (an event, a crowd), a common
                    // monster's death doesn't either: the next mob is often
                    // 3 s away (spirit wave, 2026-10-27). On your own (or with
                    // your party) it does, so farming gives a fight per mob.
                    // A real boss's death always ends the fight.
                    let real = self.is_real_boss(*target);
                    let elite = self.is_boss_actor(*target) && !real;
                    let others = self.current.as_ref().map_or(false, |f| {
                        f.stats.keys().any(|id| Some(*id) != f.self_id && self.actors.get(id).map_or(false, |a| a.kind == ActorKind::Player && a.name.as_ref().map_or(true, |n| !self.party.contains(n))))
                    });
                    if let Some(f) = self.current.as_mut() {
                        f.boss_killed = true;
                        f.end = f.end.max(*ts);
                    }
                    if real || (!elite && !others) {
                        self.pending_end = Some(*ts + self.cfg.death_grace);
                    }
                }
            }
            GameEvent::Actor { id, name, class, kind, npc_id, owner, max_hp } => {
                if npc_id.is_some() {
                    self.dead_at.remove(id); // a new spawn
                }
                // Another player back under a new id: same name, same class
                // (when known) as a player already seen. Their hits count
                // for the row they already have, not a second one.
                // Likewise an object that carries only its owner's name, with
                // that player out of sight: each aura or totem they place is a
                // new id (Grandios, 2026-10-27: "Jiang" twice, Divine Aura).
                let object = *kind == ActorKind::Summon && owner.is_none();
                if let (ActorKind::Player | ActorKind::Summon, Some(n)) = (kind, name) {
                    let unseen_owner = !self.actors.values().any(|a| a.kind == ActorKind::Player && a.name.as_deref() == Some(n.as_str()));
                    if (*kind == ActorKind::Player || (object && unseen_owner)) && Some(*id) != self.self_id && self.self_name.as_deref() != Some(n.as_str()) && !self.player_alias.contains_key(id) {
                        let old = self.actors.values().find(|a| {
                            a.id != *id && a.kind == *kind && (!object || a.owner.is_none()) && a.name.as_deref() == Some(n.as_str()) && !self.player_alias.contains_key(&a.id) && Some(a.id) != self.self_id
                                && match (&a.class, class) { (Some(x), Some(y)) => x == y, _ => true }
                        }).map(|a| a.id);
                        if let Some(old) = old {
                            self.player_alias.insert(*id, old);
                            // Hits made under the new id before its name came.
                            if let Some(f) = self.current.as_mut().filter(|f| !f.finished) {
                                if let Some(st) = f.stats.remove(id) {
                                    match f.stats.get_mut(&old) {
                                        Some(o) => o.absorb(st),
                                        None => {
                                            f.stats.insert(old, st);
                                        }
                                    }
                                }
                                if f.allies.remove(id) {
                                    f.allies.insert(old);
                                }
                            }
                        }
                    }
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
                if let Some(m) = max_hp.filter(|m| *m <= MAX_PLAUSIBLE_HP) {
                    a.max_hp = Some(m);
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
                if let Some(n) = name {
                    if self.actors.get(id).map_or(false, |a| a.kind == ActorKind::Player) {
                        self.backfill_name(*id, n);
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
                    // The meter started mid-session (your id remembered from
                    // last time) and the game has since moved to a new
                    // connection: this new id is a real zone load (2026-10-26:
                    // started in town, the first dungeon entry kept the town
                    // fight on screen and no Ready line).
                    let seen_world = self.last_ts - self.new_conn_at < 30.0;
                    if !self.self_live {
                        // Replacing an id remembered from the last session:
                        // not a zone change, the game just hasn't reloaded.
                        if let Some(old) = self.self_id {
                            self.actors.remove(&old);
                            // A different id may be a different character:
                            // don't show the remembered name until the game
                            // says who you are (zone load, self-buff, party).
                            self.remembered_name = self.self_name.take();
                        }
                    }
                    if (self.self_live || seen_world) && self.self_id.is_some() {
                        self.fresh_view = true;
                        // Zone change / relog: every entity id is reassigned.
                        finished = self.finish();
                        self.actors.clear();
                        self.player_alias.clear();
                        self.zone_first_fight = self.next_id;
                        self.zone_at = self.last_ts;
                        // You died and came back inside the same run: no Enter,
                        // no leaving, already inside. That's not an entry, so
                        // no Ready line until the next pull (Urugugu,
                        // 2026-10-27: "Ready · 3/5 in dungeon" for 75 s).
                        // Alone in the room's list (everyone else left), the
                        // way out is no respawn: you left the dungeon, even
                        // when the game sends no "left" message (Urugugu,
                        // 2026-10-27: the next party showed "On the way").
                        let respawn = self.party_info.as_ref().map_or(false, |p| p.inside && !p.entering && !self.exited_dungeon && p.members.len() > 1);
                        if !respawn {
                            self.entry_at = self.last_ts;
                        }
                        if !respawn && self.party_info.as_ref().map_or(false, |p| p.inside && !p.entering) {
                            self.exited_dungeon = true;
                            if let Some(p) = self.party_info.as_mut() {
                                p.inside = false;
                            }
                        }
                        if let Some(p) = self.party_info.as_mut() {
                            // The teleport after Enter dungeon takes you inside
                            // (not one after leaving: that's the way home).
                            p.inside |= p.entering && !self.exited_dungeon;
                            p.entering = false;
                        }
                        self.pending_end = None;
                    }
                }
                self.self_id = Some(*id);
                self.self_live = true;
                // A fight already running (your party pulled while you were
                // still loading in) is yours too under the new id.
                if let Some(f) = self.current.as_mut().filter(|f| !f.finished) {
                    f.self_id = Some(*id);
                }
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
            GameEvent::Hp { hp, max_hp, .. } if *hp > MAX_PLAUSIBLE_HP || max_hp.map_or(false, |m| m > MAX_PLAUSIBLE_HP) => {}
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
                // How much health the hits seen since the last update removed,
                // per point of their damage. In the open world the game takes
                // 4.5x the damage it shows off a monster's health (dungeon
                // bosses: 1x); the HP check needs that factor.
                if max_hp.is_none() {
                    if let Some(old) = a.hp {
                        let seen = a.taken.saturating_sub(a.taken_at_hp);
                        if *hp < old && *hp > 0 && seen > 0 {
                            if a.hp_ratios.len() >= 512 {
                                a.hp_ratios.remove(0);
                            }
                            a.hp_ratios.push((old - *hp) as f32 / seen as f32);
                        }
                    }
                    a.taken_at_hp = a.taken;
                }
                let old_hp = a.hp;
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
                // A wipe: the fight's boss is back at (nearly) full health. That
                // try is over; the next pull is a new fight. (Heals are smaller.)
                let max = self.actors.get(id).and_then(|a| a.max_hp).unwrap_or(0) as f64;
                let reset = max > 0.0 && old_hp.map_or(false, |o| o as f64 <= 0.90 * max) && *hp as f64 >= 0.97 * max;
                // Also the fight's main target when it's a real boss: a dungeon
                // variant with no rank of its own (Transcendent Bakarma 2300471)
                // isn't a card boss, and its wipes went unseen.
                let boss_reset = |f: &Fight| self.card_boss(f) == Some(*id) || (f.main_target() == Some(*id) && self.is_real_boss(*id));
                if reset && self.current.as_ref().map_or(false, |f| !f.finished && boss_reset(f)) {
                    finished = self.finish();
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
            GameEvent::PartyInfo { title, leader, members } => {
                // After the dungeon's "party gone" (0x8d3f, also sent about a
                // minute after the last boss dies) a room list still counts
                // when you're in it: the room lives on for a rerun.
                let has_me = self.self_name.as_deref().map_or(false, |me| members.iter().any(|m| m.name == me));
                // A rerun: inside the dungeon, a few seconds after the leader
                // clicks Enter, the game sends the same "party gone" as when
                // you leave. The party stays (Vakron Sky Island, 2026-10-26).
                let rerun = members.is_empty() && self.last_ts - self.rerun_click_at < 15.0 && self.party_info.as_ref().map_or(false, |p| p.inside);
                if rerun {
                    // Show the party again until the first pull.
                    self.skip_party_names = true;
                    let now = self.last_ts;
                    if let Some(p) = self.party_info.as_mut() {
                        p.rerun = true;
                        p.since = now;
                    }
                } else if self.left_party && title.is_some() && !has_me {
                    self.skip_party_names = true;
                } else if members.is_empty() {
                    self.party_info = None;
                    self.left_party = true;
                    // Leaving the party takes you out of its Force too.
                    self.force.clear();
                } else {
                    // Just you counts too: the party you start by inviting, or
                    // the room you open, lists only you until the others join
                    // (they come one by one, the full list up to 40 s later).
                    self.left_party = false;
                    let mut names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
                    names.sort();
                    let key = names.join("
");
                    // When the line-up changed (not just a Ready click): the
                    // panel shows until the next fight.
                    let changed = self.party_info.as_ref().map_or(true, |p| p.key != key);
                    // Someone joining (or a new party) brings the panel back;
                    // inside a dungeon someone leaving does too.
                    let joined = self.party_info.as_ref().map_or(true, |p| members.iter().any(|m| !p.members.iter().any(|o| o.name == m.name)));
                    let was_inside = self.party_info.as_ref().map_or(false, |p| p.inside);
                    let gone: Vec<crate::events::PartyMember> = self.party_info.as_ref().map_or(Vec::new(), |p| p.members.iter().filter(|o| !members.iter().any(|m| m.name == o.name)).cloned().collect());
                    let since = if joined || (was_inside && !gone.is_empty()) { self.last_ts } else { self.party_info.as_ref().map_or(self.last_ts, |p| p.since) };
                    // Open-world lists have no title or leader: keep the room's.
                    let keep = self.party_info.as_ref().filter(|_| !changed);
                    // The two lists alternate in a dungeon room; only one has
                    // Ready (and sometimes the gear): keep what the other lacks.
                    let mut members: Vec<crate::events::PartyMember> = members
                        .iter()
                        .map(|m| {
                            let old = keep.and_then(|p| p.members.iter().find(|o| o.name == m.name));
                            crate::events::PartyMember {
                                status: m.status.or(old.and_then(|o| o.status)),
                                gs: m.gs.or(old.and_then(|o| o.gs)),
                                cp: m.cp.or(old.and_then(|o| o.cp)),
                                server: m.server.or(old.and_then(|o| o.server)),
                                id: m.id.or(old.and_then(|o| o.id)),
                                ..m.clone()
                            }
                        })
                        .collect();
                    // Inside when the game marks you (or, before your name is
                    // known, anyone) as in the dungeon.
                    let me = self.self_name.as_deref();
                    if self.exited_dungeon {
                        for m in members.iter_mut() {
                            m.status = m.status.map(|s| s & !IN_DUNGEON);
                        }
                    }
                    let marked = |m: &crate::events::PartyMember| m.status.map_or(false, |s| s & IN_DUNGEON != 0);
                    // Open-world lists carry no status: they keep what we knew,
                    // but never "inside" once you've left the dungeon.
                    // A new party (nobody but you from the old one) never
                    // takes over "inside".
                    let same_party = self.party_info.as_ref().map_or(false, |p| p.members.iter().any(|o| Some(o.name.as_str()) != me && members.iter().any(|m| m.name == o.name)));
                    let inside = match members.iter().find(|m| Some(m.name.as_str()) == me) {
                        Some(m) if m.status.is_some() => marked(m),
                        _ => (was_inside && !self.exited_dungeon && same_party) || members.iter().any(marked),
                    };
                    let now = self.last_ts;
                    let old = self.party_info.as_ref();
                    let mut joined_list: Vec<(String, f64)> = old.map_or(Vec::new(), |p| p.joined.clone());
                    joined_list.retain(|(n, _)| members.iter().any(|m| &m.name == n));
                    if was_inside {
                        for m in members.iter().filter(|m| !old.map_or(true, |p| p.members.iter().any(|o| o.name == m.name))) {
                            joined_list.push((m.name.clone(), now));
                        }
                    }
                    let mut left_list: Vec<(crate::events::PartyMember, f64)> = old.map_or(Vec::new(), |p| p.left.clone());
                    left_list.retain(|(m, t)| now - t < 30.0 && !members.iter().any(|n| n.name == m.name));
                    if was_inside {
                        left_list.extend(gone.into_iter().map(|m| (m, now)));
                    }
                    self.party_info = Some(PartyState {
                        title: title.clone().or_else(|| keep.and_then(|p| p.title.clone())),
                        leader: leader.or_else(|| keep.and_then(|p| p.leader)),
                        members,
                        key,
                        since,
                        // Until you teleport: the lists right after the click can
                        // drop and re-add members for a moment.
                        entering: self.party_info.as_ref().map_or(false, |p| p.entering),
                        rerun: self.party_info.as_ref().map_or(false, |p| p.rerun),
                        inside,
                        joined: if inside { joined_list } else { Vec::new() },
                        left: if inside { left_list } else { Vec::new() },
                    });
                }
            }
            GameEvent::Party { .. } if std::mem::take(&mut self.skip_party_names) => {}
            GameEvent::ForceInfo { members } => {
                self.force = members.iter().filter(|m| m.key != 0).map(|m| (m.key, m.clone())).collect();
                self.note_group();
            }
            GameEvent::ForceJoin { member } => {
                if member.key != 0 {
                    self.force.insert(member.key, member.clone());
                }
                self.note_group();
            }
            GameEvent::ForceLeave { key } => {
                self.force.remove(key);
            }
            GameEvent::PartyJoin { member } => {
                self.recent_joins.retain(|(t, _)| self.last_ts - t < 5.0);
                self.recent_joins.push((self.last_ts, member.name.clone()));
                self.name_self_from_join();
                if let Some(p) = self.party_info.as_ref().filter(|_| !self.left_party) {
                    if !p.members.iter().any(|m| m.name == member.name) {
                        let mut members = p.members.clone();
                        members.push(member.clone());
                        let (title, leader) = (p.title.clone(), p.leader);
                        let names = members.iter().map(|m| m.name.clone()).collect();
                        let codes = members.iter().map(|m| m.code).collect();
                        self.handle(&GameEvent::PartyInfo { title, leader, members });
                        self.handle(&GameEvent::Party { names, codes });
                    }
                }
            }
            GameEvent::PartyReady { id, ready } => {
                if let Some(m) = self.party_info.as_mut().and_then(|p| p.members.iter_mut().find(|m| m.id == Some(*id))) {
                    let st = m.status.unwrap_or(0x0c);
                    m.status = Some(if *ready { st | 0x02 } else { st & !0x02 });
                }
            }
            GameEvent::RoomEntered => {
                self.exited_dungeon = false;
                self.room_entered_at = self.last_ts;
                self.named_from_join = None;
                self.name_self_from_join();
            }
            GameEvent::SelfReady { ready } => {
                let me = self.self_name.clone();
                if let Some(m) = self.party_info.as_mut().and_then(|p| p.members.iter_mut().find(|m| me.as_deref() == Some(m.name.as_str()))) {
                    let st = m.status.unwrap_or(0x0c);
                    m.status = Some(if *ready { st | 0x02 } else { st & !0x02 });
                }
            }
            // Out of the room: the panel empties at once. Unlike leaving a
            // party, the next room you join sends a titled list that counts.
            GameEvent::RoomLeft => {
                self.party_info = None;
                self.left_party = false;
                self.handle(&GameEvent::Party { names: Vec::new(), codes: Vec::new() });
            }
            // You left the room yourself. In the waiting room that's the end
            // of it (no party list follows); inside a dungeon the same message
            // comes just before DungeonLeft and you may still be in the party.
            GameEvent::RoomExit => {
                if self.party_info.as_ref().map_or(false, |p| !p.inside) {
                    self.handle(&GameEvent::RoomLeft);
                } else {
                    self.room_exit_at = self.last_ts;
                }
            }
            // You left the dungeon: the party (if any) is no longer inside.
            GameEvent::DungeonLeft => {
                // "Left the room" right before leaving the dungeon: you left the
                // party too (test15 exit 3, 2026-10-26; with the party staying,
                // vakron-012, only DungeonLeft comes). The room panel goes.
                if self.last_ts - self.room_exit_at < 3.0 {
                    self.room_exit_at = f64::NEG_INFINITY;
                    self.handle(&GameEvent::RoomLeft);
                }
                self.exited_dungeon = true;
                if let Some(p) = self.party_info.as_mut() {
                    p.inside = false;
                    p.joined.clear();
                    p.left.clear();
                    for m in p.members.iter_mut() {
                        m.status = m.status.map(|s| s & !IN_DUNGEON);
                    }
                }
            }
            GameEvent::PartyEntering => {
                self.exited_dungeon = false;
                // Enter while inside after a fight there: maybe a rerun (the
                // dungeon restarts in place, no teleport). Transcendence sends
                // the same click mid-run, without the "party gone" after it.
                let fought = self.display_fight().map_or(false, |f| f.end > self.zone_at);
                if self.party_info.as_ref().map_or(false, |p| p.inside) && fought {
                    self.rerun_click_at = self.last_ts;
                }
                if let Some(p) = self.party_info.as_mut() {
                    p.entering = true;
                }
            }
            GameEvent::Party { names, codes } => {
                // Your remembered name is in your party: that's you (the game
                // didn't say so after a restart, 2026-10-26: "You" plus a
                // "DewaPutih" row).
                if self.self_name.is_none() {
                    if let (Some(n), Some(id)) = (self.remembered_name.clone().filter(|n| names.contains(n)), self.self_id) {
                        self.self_name = Some(n.clone());
                        self.actor(id).name = Some(n.clone());
                        self.backfill_name(id, &n);
                    }
                }
                self.party = names.iter().cloned().collect();
                self.party_member_codes = names.iter().zip(codes.iter()).filter_map(|(n, c)| c.map(|c| (n.clone(), c))).collect();
                self.bind_party_names();
                self.note_group();
            }
            GameEvent::Fresh => {
                self.fresh_view = true;
                // Fights from here on are in the newly loaded view.
                self.zone_first_fight = self.zone_first_fight.max(self.next_id);
            }
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

    /// The live fight remembers everyone in your party and Force.
    fn note_group(&mut self) {
        if let Some(f) = self.current.as_mut().filter(|f| !f.finished) {
            f.group.extend(self.party.iter().cloned());
            f.group.extend(self.force.values().map(|m| m.name.clone()));
        }
    }

    fn ensure_fight(&mut self, ts: f64) -> &mut Fight {
        if self.current.as_ref().map_or(true, |f| f.finished) {
            self.cleared = false;
            if let Some(p) = self.party_info.as_mut() {
                if p.rerun {
                    // The rerun has started.
                    p.rerun = false;
                    p.entering = false;
                } else if p.inside {
                    // A fight inside: an Enter clicked in here is over. Urugugu
                    // (2026-10-25): an Enter mid-dungeon with no teleport stayed
                    // "entering" all run, so going home read as going in and
                    // the old party showed as Ready in the open world.
                    p.entering = false;
                }
            }
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
                tscale: Default::default(),
                own_last: Default::default(),
                grouped: false,
                group: Default::default(),
            });
            self.note_group();
        }
        self.current.as_mut().unwrap()
    }

    /// The game is talking at `ts`: times of events that carry none (party
    /// list, zone change) follow the traffic, not only the last hit.
    /// You joined a room and the room announced one new member in the same
    /// moment: that's you. Only used while the meter doesn't know you yet
    /// (fresh install, no teleport since), so your own Ready click shows.
    /// Only when exactly one player joined around then: with two, either
    /// could be you, and a name taken too early is taken back.
    fn name_self_from_join(&mut self) {
        let around: Vec<&String> = self.recent_joins.iter().filter(|(t, _)| (t - self.room_entered_at).abs() <= 1.0).map(|(_, n)| n).collect();
        if around.len() > 1 {
            if self.named_from_join.is_some() && self.self_name == self.named_from_join {
                self.self_name = None;
            }
            self.named_from_join = None;
            return;
        }
        if let (None, [name]) = (&self.self_name, around.as_slice()) {
            self.self_name = Some((*name).clone());
            self.named_from_join = Some((*name).clone());
        }
    }

    /// The game server started sending on a new connection (a zone load).
    pub fn new_connection(&mut self) {
        self.new_conn_at = self.last_ts;
    }

    pub fn see_time(&mut self, ts: f64) {
        self.last_ts = self.last_ts.max(ts);
    }

    /// Call periodically with the current capture time.
    pub fn tick(&mut self, now: f64) -> Option<Fight> {
        // A fighter's class is known from their first skill: try the party list.
        self.bind_party_names();
        if let Some(t) = self.pending_end {
            if now >= t {
                self.pending_end = None;
                return self.finish();
            }
        }
        let idle = match &self.current {
            Some(f) if !f.finished => {
                // A living boss gets a long pause (phases, untargetable). On
                // your own only while the boss is what you last hit: a mob
                // you killed next to a big monster ends the fight as usual.
                let on_boss = f.grouped || self.card_boss(f).map_or(false, |b| f.focus == Some(b)) || f.main_target().map_or(false, |t| f.focus == Some(t) && self.is_real_boss(t));
                let limit = if on_boss && self.boss_alive(f) { self.cfg.boss_idle_timeout } else { self.cfg.idle_timeout };
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
        if !self.fight_npc_boss(f, t, npc) {
            return None;
        }
        let hp = a.and_then(|a| a.hp).or(frozen.and_then(|x| x.0));
        let max_hp = a.and_then(|a| a.max_hp).or(frozen.and_then(|x| x.1))?;
        let dead = hp == Some(0) || (f.boss_killed && Some(t) == f.main_target());
        // All damage the meter saw on it, including hits from before you
        // joined in (which don't count towards anyone's DPS here), so the
        // check measures what was captured, not who was counted.
        let seen = a.map(|a| a.taken).unwrap_or(0).max(*f.targets.get(&t).unwrap_or(&0));
        let scale = self.hp_scale_of(f, t);
        (dead && max_hp > 0).then(|| seen as f64 * scale / max_hp as f64)
    }

    /// The health a monster loses per point of damage shown, when that is
    /// steady (open-world monsters: 4.5), else 1. Steady means most stretches
    /// between health updates agree within 1%: where hits go missing (other
    /// players' hits the game doesn't send) the ratio jumps around instead.
    pub fn hp_scale(a: &ActorInfo) -> f64 {
        let r = &a.hp_ratios;
        if r.len() < 5 {
            return 1.0;
        }
        let mut sorted: Vec<f32> = r.clone();
        sorted.sort_by(|x, y| x.total_cmp(y));
        let mid = sorted[sorted.len() / 2] as f64;
        let close = r.iter().filter(|x| ((**x as f64) - mid).abs() <= mid * 0.01).count();
        if mid >= 1.05 && close as f64 >= r.len() as f64 * 0.6 {
            mid
        } else {
            1.0
        }
    }

    fn hp_scale_of(&self, f: &Fight, t: u64) -> f64 {
        f.tscale.get(&t).copied().or_else(|| self.actors.get(&t).map(Self::hp_scale)).unwrap_or(1.0)
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
    /// A party member of this class that no player on screen has the name of.
    fn party_has_unmatched(&self, class: &str) -> bool {
        self.party_member_codes.iter().any(|(name, code)| {
            Some(name) != self.self_name.as_ref()
                && self.names.party_class(*code).map_or(false, |c| c == class)
                && !self.actors.values().any(|a| Self::is_class_player(a) && a.name.as_deref() == Some(name.as_str()))
        })
    }

    /// A boss or elite, also a dungeon version known only by its name.
    pub fn is_boss_actor(&self, id: u64) -> bool {
        self.actors.get(&id).and_then(|a| a.npc_id).map_or(false, |n| {
            self.npc_is_boss(n) || self.names.npc_ref(n, &self.actor_name(id)).map_or(false, |r| self.npc_is_boss(r))
        })
    }

    /// A boss (Heroic and up, or listed), not an Elite: its death ends the
    /// fight. Elites die every few seconds in open-world events.
    fn is_real_boss(&self, id: u64) -> bool {
        let real = |n: u32| self.names.bosses.contains(&n) || self.names.ranks.get(&n).map_or(false, |r| rank_is_boss_or_elite(r) && !r.to_ascii_lowercase().starts_with("elite"));
        self.actors.get(&id).and_then(|a| a.npc_id).map_or(false, |n| real(n) || self.names.npc_ref(n, &self.actor_name(id)).map_or(false, real))
    }

    /// Like `is_real_boss`, for a fight's target also after its actor is
    /// gone (a fight in the history after a zone change).
    fn fight_real_boss(&self, f: &Fight, id: u64) -> bool {
        let real = |n: u32| self.names.bosses.contains(&n) || self.names.ranks.get(&n).map_or(false, |r| rank_is_boss_or_elite(r) && !r.to_ascii_lowercase().starts_with("elite"));
        self.actors.get(&id).and_then(|a| a.npc_id).or(f.tinfo.get(&id).and_then(|x| x.2)).map_or(false, |n| real(n) || self.names.npc_ref(n, &self.fname(f, id)).map_or(false, real))
    }

    pub fn npc_is_boss(&self, npc: u32) -> bool {
        self.names.bosses.contains(&npc) || self.names.ranks.get(&npc).map_or(false, |r| rank_is_boss_or_elite(r))
    }

    /// Like `npc_is_boss`, also borrowing the rank by name for a variant
    /// with none of its own, as the card does (Grandios 2345107 at the
    /// Dimensional Invasion, 2026-10-27: Heroic on the card, but no World
    /// Boss view).
    fn fight_npc_boss(&self, f: &Fight, t: u64, npc: u32) -> bool {
        self.npc_is_boss(npc) || self.names.npc_ref(npc, &self.fname(f, t)).map_or(false, |r| self.npc_is_boss(r))
    }

    /// The boss or elite that took the most damage in this fight, if any.
    pub fn boss_target(&self, f: &Fight) -> Option<u64> {
        f.targets
            .iter()
            .filter(|(t, _)| {
                let npc = self.actors.get(t).and_then(|a| a.npc_id).or(f.tinfo.get(t).and_then(|x| x.2));
                npc.map_or(false, |n| self.fight_npc_boss(f, **t, n))
            })
            .max_by_key(|(_, d)| **d)
            .map(|(t, _)| *t)
    }

    /// The boss the target card shows: of the bosses and elites hit in this
    /// fight, the one with the most health, so an elite add (Urugugu Patrol,
    /// 232k) never takes the card from the boss (5.2M) it was summoned for.
    fn card_boss(&self, f: &Fight) -> Option<u64> {
        f.targets
            .keys()
            .filter_map(|t| {
                let a = self.actors.get(t);
                let npc = a.and_then(|a| a.npc_id).or(f.tinfo.get(t).and_then(|x| x.2))?;
                if !self.fight_npc_boss(f, *t, npc) {
                    return None;
                }
                let max = a.and_then(|a| a.max_hp).or(f.tinfo.get(t).and_then(|x| x.1)).unwrap_or(0);
                Some((max, f.targets[t], *t))
            })
            .max()
            .map(|(_, _, t)| t)
    }

    /// True while the fight's main target is a known boss that is not dead.
    fn boss_alive(&self, f: &Fight) -> bool {
        let Some(t) = f.main_target() else { return false };
        let Some(a) = self.actors.get(&t) else { return false };
        // Any real boss (our list, a gaming.tools rank, or one borrowed by
        // name for a dungeon variant), not just our list: Transcendent
        // Bakarma got the 8 s pause and one try became three fights.
        let is_boss = a.npc_id.map_or(false, |n| self.names.bosses.contains(&n)) || self.is_real_boss(t);
        is_boss && !f.boss_killed && a.hp != Some(0)
    }

    /// Ends the current fight and clears the screen (the ↺ button and the
    /// "reset" hotkey); the fight is still returned for history.
    pub fn clear(&mut self) -> Option<Fight> {
        let f = self.finish();
        self.cleared = true;
        f
    }

    /// Ends the current fight.
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
                f.tscale.insert(*t, Self::hp_scale(a));
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

    // ------------------------------------------------------------ party names

    /// Names players the game hasn't named yet from the party list. In a
    /// dungeon the game may send a member's name late or never, but the party
    /// list has every member with a class number, and a fighter's class is
    /// known from their skills. A name is given only when the match is
    /// certain: every unclaimed member's number is known, there are no more
    /// unnamed players than unclaimed members (so not strangers in the open
    /// world), and exactly one member and one fighter share the class.
    /// Numbers are learned from members the game did name, and from you.
    fn bind_party_names(&mut self) {
        if self.party_member_codes.is_empty() {
            return;
        }
        // Learn: party members whose fighter already has a name.
        let mut seen: Vec<(u32, String)> = Vec::new();
        for a in self.actors.values() {
            let name = if Some(a.id) == self.self_id { self.self_name.as_ref().or(a.name.as_ref()) } else { a.name.as_ref() };
            if let (Some(code), Some(class)) = (name.and_then(|n| self.party_member_codes.get(n)), self.actor_class(a.id)) {
                seen.push((*code, class));
            }
        }
        for (code, class) in seen {
            match self.names.party_class(code).cloned() {
                Some(c) if c != class => {
                    self.bad_codes.insert(code);
                }
                _ => {
                    self.names.party_codes.insert(code, class);
                }
            }
        }
        // Your own name not sent yet (meter started after login): you're in
        // the party list, so the one member of your class is you.
        if self.self_name.is_none() {
            if let (Some(me), Some(mine)) = (self.self_id, self.self_id.and_then(|id| self.actor_class(id))) {
                // Names on your skills' helper objects don't count, only players.
                let taken: std::collections::HashSet<&String> = self.actors.values().filter(|a| a.id != me && Self::is_class_player(a)).filter_map(|a| a.name.as_ref()).collect();
                let mut doubt = false;
                let mut same: Vec<String> = Vec::new();
                for (n, code) in &self.party_member_codes {
                    if taken.contains(n) {
                        continue;
                    }
                    match self.names.party_class(*code).filter(|_| !self.bad_codes.contains(code)) {
                        Some(c) if *c == mine => same.push(n.clone()),
                        Some(_) => {}
                        None => doubt = true,
                    }
                }
                if !doubt && same.len() == 1 {
                    let name = same.remove(0);
                    self.self_name = Some(name.clone());
                    self.actor(me).name = Some(name.clone());
                    self.backfill_name(me, &name);
                }
            }
        }
        // Players fighting on your side in this fight (not its targets).
        let Some(f) = self.current.as_ref().filter(|f| !f.finished) else { return };
        // An enemy takes more damage than it deals; a party member can show up
        // as a "target" of a stray friendly effect but deals far more. A
        // summon's or ground skill's damage counts for its owner.
        let on_side: std::collections::HashSet<u64> = f
            .stats
            .iter()
            .filter(|(id, s)| (s.dmg > 0 || s.heal > 0) && f.targets.get(id).map_or(true, |taken| *taken < s.dmg + s.heal))
            .map(|(id, _)| self.actors.get(id).and_then(|a| a.owner).unwrap_or(*id))
            .collect();
        // Members not yet tied to a fighter, by the class their number means.
        // Only a real fighter (a player with a class) holds a name: the game
        // also labels buffs and effects with their caster's name under other ids.
        let claimed: std::collections::HashSet<String> = self
            .actors
            .values()
            .filter(|a| Self::is_class_player(a))
            .filter_map(|a| a.name.clone())
            .chain(self.self_name.clone())
            .collect();
        let mut free: HashMap<String, Vec<String>> = HashMap::new();
        let mut unknown: Vec<(String, u32)> = Vec::new();
        let mut unclaimed = 0;
        for (n, code) in &self.party_member_codes {
            if claimed.contains(n) {
                continue;
            }
            unclaimed += 1;
            match self.names.party_class(*code).filter(|_| !self.bad_codes.contains(code)) {
                Some(class) => free.entry(class.clone()).or_default().push(n.clone()),
                None => unknown.push((n.clone(), *code)),
            }
        }
        // Your own name not known yet (meter started mid-run): the one member
        // of your class is you.
        if self.self_name.is_none() {
            if let Some(mine) = self.self_id.and_then(|id| self.actor_class(id)) {
                if free.get(&mine).map_or(false, |m| m.len() == 1) {
                    free.remove(&mine);
                    unclaimed -= 1;
                }
            }
        }
        // Unnamed players, by class.
        let mut fighters: HashMap<String, Vec<u64>> = HashMap::new();
        let mut unnamed = 0;
        let mut classless = false;
        for a in self.actors.values() {
            let player = a.kind == ActorKind::Player || Self::is_class_player(a);
            if !player || a.name.is_some() || Some(a.id) == self.self_id || !on_side.contains(&a.id) {
                continue;
            }
            unnamed += 1;
            match self.actor_class(a.id) {
                Some(class) => fighters.entry(class).or_default().push(a.id),
                None => classless = true,
            }
        }
        if unnamed == 0 || unnamed > unclaimed {
            return;
        }
        // Members whose number isn't known: when every unclaimed member is
        // fighting and every fighter's class is known, counting decides. For
        // each class, fighters minus known members is how many unknown members
        // have it; a class where that is 0 is fully accounted for, so its
        // known member can be named. With one unknown member its class is
        // the one with a fighter to spare, learned for next time.
        let mut settled: Option<std::collections::HashSet<String>> = None;
        if !unknown.is_empty() {
            if unnamed != unclaimed || classless {
                return; // no match is certain
            }
            let classes: std::collections::HashSet<&String> = fighters.keys().chain(free.keys()).collect();
            let diff: HashMap<String, i64> = classes
                .into_iter()
                .map(|c| (c.clone(), fighters.get(c).map_or(0, |f| f.len() as i64) - free.get(c).map_or(0, |m| m.len() as i64)))
                .collect();
            if diff.values().any(|d| *d < 0) || diff.values().sum::<i64>() != unknown.len() as i64 {
                return; // the counts don't add up: something unseen, don't guess
            }
            let spare: Vec<&String> = diff.iter().filter(|(_, d)| **d > 0).map(|(c, _)| c).collect();
            if let ([class], [(name, code)]) = (spare.as_slice(), unknown.as_slice()) {
                self.names.party_codes.insert(*code, (*class).clone());
                free.entry((*class).clone()).or_default().push(name.clone());
            } else {
                settled = Some(diff.into_iter().filter(|(_, d)| *d == 0).map(|(c, _)| c).collect());
            }
        }
        let mut bind: Vec<(u64, String)> = Vec::new();
        for (class, members) in &free {
            if settled.as_ref().map_or(false, |s| !s.contains(class)) {
                continue;
            }
            if let (Some(ids), 1) = (fighters.get(class), members.len()) {
                if ids.len() == 1 {
                    bind.push((ids[0], members[0].clone()));
                }
            }
        }
        for (id, name) in bind {
            self.actor(id).name = Some(name.clone());
            self.backfill_name(id, &name);
        }
    }

    /// A fighter using a player class's skills is a player, even before the
    /// game has said so (a Sorcerer's first hits are often ground skills).
    fn is_class_player(a: &ActorInfo) -> bool {
        a.class.is_some() && matches!(a.kind, ActorKind::Player | ActorKind::Unknown)
    }

    /// A player's name arrived late: fill it into this zone's earlier fights,
    /// which froze them as "Player #id".
    fn backfill_name(&mut self, id: u64, name: &str) {
        let placeholder = if Some(id) == self.self_id { "You".to_string() } else { format!("Player #{id}") };
        let first = self.zone_first_fight;
        for f in self.history.iter_mut().chain(self.current.iter_mut()) {
            if f.id < first {
                continue;
            }
            if let Some((n, _)) = f.names.get_mut(&id) {
                if *n == placeholder {
                    *n = name.to_string();
                }
            }
        }
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
        self.current.as_ref().filter(|f| !(self.cleared && f.finished)).or_else(|| self.history.back().filter(|_| !self.cleared))
    }

    /// Whether the meter is missing data that a zone change would bring:
    /// no zone load seen yet, and the fight on screen has a monster the game
    /// never introduced ("Unknown monster"). Dungeons introduce monsters as
    /// you go and send no zone-load message, so "no zone load seen" alone
    /// isn't a reason to ask for a teleport.
    /// Show the party panel: in a party, not fighting, and no fight since
    /// the line-up last changed (the lobby, the queue filling up), unless
    /// you have changed zone since (teleported into the dungeon).
    pub fn party_lobby(&self) -> bool {
        let Some(p) = &self.party_info else { return false };
        let fighting = self.current.as_ref().map_or(false, |f| !f.finished);
        // Outside a dungeon a party joined during the fight counts too:
        // invited while you finished a mob, the panel used to wait for the
        // next change. Inside, lists come mid-pull and the panel stays away.
        p.members.len() > 1 && !fighting && p.since > self.zone_at && self.display_fight().map_or(true, |f| if p.inside { f.end <= p.since } else { f.start <= p.since })
    }

    /// Your party sits in a dungeon room outside the dungeon: queueing for
    /// it (open-world parties have no room title).
    pub fn party_queueing(&self) -> bool {
        self.party_info.as_ref().map_or(false, |p| p.title.is_some() && !p.inside && !p.members.is_empty())
    }

    /// You hit or healed in the last 8 s (a fight nearby that only your
    /// party is in doesn't count). Also in a fight that just ended, so the
    /// panel doesn't flash between two pulls.
    pub fn self_active(&self) -> bool {
        let Some(id) = self.self_id else { return false };
        self.current.iter().chain(self.history.back()).any(|f| f.stats.get(&id).map_or(false, |s| self.last_ts - s.last < 8.0))
    }

    /// Queueing and no hit or heal of yours for 8 s: the Ready panel, even
    /// with your party fighting nearby (2026-10-26, room "mai" next to
    /// Addicted Haridrun). It used to wait for the room to change after
    /// your last hit, so it came late or not at all (2026-10-27).
    pub fn party_queue_panel(&self) -> bool {
        // Just you in the room (the others left): nothing to show.
        self.party_queueing() && !self.self_active() && self.party_info.as_ref().map_or(false, |p| p.members.len() > 1)
    }

    /// This player is in your zone: seen there, or fought in one of its
    /// fights. The game leaves
    /// the "in dungeon" bit off for some members (Bluemonday, another
    /// server, joined mid-run, 2026-10-26: "on the way" all run).
    pub fn fought_here(&self, name: &str) -> bool {
        // Seen in this zone at all: their character or a skill object that
        // carries their name (entity ids are cleared on every zone change).
        if self.actors.values().any(|a| matches!(a.kind, ActorKind::Player | ActorKind::Summon) && a.name.as_deref() == Some(name)) {
            return true;
        }
        let first = self.zone_first_fight;
        self.history.iter().chain(self.current.iter()).filter(|f| f.id >= first).any(|f| {
            f.stats.iter().any(|(id, st)| (st.dmg > 0 || st.heal > 0) && (f.names.get(id).map_or(false, |(n, _)| n == name) || self.actors.get(id).and_then(|a| a.name.as_deref()) == Some(name)))
        })
    }

    /// Joined while you were inside, and no fight has started since.
    pub fn party_is_new(&self, name: &str) -> bool {
        let Some(p) = &self.party_info else { return false };
        p.joined.iter().any(|(n, t)| n == name && self.display_fight().map_or(true, |f| f.start < *t))
    }

    /// Left while you were inside, in the last 10 s (shown faded, then gone).
    pub fn party_recently_left(&self) -> Vec<&crate::events::PartyMember> {
        let Some(p) = &self.party_info else { return Vec::new() };
        p.left.iter().filter(|(_, t)| self.last_ts - t < 10.0).map(|(m, _)| m).collect()
    }

    /// Show the party in the DPS rows with dashes: in a party, after the
    /// teleport into a dungeon and before the first fight there (the meter
    /// is ready). Not after leaving one: that party may be gone.
    pub fn party_ready(&self) -> bool {
        let Some(p) = &self.party_info else { return false };
        let fighting = self.current.as_ref().map_or(false, |f| !f.finished);
        p.members.len() > 1 && p.inside && !fighting && !self.party_lobby() && self.display_fight().map_or(true, |f| f.end <= self.entry_at)
    }

    pub fn needs_zone(&self) -> bool {
        if self.self_id.is_none() {
            return false;
        }
        self.display_fight().map_or(false, |f| {
            // After a zone load the game introduced everything, so an older
            // fight's unknown monster doesn't count. One still unknown in a
            // fight since then (back from the Shugo Festival, say) does.
            !self.fresh_view
                && f.targets.keys().any(|t| self.actors.get(t).map_or(false, |a| a.npc_id.is_none() && a.kind == ActorKind::Npc))
        })
    }

    /// Overlay table for a fight. `target` restricts totals to one target.
    pub fn fight_table(&self, f: &Fight, target: Option<u64>) -> Value {
        // The clock. Boss fight (the default when a boss or elite is in the
        // fight): damage on every enemy from the first hit on the boss to the
        // kill, adds included, divided by that time, like an encounter in
        // Details! or ACT. Trash and walking before the pull are left out.
        // On your own, a big monster you farm between mobs isn't an
        // encounter: when over a quarter of the damage from the pull on went
        // into other monsters, the whole fight is the clock (else the mobs
        // you turn to drop out of the DPS). Trash before the pull is fine.
        let boss = if self.cfg.dps_clock == DpsClock::Boss && target.is_none() { self.boss_target(f) } else { None };
        let boss = boss.filter(|b| {
            let pull = f.tspan.get(b).map_or(f.start, |s| s.0);
            let others: u64 = f.targets.iter().filter(|(t, _)| *t != b && f.tspan.get(t).map_or(false, |s| s.1 >= pull)).map(|(_, d)| *d).sum();
            let on_boss = f.targets.get(b).copied().unwrap_or(0);
            f.grouped || others as f64 <= (on_boss + others) as f64 * 0.25
        });
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
        // A crowd boss (field boss, world boss): only damage on the boss
        // counts. Dozens of players fight the adds around it, and the game
        // only sends a small part of other players' hits there, so their add
        // damage put players who barely touched the boss near the top
        // (Watcher Kaira, 2026-10-27). A crowd = 5+ players outside your
        // party on the boss, more than a dungeon party holds: with the party
        // list not in yet (the meter started inside), a dungeon group read as
        // a crowd and Tiere showed only you. Dungeon parties, solo dungeons
        // and a boss you fight alone keep the whole fight.
        let crowd = |b: &u64| f.stats.iter().filter(|(id, st)| Some(**id) != f.self_id && st.by_target.contains_key(b) && !self.party.contains(&self.fname(f, **id))).count() >= 5;
        let field_boss = if target.is_none() && !f.grouped { self.card_boss(f).filter(|b| self.fight_real_boss(f, *b) && crowd(b)) } else { None };
        let field_window = field_boss.and_then(|b| f.tspan.get(&b).copied());
        let (clock, dur) = match (self.cfg.dps_clock, target, window) {
            (_, Some(t), _) => ("target", f.tspan.get(&t).map_or(f.duration(), |(a, z)| (z - a).max(1.0))),
            (DpsClock::Boss, None, _) if field_window.is_some() => ("boss", field_window.map_or(1.0, |(a, z)| (z - a).max(1.0))),
            (DpsClock::Boss, None, Some((a, z))) => ("boss", (z - a - paused).max(1.0)),
            (DpsClock::Personal, None, _) => ("personal", f.duration()),
            _ => ("fight", f.duration()),
        };
        let personal = clock == "personal";
        let clock_boss = field_boss.or(boss);
        let own_dur = |s: &ActorStat| if personal { (s.last - s.first).max(1.0) } else { dur };
        let dmg_of = |s: &ActorStat| match (target, span) {
            (Some(t), _) => *s.by_target.get(&t).unwrap_or(&0),
            (None, _) if field_boss.is_some() => field_boss.and_then(|b| s.by_target.get(&b)).copied().unwrap_or(0),
            (None, Some((i, _))) if clock == "boss" => live.iter().enumerate().filter(|(_, l)| **l).map(|(k, _)| s.timeline.get(i + k).copied().unwrap_or(0)).sum(),
            _ => s.dmg,
        };
        // At a world boss the game sends only part of other players' hits,
        // so strangers can't be ranked fairly: the list is you, your party
        // and your Force (user's choice, 2026-10-27).
        let force_names: std::collections::HashSet<&str> = self.force.values().map(|m| m.name.as_str()).collect();
        let keep = |id: &u64| {
            field_boss.is_none() || Some(*id) == f.self_id || {
                let n = self.fname(f, *id);
                f.group.contains(&n) || self.party.contains(&n) || force_names.contains(n.as_str())
            }
        };
        let total: u64 = f.stats.iter().filter(|(id, _)| keep(id)).map(|(_, s)| dmg_of(s)).sum();
        // Your damage on the world boss against its full health: exact, as
        // the game sends all of your own hits.
        let world_share = field_boss.and_then(|b| {
            let max = self.actors.get(&b).and_then(|a| a.max_hp).or(f.tinfo.get(&b).and_then(|x| x.1)).filter(|m| *m > 0)?;
            let mine = f.self_id.and_then(|me| f.stats.get(&me)).and_then(|s| s.by_target.get(&b)).copied().unwrap_or(0);
            Some(mine as f64 / max as f64)
        });
        let mut rows: Vec<(&u64, &ActorStat)> = f.stats.iter().filter(|(id, s)| dmg_of(s) > 0 && keep(id)).collect();
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
                    .set("in_force", force_names.contains(self.fname(f, **id).as_str()))
                    .set("dmg", d)
                    .set("dps", d as f64 / own_dur(s))
                    .set("share", if total > 0 { d as f64 / total as f64 } else { 0.0 })
                    .set("hits", s.hits)
                    .set("crit_rate", if s.hits > 0 { s.crits as f64 / s.hits as f64 } else { 0.0 })
                    .set("back_rate", if s.hits > 0 { s.back as f64 / s.hits as f64 } else { 0.0 })
                    .set("perfect_rate", if s.hits > 0 { s.perfect as f64 / s.hits as f64 } else { 0.0 })
                    .set("front_rate", if s.hits > 0 { s.front as f64 / s.hits as f64 } else { 0.0 })
                    .set("double_rate", if s.hits > 0 { s.doubles as f64 / s.hits as f64 } else { 0.0 })
                    .set("cp", if Some(**id) == f.self_id { self.self_cp } else { None })
                    .set("max_hit", s.max_hit)
                    .set("max_hit_skill", self.names.skill(s.max_hit_skill))
                    .set("heal", s.heal)
                    .set("hps", s.heal as f64 / own_dur(s))
                    .set("taken", s.taken)
                    .set("deaths", s.deaths)
                    .build()
            })
            .collect();
        // A boss or elite in the fight takes the card; otherwise it follows
        // the enemy you hit last (a healer's few hits and damage-over-time
        // ticks often land on adds), and the biggest target only when you
        // haven't hit anything yourself (spectating a fight).
        // In a party the card stays on the boss (adds, healers' ticks); on
        // your own it shows what you're fighting now: the last monster you
        // used a skill on, so a mob you turn to next to a big monster shows.
        let solo = !f.grouped;
        // The card shows what you're hitting while the fight is on (the boss
        // until you hit something); a finished party fight shows its boss.
        let main = if solo || !f.finished { target.or(f.focus).or_else(|| self.card_boss(f)) } else { target.or_else(|| self.card_boss(f)).or(f.focus) }.or_else(|| f.main_target());
        let target_v = main.map(|t| {
            let a = self.actors.get(&t);
            let frozen = f.tinfo.get(&t).copied();
            // A finished fight shows the health at its end: a monster that
            // respawns, or a boss that resets after a wipe, is back at full
            // health while the card still shows that fight.
            let (hp, max_hp) = match frozen.filter(|_| f.finished) {
                Some((h, m, _)) if h.is_some() || m.is_some() => (h, m.or(a.and_then(|a| a.max_hp))),
                _ => (a.map(|a| a.hp).or(frozen.map(|x| x.0)).flatten(), a.map(|a| a.max_hp).or(frozen.map(|x| x.1)).flatten()),
            };
            let hp_pct = match (hp, max_hp) {
                (Some(h), Some(m)) if m > 0 => Some(h as f64 / m as f64),
                _ => None,
            };
            let npc_id = a.and_then(|a| a.npc_id).or(frozen.and_then(|x| x.2));
            // HP check: once it's dead, how much of its health the meter saw.
            let counted = self.hp_counted(f, t);
            let name = self.fname(f, t);
            // Rank, family and portrait, borrowed by name for unlisted variants.
            let info = npc_id.and_then(|n| self.names.npc_ref(n, &name));
            let enrage = npc_id.and_then(|n| self.names.enrage_for(n, &name));
            Obj::new()
                .set("id", t)
                .set("counted", counted)
                .set("watched", if counted.is_some() { self.watched_whole(f, t) } else { None })
                .set("hp_scale", if counted.is_some() { Some(self.hp_scale_of(f, t)) } else { None })
                .set("name", name)
                .set("npc_id", npc_id)
                .set("art_npc", info)
                .set("named", npc_id.map_or(true, |n| self.names.npcs.contains_key(&n)) || info.is_some())
                .set("dmg_taken", *f.targets.get(&t).unwrap_or(&0))
                .set("secs", f.tspan.get(&t).map(|(a, b)| (b - a).max(1.0)))
                .set("hp_pct", hp_pct)
                .set("max_hp", max_hp)
                .set("hp", hp)
                .set("berserk", enrage.map(|e| e.0))
                .set("berserk_from", enrage.map(|e| e.1))
                .set("rank", info.and_then(|n| self.names.ranks.get(&n).cloned()))
                .set("family", info.and_then(|n| self.names.families.get(&n).cloned()))
                .build()
        });
        Obj::new()
            .set("fight_id", f.id)
            .set("live", !f.finished)
            .set("duration", f.duration())
            .set("clock", clock)
            .set("clock_secs", dur)
            .set("clock_paused", if clock == "boss" && field_boss.is_none() { paused } else { 0.0 })
            .set("clock_target", if clock == "boss" { clock_boss } else { None })
            .set("clock_npc", if clock == "boss" { clock_boss.and_then(|t| self.actors.get(&t).and_then(|a| a.npc_id).or(f.tinfo.get(&t).and_then(|x| x.2))) } else { None })
            .set("clock_target_name", if clock == "boss" { clock_boss.map(|t| self.fname(f, t)) } else { None })
            // HP check for the boss itself: its damage against its max HP, once dead.
            .set("clock_counted", if clock == "boss" { clock_boss.and_then(|b| self.hp_counted(f, b)) } else { None })
            // ...and whether the meter watched it from (nearly) full health.
            .set("clock_watched", if clock == "boss" { clock_boss.and_then(|b| self.watched_whole(f, b)) } else { None })
            // A world boss: the card says so, with your share of its health.
            .set("world_boss", field_boss.is_some())
            .set("boss_share", world_share)
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
        assert!(e.tick(95.0).is_none(), "a minute-long untargetable phase is still the same fight");
        assert!(e.tick(185.0).is_some());
    }

    /// Drana Predator (2026-10-27): one garbled health value became the
    /// monster type's maximum, "0 / 108728033.00B" on every one after it.
    #[test]
    fn impossible_health_is_ignored() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(7), owner: None, max_hp: Some(108_728_033_004_747_280) });
        e.handle(&GameEvent::Hp { ts: 1.0, id: 50, hp: 108_728_033_004_747_280, max_hp: None });
        assert_eq!(e.npc_max_hp.get(&7), None);
        e.handle(&GameEvent::Actor { id: 51, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(7), owner: None, max_hp: Some(119_700) });
        assert_eq!(e.actors[&51].max_hp, Some(119_700));
    }

    #[test]
    fn heal_to_full_is_not_an_amount() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: None });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1000) });
        e.handle(&dmg(0.0, 1, 50, 18000000, 10, 0));
        e.handle(&GameEvent::Heal { ts: 1.0, src: 1, tgt: 1, skill: 16170031, amount: 0x7fff_ffff, flags: 0x40 });
        e.handle(&GameEvent::Heal { ts: 2.0, src: 1, tgt: 1, skill: 16170031, amount: 500, flags: 0 });
        assert_eq!(e.current.as_ref().unwrap().stats[&1].heal, 500, "only the real heal counts");
    }

    #[test]
    fn a_crowd_boss_counts_only_the_boss() {
        let run = |strangers: u64| {
            let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
            e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
            e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(100000) });
            e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(888), owner: None, max_hp: Some(5000) });
            e.handle(&dmg(0.0, 1, 50, 18000000, 1000, 0));
            e.handle(&dmg(1.0, 1, 60, 18000000, 500, 0));
            for k in 0..strangers {
                let id = 2 + k;
                e.handle(&GameEvent::Actor { id, name: Some(format!("P{id}")), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
                e.handle(&dmg(2.0, id, 50, 11000000, 100, 0));
                e.handle(&dmg(3.0, id, 60, 11000000, 4000, 0));
            }
            // P3 is in your Force; P2 and P4 are strangers.
            e.handle(&GameEvent::ForceInfo { members: vec![crate::events::ForceMember { name: "P3".into(), key: 7, ..Default::default() }] });
            let f = e.current.as_ref().unwrap();
            let t = e.fight_table(f, None);
            let dmg = |n: &str| t.get("players").unwrap().as_arr().unwrap().iter().find(|r| r.str_or("name", "") == n).map_or(0.0, |r| r.f64_or("dmg", 0.0));
            (dmg("Me"), dmg("P2"), dmg("P3"), t.bool_or("world_boss", false), t.f64_or("boss_share", 0.0))
        };
        assert_eq!(run(5), (1000.0, 0.0, 100.0, true, 0.01), "a world boss: only boss damage, only you and your Force; your share of its HP");
        assert!(!run(4).3, "4 others fit in a dungeon party (its list not in yet): no world boss");
        assert_eq!(run(0), (1500.0, 0.0, 0.0, false, 0.0), "a boss you fight alone keeps its adds");
    }

    #[test]
    fn a_wipe_ends_the_try() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: None });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1000) });
        e.handle(&dmg(0.0, 1, 50, 18000000, 300, 0));
        e.handle(&GameEvent::Hp { ts: 1.0, id: 50, hp: 700, max_hp: None });
        // The boss heals a bit: same try.
        e.handle(&GameEvent::Hp { ts: 5.0, id: 50, hp: 820, max_hp: None });
        assert!(e.current.as_ref().map_or(false, |f| !f.finished));
        e.handle(&GameEvent::Hp { ts: 6.0, id: 50, hp: 700, max_hp: None });
        // Party wiped: the boss resets (seen at 99%, someone already hit it).
        let done = e.handle(&GameEvent::Hp { ts: 40.0, id: 50, hp: 990, max_hp: None });
        assert!(done.is_some(), "the try ends at the reset");
        e.handle(&dmg(45.0, 1, 50, 18000000, 10, 0));
        assert_eq!(e.current.as_ref().unwrap().total(), 10, "the next pull is a new fight");
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
        // A dungeon boss, with your party (barrier phases keep the fight open).
        let m = |n: &str| crate::events::PartyMember { name: n.into(), status: Some(0x1c), ..Default::default() };
        e.handle(&GameEvent::PartyInfo { title: Some("run".into()), leader: None, members: vec![m("Me"), m("Ally")] });
        e.handle(&GameEvent::Party { names: vec!["Me".into(), "Ally".into()], codes: vec![None, None] });
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
    fn next_mob_after_a_boss_kill_is_a_new_fight() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(10_000) });
        e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
        e.handle(&dmg(1.0, 1, 50, 18000000, 10_000, 0));
        e.handle(&GameEvent::Hp { ts: 1.0, id: 50, hp: 0, max_hp: None });
        e.handle(&GameEvent::Death { ts: 1.1, target: 50, killer: None });
        // Straight on to a mob, within the death grace.
        let done = e.handle(&dmg(2.0, 1, 60, 18000000, 100, 0)).expect("the boss fight ends");
        assert!(done.boss_killed && done.targets.contains_key(&50) && !done.targets.contains_key(&60));
        let now = e.current.as_ref().unwrap();
        assert!(now.targets.contains_key(&60) && !now.targets.contains_key(&50));
    }

    #[test]
    fn leaving_a_world_boss_for_mobs_is_a_new_fight() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(160_000_000) });
        e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(119_700) });
        e.handle(&dmg(1.0, 1, 50, 18000000, 9000, 0));
        // Your damage-over-time keeps ticking on the boss while you walk off.
        for t in [3.0, 5.0, 7.0, 9.0, 11.0, 13.0] {
            e.handle(&dmg(t, 1, 50, 18000000, 300, flags::DOT));
        }
        // A mob 12 s after your last skill on the boss: its own fight.
        let done = e.handle(&dmg(13.5, 1, 60, 18010000, 4000, 0)).expect("the boss fight ends");
        assert!(done.targets.contains_key(&50));
        let now = e.current.as_ref().unwrap();
        assert!(now.targets.contains_key(&60) && !now.targets.contains_key(&50));
        // Back and forth within a few seconds stays one fight.
        let mut g = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        g.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        g.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(160_000_000) });
        g.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(119_700) });
        g.handle(&dmg(1.0, 1, 50, 18000000, 9000, 0));
        assert!(g.handle(&dmg(4.0, 1, 60, 18010000, 4000, 0)).is_none());
    }

    /// Shugo festival (2026-10-27): another player on a Thieves Cannon (an
    /// elite you never hit) made the card follow it, and each mob you hit
    /// next read as "left the boss": three fights in a second.
    #[test]
    fn an_elite_only_others_hit_is_not_one_you_left() {
        let setup = |rank: &str| {
            let mut e = Engine::new(EngineConfig::default(), Names::from_json(&format!(r#"{{"ranks": {{"777": "{rank}"}}}}"#)).unwrap());
            e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
            e.handle(&GameEvent::Actor { id: 2, name: Some("Ally".into()), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
            e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(9_000_000) });
            for id in [60, 70, 80] {
                e.handle(&GameEvent::Actor { id, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(1_300_000) });
            }
            e
        };
        // A stranger joins you on your mob, then hits the elite.
        let mut e = setup("Elite");
        e.handle(&dmg(1.0, 1, 60, 18000000, 100_000, 0));
        e.handle(&dmg(1.2, 2, 60, 18000000, 100_000, 0));
        e.handle(&dmg(1.4, 2, 50, 18000000, 2_000_000, 0));
        assert!(e.handle(&dmg(1.6, 1, 70, 18000000, 100_000, 0)).is_none(), "your next mob stays in the fight");
        assert!(e.handle(&dmg(7.0, 1, 80, 18000000, 100_000, 0)).is_none(), "also seconds later");
        // Your open-world party on the elite: you're still on it while they are.
        let mut p = setup("Elite");
        p.handle(&GameEvent::Party { names: vec!["Me".into(), "Ally".into()], codes: vec![None, None] });
        p.handle(&dmg(1.0, 1, 60, 18000000, 100_000, 0));
        p.handle(&dmg(1.2, 2, 60, 18000000, 100_000, 0));
        p.handle(&dmg(1.4, 2, 50, 18000000, 2_000_000, 0));
        assert!(p.handle(&dmg(5.0, 1, 70, 18000000, 100_000, 0)).is_none(), "party on the elite 4 s ago");
        assert!(p.handle(&dmg(12.0, 1, 80, 18000000, 100_000, 0)).is_none(), "an elite left 10 s ago: same fight");
        // An elite on the card dying is just a kill: the fight goes on.
        let mut k = setup("Elite");
        k.handle(&dmg(1.0, 1, 50, 18000000, 9_000_000, 0));
        k.handle(&GameEvent::Death { ts: 1.0, target: 50, killer: Some(1) });
        assert!(k.tick(4.5).is_none(), "no end 3 s after an elite's death");
        assert!(k.handle(&dmg(5.0, 1, 60, 18000000, 100_000, 0)).is_none(), "the next mob: same fight");
        // A real boss only others hit: your next mob is its own fight, as before.
        let mut b = setup("Legendary");
        b.handle(&dmg(1.0, 1, 60, 18000000, 100_000, 0));
        b.handle(&dmg(1.2, 2, 60, 18000000, 100_000, 0));
        b.handle(&dmg(1.4, 2, 50, 18000000, 2_000_000, 0));
        assert!(b.handle(&dmg(1.6, 1, 70, 18000000, 100_000, 0)).is_some(), "a world boss you never hit");
    }

    /// Divine Auldor (2026-10-27): a party member died and came back under a
    /// new id and showed up twice; another one hit before the game named
    /// their new id.
    #[test]
    fn a_player_under_a_new_id_keeps_one_row() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        let player = |id: u64, name: Option<&str>| GameEvent::Actor { id, name: name.map(|n| n.into()), class: Some("Chanter".into()), kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None };
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(90_000_000) });
        e.handle(&player(2, Some("Ally")));
        e.handle(&dmg(1.0, 1, 50, 18000000, 1000, 0));
        e.handle(&dmg(1.5, 2, 50, 18000000, 500, 0));
        // Back under id 3: hits before the name arrives, then after.
        e.handle(&player(3, None));
        e.handle(&dmg(5.0, 3, 50, 18000000, 300, 0));
        e.handle(&player(3, Some("Ally")));
        e.handle(&dmg(6.0, 3, 50, 18000000, 200, 0));
        let f = e.current.as_ref().unwrap();
        assert_eq!(f.stats[&2].dmg, 1000, "one row for Ally");
        assert!(!f.stats.contains_key(&3));
        // Two auras a player out of sight placed, each carrying their name
        // (Grandios, 2026-10-27: "Jiang" twice).
        let aura = |id: u64| GameEvent::Actor { id, name: Some("Jiang".into()), class: None, kind: ActorKind::Summon, npc_id: None, owner: None, max_hp: None };
        e.handle(&aura(40));
        e.handle(&dmg(7.0, 40, 50, 17150352, 100, 0));
        e.handle(&aura(41));
        e.handle(&dmg(8.0, 41, 50, 17150352, 100, 0));
        let f = e.current.as_ref().unwrap();
        assert_eq!(f.stats.get(&40).map(|s| s.dmg), Some(200), "one row for Jiang's auras");
        assert!(!f.stats.contains_key(&41));
    }

    /// A common monster dying ends the fight 3 s later on your own (a fight
    /// per mob when farming), but not with other players in it: a spirit
    /// wave split whenever the next mob was 3 s away (2026-10-27).
    #[test]
    fn a_kill_ends_the_fight_only_on_your_own() {
        let setup = || {
            let mut e = Engine::new(EngineConfig::default(), Names::default());
            e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
            e.handle(&GameEvent::Actor { id: 2, name: Some("Stranger".into()), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
            e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(50_000) });
            e
        };
        let mut solo = setup();
        solo.handle(&dmg(1.0, 1, 60, 18000000, 50_000, 0));
        solo.handle(&GameEvent::Death { ts: 1.0, target: 60, killer: Some(1) });
        assert!(solo.tick(4.5).is_some(), "on your own: the kill ends it");
        let mut crowd = setup();
        crowd.handle(&dmg(1.0, 1, 60, 18000000, 25_000, 0));
        crowd.handle(&dmg(1.2, 2, 60, 18000000, 25_000, 0));
        crowd.handle(&GameEvent::Death { ts: 1.2, target: 60, killer: Some(2) });
        assert!(crowd.tick(4.5).is_none(), "with others: wait for the 8 s pause");
        assert!(crowd.tick(9.5).is_some(), "the pause ends it");
    }

    /// Tetrata (2026-10-27): the soldiers you kill before the boss stay a
    /// fight of their own; the boss starts a new one.
    #[test]
    fn a_boss_showing_up_starts_its_own_fight() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"ranks": {"777": "Legendary", "6": "Elite"}}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 2, name: Some("Ally".into()), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(90_000_000) });
        for id in [60, 70] {
            e.handle(&GameEvent::Actor { id, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(1_300_000) });
        }
        e.handle(&dmg(1.0, 1, 60, 18000000, 100_000, 0));
        e.handle(&dmg(1.2, 2, 60, 18000000, 100_000, 0));
        assert!(e.handle(&dmg(3.0, 1, 70, 18000000, 100_000, 0)).is_none(), "soldiers stay one fight");
        let done = e.handle(&dmg(4.0, 2, 50, 18000000, 2_000_000, 0)).expect("others on the boss: its own fight");
        assert!(done.targets.contains_key(&60) && done.targets.contains_key(&70) && !done.targets.contains_key(&50));
        // You join the boss; adds during the boss fight stay in it.
        assert!(e.handle(&dmg(4.5, 1, 50, 18000000, 100_000, 0)).is_none());
        assert!(e.handle(&dmg(5.0, 1, 60, 18000000, 100_000, 0)).is_none());
        assert!(e.current.as_ref().unwrap().targets.contains_key(&50));
        // In a crowd, also when your hit on the boss comes first.
        let mut c = Engine::new(EngineConfig::default(), Names::from_json(r#"{"ranks": {"777": "Legendary", "6": "Elite"}}"#).unwrap());
        c.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        c.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(90_000_000) });
        c.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(1_300_000) });
        c.handle(&dmg(1.0, 1, 60, 18000000, 100_000, 0));
        for (i, id) in [2u64, 3, 4].into_iter().enumerate() {
            c.handle(&GameEvent::Actor { id, name: Some(format!("P{id}")), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
            c.handle(&dmg(1.2 + i as f64 * 0.1, id, 60, 18000000, 100_000, 0));
        }
        assert!(c.handle(&dmg(3.0, 1, 50, 18000000, 100_000, 0)).is_some(), "you first on a world boss in a crowd");
    }

    /// Air Raiders Captain Tetrata (2026-10-26): an add that was the main
    /// target died, the boss took over as main target, then a hit on a new
    /// add read as "boss down, moved on" and split one boss into two fights.
    #[test]
    fn an_adds_death_is_not_the_bosss() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(90_000_000) });
        for id in [60, 70] {
            e.handle(&GameEvent::Actor { id, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500_000) });
        }
        e.handle(&dmg(1.0, 1, 60, 18000000, 500_000, 0));
        e.handle(&GameEvent::Death { ts: 1.5, target: 60, killer: Some(1) });
        assert!(e.handle(&dmg(2.0, 1, 50, 18000000, 2_000_000, 0)).is_none());
        assert!(e.handle(&dmg(6.0, 1, 70, 18000000, 4000, 0)).is_none(), "the boss lives: one fight");
        let f = e.current.as_ref().unwrap();
        assert!(f.targets.contains_key(&50) && f.targets.contains_key(&70) && !f.boss_killed);
    }

    /// An Elementalist's Earth Spirit "hits" itself with 16990004 as it
    /// spawns: that's its starting health, not healing or damage.
    #[test]
    fn a_summons_spawn_health_is_not_a_hit() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(10_000) });
        e.handle(&GameEvent::Actor { id: 9, name: None, class: None, kind: ActorKind::Summon, npc_id: Some(2920001), owner: Some(1), max_hp: None });
        e.handle(&dmg(1.0, 1, 50, 18000000, 100, 0));
        e.handle(&dmg(1.5, 9, 9, 16990004, 25_000, 0));
        let f = e.current.as_ref().unwrap();
        let me = &f.stats[&1];
        assert_eq!((me.dmg, me.heal, me.taken), (100, 0, 0), "only the real hit counts");
    }

    /// Transcendent Bakarma 2300471 (Draupnir) has no rank of its own; its
    /// open-world namesake is Heroic. Its pauses ended the try after 8 s
    /// (one try became three fights) and its wipes went unseen.
    #[test]
    fn a_boss_ranked_only_by_name_keeps_its_pauses_and_wipes() {
        let names = Names::from_json(r#"{"npcs": {"2090428": "Bakarma", "2300471": "Bakarma"}, "ranks": {"2090428": "Heroic"}}"#).unwrap();
        let mut e = Engine::new(EngineConfig::default(), names);
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(2300471), owner: None, max_hp: Some(1000) });
        e.handle(&dmg(1.0, 1, 50, 18000000, 100, 0));
        e.handle(&GameEvent::Hp { id: 50, hp: 600, max_hp: Some(1000), ts: 2.0 });
        // A 20 s phase with no hits: still the same try.
        assert!(e.tick(22.0).is_none(), "a boss pause doesn't end the try");
        e.handle(&dmg(23.0, 1, 50, 18000000, 100, 0));
        // Wipe: back to full health. That try is over.
        let done = e.handle(&GameEvent::Hp { id: 50, hp: 1000, max_hp: Some(1000), ts: 30.0 });
        assert!(done.is_some(), "the wipe ends the try");
    }

    #[test]
    fn solo_farming_next_to_a_big_monster_counts_the_mobs() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(160_000_000) });
        for (i, id) in [60u64, 61, 62].iter().enumerate() {
            e.handle(&GameEvent::Actor { id: *id, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(119_700) });
            let t = i as f64 * 6.0;
            e.handle(&dmg(t, 1, 50, 18000000, 2000, 0));
            e.handle(&dmg(t + 3.0, 1, *id, 18010000, 6000, 0));
        }
        let f = e.current.as_ref().unwrap();
        let t = e.fight_table(f, None);
        // Mostly mobs: the whole fight is the clock, so the mobs count.
        assert_eq!(t.str_or("clock", ""), "fight", "{}", t.to_string());
        // And the card shows the mob you hit last.
        assert_eq!(t.get("target").unwrap().f64_or("id", 0.0), 62.0);
    }

    #[test]
    fn a_mob_dying_next_to_a_living_boss_is_not_a_kill() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(160_000_000) });
        e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(119_700) });
        e.handle(&dmg(0.0, 1, 50, 18000000, 20_000, 0));
        e.handle(&dmg(2.0, 1, 60, 18010000, 119_700, 0)); // more damage than on the boss
        e.handle(&GameEvent::Death { ts: 2.1, target: 60, killer: Some(1) });
        let f = e.current.as_ref().unwrap();
        assert!(!f.boss_killed, "the boss is alive");
        assert!(e.tick(5.5).is_none(), "the fight goes on");
    }

    #[test]
    fn finished_fight_keeps_its_end_health() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(100_000) });
        e.handle(&dmg(0.5, 1, 50, 18000000, 1000, 0));
        e.handle(&GameEvent::Hp { ts: 0.5, id: 50, hp: 40_000, max_hp: None });
        let f = e.finish().expect("fight");
        // A wipe: the boss resets to full while the card still shows that fight.
        e.handle(&GameEvent::Hp { ts: 90.0, id: 50, hp: 100_000, max_hp: None });
        let t = e.fight_table(&f, None);
        assert_eq!(t.get("target").unwrap().f64_or("hp_pct", 0.0), 0.4, "{}", t.to_string());
    }

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

    /// Open-world monsters lose 4.5x the damage shown (seen on Corrupted
    /// Nornir, training scarecrows and the air-raid boss; the game's own
    /// Combat Analysis shows the smaller number, so DPS stays as it is).
    /// The HP check allows for a steady factor, not for a jumpy one (hits
    /// really missing), and dungeon bosses stay 1:1.
    #[test]
    fn hp_check_allows_for_steady_health_scaling() {
        let run = |factor: f64, jumpy: bool| {
            let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
            e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
            e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(45_000) });
            let mut hp: f64 = 45_000.0;
            let mut t = 0.0;
            let mut i = 0;
            while hp > 0.0 {
                let d = 1000 + (i % 3) * 250;
                e.handle(&dmg(t, 1, 50, 18000000, d, 0));
                // Unseen hits from other players make the ratio jump around.
                let extra = if jumpy && i % 2 == 0 { 3000.0 + (i as f64) * 37.0 } else { 0.0 };
                hp = (hp - d as f64 * factor - extra).max(0.0);
                e.handle(&GameEvent::Hp { ts: t + 0.05, id: 50, hp: hp as u64, max_hp: None });
                t += 1.0;
                i += 1;
            }
            e.handle(&GameEvent::Death { ts: t, target: 50, killer: Some(1) });
            let f = e.current.as_ref().unwrap();
            let tg = e.fight_table(f, None);
            let tg = tg.get("target").unwrap().clone();
            (tg.f64_or("counted", -1.0), tg.f64_or("hp_scale", -1.0))
        };
        let (c, k) = run(4.5, false);
        assert!((k - 4.5).abs() < 0.01 && c > 0.97 && c < 1.2, "open world (last hit overkills): {c} {k}");
        let (c, k) = run(1.0, false);
        assert!((k - 1.0).abs() < 1e-9 && c > 0.97 && c < 1.1, "dungeon: {c} {k}");
        let (c, k) = run(1.0, true);
        assert!((k - 1.0).abs() < 1e-9 && c < 0.6, "missing hits stay visible: {c} {k}");
    }

    #[test]
    fn ranks_count_as_boss() {
        assert!(rank_is_boss_or_elite("Elite"));
        assert!(rank_is_boss_or_elite("HeroicMonster"));
        assert!(!rank_is_boss_or_elite("Common"));
        assert!(!rank_is_boss_or_elite("NormalMonster"));
        assert!(!rank_is_boss_or_elite(""));
        for other in ["normalcitizen", "object", "Abyss Portal", "vessel"] {
            assert!(!rank_is_boss_or_elite(other), "{other}");
        }
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
        let mut f = Fight { id: 1, start: 0.0, end: 1.0, started_unix: 0.0, stats: Default::default(), targets: Default::default(), deaths: vec![], loot: vec![], zone: None, boss_killed: false, finished: true, self_id: None, names: Default::default(), allies: Default::default(), focus: None, tspan: Default::default(), tsec: Default::default(), tinfo: Default::default(), twatch: Default::default(), tscale: Default::default(), own_last: Default::default(), grouped: false, group: Default::default() };
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

    /// Conquest Vakron (2300812) isn't in the monster data, but the listed
    /// Vakron (2300582) is: the boss card borrows its rank, family and picture.
    #[test]
    fn unlisted_variants_borrow_by_name() {
        let n = Names::from_json(
            r#"{"npcs": {"2300582": "Vakron", "2100001": "Guard", "2100002": "Guard", "2300585": "Giant Honeybee"},
                "ranks": {"2300582": "Heroic", "2100001": "Common", "2100002": "Elite"},
                "families": {"2300582": "Natura", "2300585": "Fera"}}"#,
        )
        .unwrap();
        assert_eq!(n.npc_ref(2300816, "Giant Honeybee"), Some(2300585), "family without a rank");
        assert_eq!(n.npc_ref(2300582, "Vakron"), Some(2300582), "a listed id is itself");
        assert_eq!(n.npc_ref(2300812, "Vakron"), Some(2300582));
        assert_eq!(n.npc_ref(2300812, "vakron "), Some(2300582), "case and spaces don't matter");
        assert_eq!(n.npc_ref(2199999, "Guard"), None, "a name shared by different ranks");
        assert_eq!(n.npc_ref(2300812, "Unknown monster"), None);

        let mut e = Engine::new(EngineConfig::default(), n);
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: Some("Vakron".into()), class: None, kind: ActorKind::Npc, npc_id: Some(2300812), owner: None, max_hp: Some(1000) });
        e.handle(&GameEvent::Damage { ts: 0.0, src: 1, tgt: 50, skill: 18000000, amount: 10, flags: 0 });
        let t = e.fight_table(e.current.as_ref().unwrap(), None);
        let card = t.get("target").unwrap();
        assert_eq!(card.str_or("rank", ""), "Heroic");
        assert_eq!(card.str_or("family", ""), "Natura");
        assert_eq!(card.get("art_npc").and_then(|x| x.as_u64()), Some(2300582));
        assert!(card.bool_or("named", false), "no '✎ name it' for a known boss");
    }

    /// Urugugu run: the game named DarkMagik but sent Arthemis, Clairvoyant
    /// and Nemura's names late or never. The party list's class numbers name
    /// them from their first skill, and a late name fills in earlier fights.
    #[test]
    fn party_list_names_you() {
        let names = || {
            Names::from_json(r#"{"classes": {"17": "Cleric", "18": "Chanter"}, "party_class_codes": {"31": "Cleric", "35": "Chanter"}}"#).unwrap()
        };
        let run = |codes: Vec<Option<u32>>| {
            let mut e = Engine::new(EngineConfig::default(), names());
            // Meter started after login: the game never said who you are.
            e.handle(&GameEvent::SelfId { id: 1, name: None });
            e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1_000_000) });
            // One of your skills' helper objects carries your name.
            e.handle(&GameEvent::Actor { id: 900, name: Some("Me".into()), class: None, kind: ActorKind::Unknown, npc_id: None, owner: None, max_hp: None });
            e.handle(&GameEvent::Damage { ts: 1.0, src: 1, tgt: 50, skill: 17_010_000, amount: 100, flags: 0 });
            assert_eq!(e.actor_name(1), "You");
            e.handle(&GameEvent::Party { names: vec!["Arthemis".into(), "Me".into()], codes });
            e.tick(1.5);
            e
        };
        let e = run(vec![Some(35), Some(31)]);
        assert_eq!(e.actor_name(1), "Me");
        assert_eq!(e.current.as_ref().map(|f| e.fname(f, 1)).as_deref(), Some("Me"));
        // Two Clerics: can't tell which one you are.
        let e = run(vec![Some(31), Some(31)]);
        assert_eq!(e.actor_name(1), "You");
    }

    #[test]
    fn party_boss_counts_before_you_arrive() {
        let run = |party: bool| {
            let names = Names::from_json(r#"{"bosses": ["777"], "classes": {"18": "Chanter"}}"#).unwrap();
            let mut e = Engine::new(EngineConfig::default(), names);
            e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
            if party {
                // A dungeon room (an open-world party counts like you alone).
                let m = |n: &str| crate::events::PartyMember { name: n.into(), status: Some(0x0c), ..Default::default() };
                e.handle(&GameEvent::PartyInfo { title: Some("run".into()), leader: None, members: vec![m("Me"), m("Arthemis")] });
                e.handle(&GameEvent::Party { names: vec!["Me".into(), "Arthemis".into()], codes: vec![Some(31), Some(35)] });
            }
            e.handle(&GameEvent::Actor { id: 2, name: Some("Arthemis".into()), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
            e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(100_000) });
            // Your group pulls the boss; you're still on the way.
            e.handle(&GameEvent::Damage { ts: 1.0, src: 2, tgt: 50, skill: 18_010_000, amount: 5000, flags: 0 });
            // An unnamed thing using Chanter skills: Arthemis, the only
            // Chanter, is already on screen, so it's a summon, not a member.
            e.handle(&GameEvent::Actor { id: 3, name: None, class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None });
            e.handle(&GameEvent::Damage { ts: 1.5, src: 3, tgt: 50, skill: 18_020_000, amount: 70, flags: 0 });
            e.current.as_ref().map_or(0, |f| f.targets.get(&50).copied().unwrap_or(0))
        };
        assert_eq!(run(true), 5000, "party: counts from the pull, without the summon");
        assert_eq!(run(false), 0, "a stranger's boss: still ignored");
    }

    #[test]
    fn loading_in_after_the_pull_you_are_still_you() {
        let mut cfg = EngineConfig::default();
        cfg.only_my_targets = false;
        let mut e = Engine::new(cfg, Names::default());
        // The id remembered from town; the party pulls while you load in.
        e.restore_self(1, Some("Me".into()));
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(5000) });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 7, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        // You arrive with your new id and join in.
        e.handle(&GameEvent::SelfId { id: 2, name: Some("Me".into()) });
        e.handle(&GameEvent::Damage { ts: 12.0, src: 2, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        let t = e.fight_table(e.current.as_ref().unwrap(), None);
        let me = t.get("players").and_then(|p| p.as_arr()).unwrap().iter().find(|p| p.bool_or("is_self", false)).map(|p| p.str_or("name", "").to_string());
        assert_eq!(me.as_deref(), Some("Me"), "your row is marked as you");
    }

    /// From the Vakron Island run: inside the dungeon Jiii left, リリイ joined
    /// (status 0x0c, then 0x1c once inside), the leader left after the last
    /// boss, then you left the dungeon with no party list after it.
    #[test]
    fn party_inside_a_dungeon() {
        let m = |n: &str, id: u64, st: u8| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(st), ..Default::default() };
        let room = |ms: Vec<crate::events::PartyMember>| GameEvent::PartyInfo { title: Some("80k".into()), leader: Some(10), members: ms };
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        // The waiting room: Ready counts, nobody inside.
        e.handle(&room(vec![m("Lead", 10, 0x0c), m("Me", 11, 0x0e), m("Jiii", 12, 0x0e)]));
        assert!(!e.party_info.as_ref().unwrap().inside, "waiting room");
        // Inside: the lists mark everyone 0x10.
        e.handle(&room(vec![m("Lead", 10, 0x1e), m("Me", 11, 0x1e), m("Jiii", 12, 0x1e)]));
        assert!(e.party_info.as_ref().unwrap().inside, "inside");
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        e.finish();
        assert!(!e.party_lobby(), "after the boss: its result");
        // Jiii leaves: the panel comes back, Jiii shown as left.
        e.see_time(15.0);
        e.handle(&room(vec![m("Lead", 10, 0x1e), m("Me", 11, 0x1e)]));
        assert!(e.party_lobby(), "someone left inside: the panel");
        assert_eq!(e.party_recently_left().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["Jiii"]);
        // リリイ joins, on the way (no 0x10), then inside.
        e.see_time(18.0);
        e.handle(&room(vec![m("Lead", 10, 0x1e), m("Me", 11, 0x1e), m("リリイ", 13, 0x0c)]));
        assert!(e.party_lobby() && e.party_is_new("リリイ") && !e.party_is_new("Lead"));
        assert!(e.party_info.as_ref().unwrap().inside, "still inside while she's on the way");
        e.handle(&room(vec![m("Lead", 10, 0x1e), m("Me", 11, 0x1e), m("リリイ", 13, 0x1c)]));
        assert_eq!(e.party_info.as_ref().unwrap().members[2].status, Some(0x1c));
        // The next boss: リリイ is no longer new.
        e.handle(&GameEvent::Damage { ts: 40.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        e.finish();
        assert!(!e.party_is_new("リリイ"), "new until the next fight");
        e.see_time(30.0);
        assert!(e.party_recently_left().is_empty(), "the left row is gone after 10 s");
        // The leader leaves after the last boss: the panel comes back (option A).
        e.see_time(50.0);
        e.handle(&room(vec![m("Me", 11, 0x1e), m("リリイ", 13, 0x1c)]));
        assert!(e.party_lobby());
        // You leave the dungeon: no party list follows. After the teleport
        // out there's no "Ready, waiting for the first hit".
        e.handle(&GameEvent::DungeonLeft);
        e.handle(&GameEvent::SelfId { id: 2, name: Some("Me".into()) });
        assert!(!e.party_info.as_ref().unwrap().inside, "left the dungeon");
        assert!(!e.party_ready(), "no ready line after leaving the dungeon");
        assert!(e.party_info.is_some(), "the party itself is kept (you may still be in it)");
    }

    /// Report 1792829501: you invite your friends, then open a room ("LAGI")
    /// whose first list has only you; they join one by one and click Ready.
    #[test]
    fn room_you_open_alone_fills_as_people_join() {
        let m = |n: &str, id: u64| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(0x0c), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        // Open world: your new party lists just you, then an invite is accepted.
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![crate::events::PartyMember { name: "Me".into(), ..Default::default() }] });
        assert!(!e.party_lobby(), "no panel with just you");
        e.handle(&GameEvent::PartyJoin { member: crate::events::PartyMember { name: "Friend".into(), ..Default::default() } });
        assert_eq!(e.party_info.as_ref().map(|p| p.members.len()), Some(2), "the friend who accepted is added");
        assert!(e.party_lobby());
        // The room you open: only you at first.
        e.handle(&GameEvent::PartyInfo { title: Some("LAGI".into()), leader: Some(11), members: vec![m("Me", 11)] });
        assert!(!e.party_lobby(), "no panel with just you");
        e.handle(&GameEvent::PartyJoin { member: m("Friend", 20) });
        e.handle(&GameEvent::PartyJoin { member: m("Other", 30) });
        let p = e.party_info.as_ref().unwrap();
        assert_eq!((p.members.len(), p.title.as_deref(), p.leader), (3, Some("LAGI"), Some(11)));
        assert!(e.party_lobby(), "the room shows as they join");
        e.handle(&GameEvent::PartyReady { id: 20, ready: true });
        assert_eq!(e.party_info.as_ref().unwrap().members.iter().find(|m| m.name == "Friend").and_then(|m| m.status), Some(0x0e), "their Ready shows");
    }

    /// ↺: the fight ends and leaves the screen (still in history); the next
    /// hit starts a new fight that shows as usual.
    #[test]
    fn clear_empties_the_screen_until_the_next_fight() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        assert!(e.clear().is_some(), "the running fight is handed over for history");
        assert!(e.display_fight().is_none(), "nothing on screen");
        assert!(e.clear().is_none(), "pressing again with no fight is fine");
        e.handle(&GameEvent::Damage { ts: 20.0, src: 1, tgt: 50, skill: 18_000_000, amount: 50, flags: 0 });
        assert_eq!(e.display_fight().map(|f| f.start), Some(20.0), "the next fight shows");
    }

    /// The bundled A2Tools file names skills gaming.tools lacks (all seen
    /// unnamed in real fights), every variant of them, and no internal names.
    #[test]
    fn a2tools_skill_names() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/a2tools/names.json")).unwrap();
        let n = Names::from_json(&src).unwrap();
        assert_eq!(n.skill(11_440_030), "Upward Strike");
        assert_eq!(n.skill(12_440_050), "Threatening Blow");
        assert_eq!(n.skill(18_400_010), "Piercing Strike");
        assert_eq!(n.skill(16_990_002), "Water Spirit: Basic Attack");
        assert!(n.skills.values().all(|s| !s.contains("<DNT>") && !s.contains("Unused") && !s.contains('_')));
    }

    #[test]
    fn kicked_from_room_then_next_room_shows() {
        let m = |n: &str, id: u64| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(0x0c), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::PartyInfo { title: Some("80k fast run".into()), leader: Some(10), members: vec![m("Lead", 10), m("Me", 11)] });
        e.handle(&GameEvent::RoomLeft);
        assert!(e.party_info.is_none(), "kicked: the old room is gone");
        assert!(e.party.is_empty());
        e.handle(&GameEvent::PartyInfo { title: Some("new room".into()), leader: Some(20), members: vec![m("Other", 20), m("Me", 11)] });
        assert_eq!(e.party_info.as_ref().map(|p| p.members.len()), Some(2), "the next room shows");
    }

    #[test]
    fn leaving_a_waiting_room_empties_the_panel() {
        let m = |n: &str, id: u64, s: u8| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(s), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::PartyInfo { title: Some("alt akun pro 75k".into()), leader: Some(10), members: vec![m("Lead", 10, 0x0c), m("Me", 11, 0x0e)] });
        e.handle(&GameEvent::RoomExit);
        assert!(e.party_info.is_none(), "left the waiting room: the old room is gone");
        e.handle(&GameEvent::PartyInfo { title: Some("new room".into()), leader: Some(20), members: vec![m("Other", 20, 0x0c), m("Me", 11, 0x0c)] });
        assert_eq!(e.party_info.as_ref().map(|p| p.members.len()), Some(2), "the next room shows");
        // Inside a dungeon a plain exit is DungeonLeft alone: the party stays.
        e.handle(&GameEvent::PartyInfo { title: Some("new room".into()), leader: Some(20), members: vec![m("Other", 20, 0x1c), m("Me", 11, 0x1c)] });
        assert!(e.party_info.as_ref().unwrap().inside);
        e.handle(&GameEvent::DungeonLeft);
        assert!(e.party_info.is_some(), "a dungeon exit keeps the party");
        // Left the room right before leaving the dungeon: you left the party
        // ("2 runs" with cutta stayed on screen, test15 2026-10-26).
        e.handle(&GameEvent::PartyInfo { title: Some("2 runs".into()), leader: Some(20), members: vec![m("cutta", 20, 0x1e), m("Me", 11, 0x1e)] });
        e.see_time(100.0);
        e.handle(&GameEvent::RoomExit);
        e.handle(&GameEvent::DungeonLeft);
        assert!(e.party_info.is_none(), "left the party with the dungeon");
        // If the party did stay, its next list brings it back.
        e.handle(&GameEvent::PartyInfo { title: Some("2 runs".into()), leader: Some(20), members: vec![m("cutta", 20, 0x0e), m("Me", 11, 0x0e)] });
        assert_eq!(e.party_info.as_ref().map(|p| p.members.len()), Some(2));
    }

    /// Rerun (test-006): after the run you wait outside, but the room still
    /// marks the old members 0x1c/0x1e. The panel shows Ready, not "In
    /// dungeon", until the leader clicks Enter and you teleport.
    #[test]
    fn rerun_room_shows_ready_outside() {
        let m = |n: &str, id: u64, st: u8| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(st), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("DewiHijau".into()) });
        e.handle(&GameEvent::PartyInfo { title: Some("60+".into()), leader: Some(241095), members: vec![m("LilSmokeyURA", 241095, 0x1c), m("DewiHijau", 255150, 0x1e)] });
        assert!(e.party_info.as_ref().unwrap().inside, "the run");
        e.handle(&GameEvent::DungeonLeft);
        e.handle(&GameEvent::SelfId { id: 2, name: Some("DewiHijau".into()) });
        e.handle(&GameEvent::PartyInfo {
            title: Some("60+".into()),
            leader: Some(241095),
            members: vec![m("LilSmokeyURA", 241095, 0x1c), m("Palacicih", 255141, 0x1e), m("DewiHijau", 255150, 0x1e), m("Hinjyaku", 83586, 0x0e)],
        });
        let p = e.party_info.as_ref().unwrap();
        assert!(!p.inside, "waiting outside for the rerun");
        assert_eq!(p.members[1].status, Some(0x0e), "Ready, not in dungeon");
        assert!(p.left.is_empty() && p.joined.is_empty());
        // The leader clicks Enter and you teleport in: inside again.
        e.handle(&GameEvent::PartyEntering);
        e.handle(&GameEvent::SelfId { id: 3, name: Some("DewiHijau".into()) });
        assert!(e.party_info.as_ref().unwrap().inside, "the next run");
    }

    /// Two players join the room in the same second as you: either could be
    /// you, so no name (and one taken from the first entry is taken back).
    #[test]
    fn two_joins_at_once_name_nobody() {
        let m = |n: &str, id: u64| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(0x0c), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.see_time(10.0);
        e.handle(&GameEvent::RoomEntered);
        e.handle(&GameEvent::PartyJoin { member: m("Me", 1) });
        assert_eq!(e.self_name.as_deref(), Some("Me"));
        e.see_time(10.5);
        e.handle(&GameEvent::PartyJoin { member: m("Stranger", 2) });
        assert!(e.self_name.is_none(), "taken back");
        e.see_time(30.0);
        e.handle(&GameEvent::PartyJoin { member: m("Other", 3) });
        e.handle(&GameEvent::PartyJoin { member: m("Another", 4) });
        e.handle(&GameEvent::RoomEntered);
        assert!(e.self_name.is_none(), "two at once: nobody");
    }

    /// Fresh install: no teleport yet, so the meter doesn't know you. Joining
    /// a room names you (0x9710 + your 0x970b entry) and your Ready shows.
    #[test]
    fn room_join_names_you_on_a_fresh_install() {
        let m = |n: &str, id: u64| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(0x0c), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.see_time(10.0);
        // Someone else joining doesn't name you.
        e.handle(&GameEvent::PartyJoin { member: m("Other", 9) });
        e.see_time(24.0);
        e.handle(&GameEvent::RoomEntered);
        assert!(e.self_name.is_none(), "the join entry was 14 s earlier");
        e.handle(&GameEvent::PartyJoin { member: m("DewiHijau", 255150) });
        assert_eq!(e.self_name.as_deref(), Some("DewiHijau"));
        e.handle(&GameEvent::PartyInfo { title: Some("bajubesi".into()), leader: Some(1), members: vec![m("LilSmokeyURA", 1), m("DewiHijau", 255150)] });
        e.handle(&GameEvent::SelfReady { ready: true });
        assert_eq!(e.party_info.as_ref().unwrap().members[1].status, Some(0x0e), "your Ready shows");
        // A name the meter already knows is never replaced.
        e.see_time(40.0);
        e.handle(&GameEvent::RoomEntered);
        e.handle(&GameEvent::PartyJoin { member: m("Someone", 7) });
        assert_eq!(e.self_name.as_deref(), Some("DewiHijau"));
    }

    #[test]
    fn your_own_ready_click_shows() {
        let m = |n: &str, id: u64| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(0x0c), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::PartyInfo { title: Some("85k".into()), leader: Some(10), members: vec![m("Lead", 10), m("Me", 11)] });
        e.handle(&GameEvent::SelfReady { ready: true });
        assert_eq!(e.party_info.as_ref().unwrap().members[1].status, Some(0x0e));
        assert_eq!(e.party_info.as_ref().unwrap().members[0].status, Some(0x0c), "only you");
        e.handle(&GameEvent::SelfReady { ready: false });
        assert_eq!(e.party_info.as_ref().unwrap().members[1].status, Some(0x0c));
    }

    #[test]
    fn room_joins_ready_and_enter_show_at_once() {
        let m = |n: &str, id: u64, st: u8| crate::events::PartyMember { name: n.into(), id: Some(id), status: Some(st), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::PartyInfo { title: Some("Leaving now.".into()), leader: Some(10), members: vec![m("Lead", 10, 0x0c), m("Me", 11, 0x0e)] });
        // Someone joins: shown before the next full list.
        e.handle(&GameEvent::PartyJoin { member: m("New", 12, 0x0c) });
        let p = e.party_info.as_ref().unwrap();
        assert_eq!(p.members.len(), 3);
        assert_eq!(p.title.as_deref(), Some("Leaving now."), "still the room");
        assert!(e.party.contains("New"));
        // ...and clicks Ready.
        e.handle(&GameEvent::PartyReady { id: 12, ready: true });
        assert_eq!(e.party_info.as_ref().unwrap().members[2].status, Some(0x0e));
        // The leader clicks Enter dungeon.
        e.handle(&GameEvent::PartyEntering);
        assert!(e.party_info.as_ref().unwrap().entering);
        // The full list right after keeps it.
        e.handle(&GameEvent::PartyInfo { title: Some("Leaving now.".into()), leader: Some(10), members: vec![m("Lead", 10, 0x0c), m("Me", 11, 0x0e), m("New", 12, 0x0e)] });
        assert!(e.party_info.as_ref().unwrap().entering);
    }

    /// Vakron Sky Island rerun (2026-10-26): the leader clicks Enter inside
    /// after the last boss, then the game sends "party gone" and no list for
    /// minutes. The party stays, shows until the first pull, and a list in
    /// the middle of the run doesn't bring the panel back.
    #[test]
    fn a_rerun_keeps_the_party() {
        let m = |n: &str, st: u8| crate::events::PartyMember { name: n.into(), status: Some(st), ..Default::default() };
        let room = |st: u8| GameEvent::PartyInfo { title: Some("Leaving now.".into()), leader: None, members: vec![m("Lead", st), m("Me", st)] };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Party { names: vec!["Lead".into(), "Me".into()], codes: vec![None, None] });
        e.handle(&room(0x1e));
        e.handle(&GameEvent::Actor { id: 50, name: Some("Vakron".into()), class: None, kind: ActorKind::Npc, npc_id: Some(2300812), owner: None, max_hp: None });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 1, tgt: 50, skill: 1, amount: 100, flags: 0 });
        e.tick(100.0);
        assert!(!e.party_lobby(), "hidden after the boss");
        e.see_time(160.0);
        e.handle(&room(0x1c));
        e.handle(&GameEvent::PartyEntering);
        e.see_time(165.0);
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![] });
        e.handle(&GameEvent::Party { names: vec![], codes: vec![] });
        let p = e.party_info.as_ref().expect("party kept");
        assert!(p.rerun && p.inside);
        assert!(e.party_lobby(), "shown until the first pull");
        assert!(e.party.contains("Lead"));
        e.handle(&GameEvent::Damage { ts: 250.0, src: 1, tgt: 50, skill: 1, amount: 100, flags: 0 });
        e.tick(400.0);
        assert!(!e.party_info.as_ref().unwrap().rerun);
        e.see_time(450.0);
        e.handle(&room(0x1c));
        assert!(!e.party_lobby(), "no pop-up mid-run");
        // Without the Enter click first, "party gone" still clears the party.
        e.see_time(600.0);
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![] });
        assert!(e.party_info.is_none());
    }

    /// 2026-10-26: the meter started in town (your id remembered from last
    /// time, no self message since). Entering the dungeon opens a new game
    /// connection: that id change is a zone change, so the town fight ends
    /// and the Ready line can show. Without a new connection it's just the
    /// game confirming your current id.
    #[test]
    fn a_new_connection_after_a_restart_is_a_zone_change() {
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.restore_self(5, Some("Me".into()));
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 5, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        assert!(e.fought_here("Me"));
        e.see_time(12.0);
        e.handle(&GameEvent::SelfId { id: 6, name: None });
        assert!(e.current.as_ref().map_or(false, |f| !f.finished), "same connection: not a zone change");
        e.see_time(40.0);
        e.new_connection();
        e.handle(&GameEvent::SelfId { id: 7, name: Some("Me".into()) });
        assert!(e.current.as_ref().map_or(true, |f| f.finished), "the town fight ended");
        assert_eq!(e.zone_at, 40.0);
        assert!(e.fought_here("Me"), "you're in the new zone");
        assert!(!e.fought_here("Friend"), "not seen here yet");
        // Their skill object shows up (it carries their name): they're here.
        e.handle(&GameEvent::Actor { id: 80, name: Some("Friend".into()), class: None, kind: ActorKind::Summon, npc_id: Some(2950001), owner: None, max_hp: None });
        assert!(e.fought_here("Friend"));
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 41.0, src: 7, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        assert!(e.fought_here("Me") && !e.fought_here("Someone"));
    }

    /// 2026-10-26: started in town, entered the dungeon, and the game never
    /// said your name again ("You" plus a "DewaPutih" summon row). Your
    /// party list naming the remembered name confirms it's you.
    #[test]
    fn the_party_list_confirms_your_remembered_name() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.restore_self(5, Some("Me".into()));
        e.see_time(10.0);
        e.new_connection();
        e.handle(&GameEvent::SelfId { id: 6, name: None });
        assert_eq!(e.self_name, None, "a new id could be another character");
        e.handle(&GameEvent::Party { names: vec!["Other".into()], codes: vec![None] });
        assert_eq!(e.self_name, None, "not in this party: not confirmed");
        e.handle(&GameEvent::Party { names: vec!["Me".into(), "A".into()], codes: vec![None, None] });
        assert_eq!(e.self_name.as_deref(), Some("Me"));
        // Your skill objects that carry your name count for you again.
        e.handle(&GameEvent::Actor { id: 70, name: Some("Me".into()), class: None, kind: ActorKind::Summon, npc_id: Some(2950001), owner: None, max_hp: None });
        assert_eq!(e.resolve_owner(70), 6);
    }

    /// Queueing for a dungeon in town (room "mai", 2026-10-26): the panel
    /// shows once you stop hitting for 8 s; while you're hitting something
    /// it's the slim bar's turn instead.
    #[test]
    fn queueing_shows_the_room_unless_you_fight() {
        let m = |n: &str| crate::events::PartyMember { name: n.into(), status: Some(0x0c), ..Default::default() };
        let room = |ns: &[&str]| GameEvent::PartyInfo { title: Some("mai".into()), leader: None, members: ns.iter().map(|n| m(n)).collect() };
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&room(&["Me", "A"]));
        assert!(e.party_queueing());
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(5000) });
        e.see_time(10.0);
        e.handle(&GameEvent::Damage { ts: 10.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        e.see_time(12.0);
        assert!(e.self_active() && !e.party_queue_panel(), "you're fighting: the bar");
        e.see_time(25.0);
        assert!(!e.self_active());
        assert!(e.party_queue_panel(), "you stopped: the room shows (2026-10-27: came late)");
        e.handle(&room(&["Me", "A", "B"]));
        assert!(e.party_queue_panel(), "someone joined: still the room");
        // Inside the dungeon it's not a queue any more.
        e.handle(&GameEvent::PartyInfo { title: Some("mai".into()), leader: None, members: vec![crate::events::PartyMember { name: "Me".into(), status: Some(0x1c), ..Default::default() }, m("A")] });
        assert!(!e.party_queueing());
    }

    /// Urugugu, 2026-10-27: you left the party, the others left the room
    /// one by one, and the game sent no "left the dungeon". The way out read
    /// as a respawn, and a friend's new party showed "On the way".
    #[test]
    fn alone_in_the_room_the_way_out_leaves_the_dungeon() {
        let m = |n: &str, st: u8| crate::events::PartyMember { name: n.into(), status: Some(st), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.see_time(1.0);
        e.handle(&GameEvent::PartyInfo { title: Some("DPS".into()), leader: None, members: vec![m("Me", 0x1c), m("A", 0x1c)] });
        assert!(e.party_info.as_ref().unwrap().inside);
        e.handle(&GameEvent::PartyInfo { title: Some("DPS".into()), leader: None, members: vec![m("Me", 0x1c)] });
        e.see_time(20.0);
        e.handle(&GameEvent::SelfId { id: 2, name: None });
        assert!(!e.party_info.as_ref().map_or(false, |p| p.inside), "alone: you went out");
        // A friend's party in the open world: no status, not inside.
        e.see_time(80.0);
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![crate::events::PartyMember { name: "Me".into(), ..Default::default() }, crate::events::PartyMember { name: "F".into(), ..Default::default() }] });
        assert!(!e.party_info.as_ref().unwrap().inside, "a new party isn't inside");
        // Still a respawn with others in the room.
        let mut r = Engine::new(EngineConfig::default(), Names::default());
        r.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        r.see_time(1.0);
        r.handle(&GameEvent::PartyInfo { title: Some("DPS".into()), leader: None, members: vec![m("Me", 0x1c), m("A", 0x1c)] });
        r.see_time(20.0);
        r.handle(&GameEvent::SelfId { id: 2, name: None });
        assert!(r.party_info.as_ref().unwrap().inside, "died and came back: still inside");
    }

    /// Urugugu, 2026-10-25: the leader clicked Enter inside for the next
    /// run (no teleport, no "party gone"). After that run you went home: no
    /// Ready line with the old party in the open world.
    #[test]
    fn an_enter_inside_is_over_once_you_fight() {
        let m = |n: &str| crate::events::PartyMember { name: n.into(), status: Some(0x1e), ..Default::default() };
        let room = GameEvent::PartyInfo { title: Some("Daeva".into()), leader: None, members: vec![m("Lead"), m("Me")] };
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&room);
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        e.finish();
        e.see_time(60.0);
        e.handle(&GameEvent::PartyEntering);
        assert!(e.party_info.as_ref().unwrap().entering);
        // The next run, inside the same dungeon.
        e.handle(&GameEvent::Damage { ts: 120.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        e.finish();
        assert!(!e.party_info.as_ref().unwrap().entering, "the Enter is done with");
        // You leave the dungeon and teleport home.
        e.handle(&GameEvent::DungeonLeft);
        e.handle(&GameEvent::SelfId { id: 2, name: Some("Me".into()) });
        assert!(!e.party_info.as_ref().unwrap().inside, "home, not inside");
        assert!(!e.party_ready(), "no Ready line in the open world");
        // You invite friends in town (open-world lists, no status): a
        // normal party, nobody "on the way" (report 1792942328).
        let ow = |ns: &[&str]| GameEvent::PartyInfo { title: None, leader: None, members: ns.iter().map(|n| crate::events::PartyMember { name: (*n).into(), ..Default::default() }).collect() };
        e.handle(&ow(&["Me"]));
        e.handle(&ow(&["Me", "Friend"]));
        assert!(!e.party_info.as_ref().unwrap().inside, "open-world party, not a dungeon");
    }

    #[test]
    fn after_leaving_the_dungeon_lists_who_is_inside_is_not_your_party() {
        let m = |n: &str| crate::events::PartyMember { name: n.into(), status: Some(0x1e), ..Default::default() };
        let room = |names: &[&str]| GameEvent::PartyInfo { title: Some("Leaving now.".into()), leader: None, members: names.iter().map(|n| m(n)).collect() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&room(&["Lead", "Me", "Other"]));
        // "You left the party".
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![] });
        e.handle(&GameEvent::Party { names: vec![], codes: vec![] });
        // The dungeon keeps listing who is still inside, without you.
        e.handle(&room(&["Lead", "Other"]));
        e.handle(&GameEvent::Party { names: vec!["Lead".into(), "Other".into()], codes: vec![None, None] });
        assert!(e.party_info.is_none() && e.party.is_empty(), "no party after leaving");
        // A real party again (the general party list).
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![m("Me"), m("Friend")] });
        assert_eq!(e.party_info.as_ref().unwrap().members.len(), 2);
    }

    #[test]
    fn a_rerun_room_that_lists_you_still_counts() {
        let m = |n: &str| crate::events::PartyMember { name: n.into(), status: Some(0x1e), ..Default::default() };
        let room = |names: &[&str]| GameEvent::PartyInfo { title: Some("50k 10 run".into()), leader: None, members: names.iter().map(|n| m(n)).collect() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&room(&["Lead", "Me", "Other"]));
        // About a minute after the last boss dies (urugugu-009).
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![] });
        e.handle(&GameEvent::Party { names: vec![], codes: vec![] });
        // Everyone accepts the rerun: the same room, you included.
        e.handle(&room(&["Lead", "Me", "Other"]));
        e.handle(&GameEvent::Party { names: vec!["Lead".into(), "Me".into(), "Other".into()], codes: vec![None, None, None] });
        assert_eq!(e.party_info.as_ref().map(|p| p.members.len()), Some(3));
        assert!(e.party.contains("Other"));
    }

    #[test]
    fn after_a_boss_dies_the_card_follows_the_adds() {
        let names = Names::from_json(r#"{"bosses": ["777"]}"#).unwrap();
        let mut e = Engine::new(EngineConfig::default(), names);
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Party { names: vec!["Me".into(), "Ally".into()], codes: vec![None, None] });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1000) });
        e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 1.0, src: 1, tgt: 50, skill: 18_000_000, amount: 400, flags: 0 });
        // You hit an add during the boss: the card shows the add.
        e.handle(&GameEvent::Damage { ts: 2.0, src: 1, tgt: 60, skill: 18_000_000, amount: 100, flags: 0 });
        let t = e.fight_table(e.current.as_ref().unwrap(), None);
        assert_eq!(t.get("target").unwrap().f64_or("id", 0.0), 60.0, "what you're hitting");
        e.handle(&GameEvent::Damage { ts: 3.0, src: 1, tgt: 50, skill: 18_000_000, amount: 600, flags: 0 });
        e.handle(&GameEvent::Hp { ts: 3.0, id: 50, hp: 0, max_hp: None });
        e.handle(&GameEvent::Death { ts: 3.0, target: 50, killer: Some(1) });
        // The boss is dead; you go on with the add you'd hit before: a new fight.
        let done = e.handle(&GameEvent::Damage { ts: 4.0, src: 1, tgt: 60, skill: 18_000_000, amount: 50, flags: 0 });
        let done = done.expect("the boss fight ends");
        assert!(done.boss_killed);
        assert_eq!(e.current.as_ref().unwrap().total(), 50, "the add is its own fight");
        // The finished boss fight still shows the boss.
        let t = e.fight_table(e.history.back().unwrap(), None);
        assert_eq!(t.get("target").unwrap().f64_or("id", 0.0), 50.0);
    }

    #[test]
    fn party_panel_after_a_run_when_a_new_party_forms() {
        let m = |n: &str| crate::events::PartyMember { name: n.into(), ..Default::default() };
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        e.finish();
        // Back to town (no fighting since), then a new party: real times, not the last hit's.
        e.see_time(50.0);
        e.handle(&GameEvent::SelfId { id: 2, name: Some("Me".into()) });
        e.see_time(60.0);
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![m("Me"), m("New")] });
        assert!(e.party_lobby(), "the waiting room shows the party panel");
    }

    #[test]
    fn party_panel_shows_in_the_lobby_until_the_first_fight() {
        let m = |n: &str| crate::events::PartyMember { name: n.into(), ..Default::default() };
        let info = |names: &[&str]| GameEvent::PartyInfo { title: Some("DPS".into()), leader: None, members: names.iter().map(|n| m(n)).collect() };
        let mut e = Engine::new(EngineConfig::default(), Names::from_json(r#"{"bosses": ["777"]}"#).unwrap());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&info(&["Me", "Ally"]));
        assert!(e.party_lobby(), "in the lobby");
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 10.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        assert!(!e.party_lobby(), "fighting");
        e.finish();
        e.handle(&info(&["Me", "Ally"]));
        assert!(!e.party_lobby(), "same party after a fight: the fight stays on screen");
        e.handle(&info(&["Me", "Ally", "Third"]));
        assert!(e.party_lobby(), "someone joined");
        e.handle(&GameEvent::Damage { ts: 20.0, src: 1, tgt: 50, skill: 18_000_000, amount: 100, flags: 0 });
        e.finish();
        e.handle(&info(&["Me", "Ally"]));
        assert!(!e.party_lobby(), "someone left after the run: no panel");
        e.handle(&info(&["Me", "Ally", "Fourth"]));
        assert!(e.party_lobby(), "someone joined again");
        // Teleport into the dungeon: you get a new id.
        e.handle(&GameEvent::Damage { ts: 30.0, src: 1, tgt: 50, skill: 18_000_000, amount: 1, flags: 0 });
        e.finish();
        e.handle(&info(&["Me", "Ally", "Fourth", "Fifth"]));
        e.handle(&GameEvent::PartyEntering);
        e.handle(&GameEvent::SelfId { id: 2, name: Some("Me".into()) });
        assert!(!e.party_lobby() && e.party_ready(), "in the dungeon: the ready list");
        e.handle(&info(&["Me", "Ally", "Fourth", "Fifth"]));
        assert!(e.party_ready(), "the list resent in the dungeon changes nothing");
        e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(6), owner: None, max_hp: Some(500) });
        e.handle(&GameEvent::Damage { ts: 40.0, src: 2, tgt: 60, skill: 18_000_000, amount: 100, flags: 0 });
        assert!(!e.party_ready() && !e.party_lobby(), "first hit: the fight");
        e.finish();
        assert!(!e.party_ready(), "after it: that fight stays on screen");
        // You die and come back inside the same run: a new id, but no Enter.
        e.handle(&GameEvent::SelfId { id: 3, name: Some("Me".into()) });
        assert!(!e.party_ready(), "a respawn isn't an entry: no Ready line");
        e.handle(&GameEvent::PartyInfo { title: None, leader: None, members: vec![] });
        assert!(e.party_info.is_none() && !e.party_lobby() && !e.party_ready(), "left the party");
    }

    #[test]
    fn leaving_the_party_clears_it() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::Party { names: vec!["Me".into(), "Arthemis".into()], codes: vec![Some(31), Some(35)] });
        assert!(e.party.contains("Arthemis"));
        // "You left the party" comes as an empty list.
        e.handle(&GameEvent::Party { names: vec![], codes: vec![] });
        assert!(e.party.is_empty() && e.party_member_codes.is_empty());
    }

    #[test]
    fn party_list_names_unnamed_members() {
        let names = || {
            Names::from_json(
                r#"{"classes": {"11": "Gladiator", "15": "Sorcerer", "16": "Elementalist", "17": "Cleric", "18": "Chanter"},
                    "party_class_codes": {"6": "Gladiator", "22": "Elementalist", "26": "Sorcerer", "31": "Cleric", "35": "Chanter"}}"#,
            )
            .unwrap()
        };
        let party = || GameEvent::Party {
            names: vec!["Arthemis".into(), "Me".into(), "Clairvoyant".into(), "DarkMagik".into(), "Nemura".into()],
            codes: vec![Some(35), Some(31), Some(22), Some(26), Some(6)],
        };
        let player = |e: &mut Engine, id: u64, name: Option<&str>| {
            e.handle(&GameEvent::Actor { id, name: name.map(String::from), class: None, kind: ActorKind::Player, npc_id: None, owner: None, max_hp: None })
        };
        let mut e = Engine::new(EngineConfig::default(), names());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1_000_000) });
        player(&mut e, 3850, Some("DarkMagik"));
        for id in [5008, 1319, 6377] {
            player(&mut e, id, None);
        }
        e.handle(&party());
        // Each one's first skill gives their class (Chanter, Elementalist, Gladiator).
        for (src, skill) in [(1, 17_010_000), (3850, 15_010_000), (5008, 18_010_000), (1319, 16_010_000), (6377, 11_010_000)] {
            e.handle(&GameEvent::Damage { ts: 1.0, src, tgt: 50, skill, amount: 100, flags: 0 });
        }
        // A stray friendly effect makes Clairvoyant a "target" too; she still
        // deals far more than she takes, so she is on your side.
        e.handle(&GameEvent::Damage { ts: 1.1, src: 3850, tgt: 1319, skill: 15_020_000, amount: 5, flags: 0 });
        e.tick(1.5);
        assert_eq!(e.actor_name(5008), "Arthemis");
        assert_eq!(e.actor_name(1319), "Clairvoyant");
        assert_eq!(e.actor_name(6377), "Nemura");

        // First Urugugu run: two Gladiators, and Mikaa's number (8) never seen.
        // Counting makes Mikaa the second Gladiator; the Chanter and Sorcerer
        // are named, the two Gladiators can't be told apart and stay unnamed.
        let mut g = Engine::new(EngineConfig::default(), names());
        g.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        g.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1_000_000) });
        g.handle(&GameEvent::Party {
            names: vec!["Arthemis".into(), "Me".into(), "Prom".into(), "DarkMagik".into(), "Mikaa".into()],
            codes: vec![Some(35), Some(31), Some(6), Some(26), Some(8)],
        });
        for (src, skill) in [(1, 17_010_000), (67, 11_010_000), (2395, 11_020_000), (10435, 18_010_000), (6792, 15_010_000)] {
            if src != 1 {
                player(&mut g, src, None);
            }
            g.handle(&GameEvent::Damage { ts: 1.0, src, tgt: 50, skill, amount: 100, flags: 0 });
        }
        g.tick(1.5);
        assert_eq!(g.actor_name(10435), "Arthemis");
        assert_eq!(g.actor_name(6792), "DarkMagik");
        assert_eq!(g.actor_name(67), "Player #67");
        assert_eq!(g.actor_name(2395), "Player #2395");
        assert_eq!(g.names.party_codes.get(&8).map(String::as_str), Some("Gladiator"), "learned");

        // Open world: a stranger with no name must not be taken for a member.
        let mut w = Engine::new(EngineConfig::default(), names());
        w.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        w.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1_000_000) });
        w.handle(&party());
        for id in [5008, 1319, 6377, 3850, 9001] {
            player(&mut w, id, None);
        }
        w.handle(&GameEvent::Damage { ts: 1.0, src: 9001, tgt: 50, skill: 11_010_000, amount: 100, flags: 0 });
        w.tick(1.5);
        assert_eq!(w.actor_name(9001), "Player #9001", "five unnamed players, four unclaimed members: no guess");

        // A late name fills in the earlier fight that froze "Player #6377".
        let mut l = Engine::new(EngineConfig::default(), names());
        l.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        l.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1_000_000) });
        player(&mut l, 6377, None);
        l.handle(&GameEvent::Damage { ts: 1.0, src: 1, tgt: 50, skill: 17_010_000, amount: 50_000, flags: 0 });
        l.handle(&GameEvent::Damage { ts: 1.2, src: 6377, tgt: 50, skill: 11_010_000, amount: 50_000, flags: 0 });
        let done = l.finish().expect("fight");
        assert_eq!(l.fname(&l.history[0], 6377), "Player #6377");
        player(&mut l, 6377, Some("Nemura"));
        assert_eq!(l.fname(&l.history[0], 6377), "Nemura");
        assert_eq!(done.id, l.history[0].id);
    }

    /// The "teleport once" notice only when something is actually missing.
    #[test]
    fn zone_notice_only_for_unknown_monsters() {
        let mut e = Engine::new(EngineConfig::default(), Names::default());
        e.handle(&GameEvent::SelfId { id: 1, name: Some("Me".into()) });
        // A monster the game introduced: nothing missing, no notice (a dungeon).
        e.handle(&GameEvent::Actor { id: 50, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(777), owner: None, max_hp: Some(1000) });
        e.handle(&GameEvent::Damage { ts: 1.0, src: 1, tgt: 50, skill: 17_010_000, amount: 10, flags: 0 });
        assert!(!e.needs_zone());
        // One that was already there when the meter started: ask for a teleport.
        e.handle(&GameEvent::Actor { id: 60, name: None, class: None, kind: ActorKind::Npc, npc_id: None, owner: None, max_hp: None });
        e.handle(&GameEvent::Damage { ts: 2.0, src: 1, tgt: 60, skill: 17_010_000, amount: 10, flags: 0 });
        assert!(e.needs_zone());
        // After a zone load the game has introduced everything.
        e.handle(&GameEvent::Fresh);
        assert!(!e.needs_zone());
        // A boss's summon the game never named doesn't bring it back: the
        // meter saw this zone load, so a teleport wouldn't help.
        e.finish();
        e.handle(&GameEvent::Actor { id: 70, name: None, class: None, kind: ActorKind::Npc, npc_id: None, owner: None, max_hp: None });
        e.handle(&GameEvent::Damage { ts: 30.0, src: 1, tgt: 70, skill: 17_010_000, amount: 10, flags: 0 });
        assert!(!e.needs_zone());
    }

    /// Enrage timers are listed for the base boss; variants find them by name.
    #[test]
    fn enrage_timer_for_variants() {
        let n = Names::from_json(
            r#"{"npcs": {"2300582": "Vakron", "2301060": "Kaldrix", "9000001": "Twin", "9000002": "Twin"},
                "berserk": {"2300582": 300, "2301060": 480, "9000001": 300, "9000002": 600}}"#,
        )
        .unwrap();
        assert_eq!(n.berserk_for(2300582, "Vakron"), Some(300));
        assert_eq!(n.berserk_for(2300812, "Vakron"), Some(300), "Conquest Vakron by name");
        assert_eq!(n.berserk_for(2301999, "kaldrix"), Some(480));
        assert_eq!(n.berserk_for(9000003, "Twin"), None, "same name, different timers");
        assert_eq!(n.berserk_for(2300812, "Unknown monster"), None);
        // aion2.gg fills the gaps; our own list wins, by id and by name.
        let mut m = n.clone();
        m.merge(Names::from_aion2gg(r#"{"2300582": {"name": "Vakron", "enrage": 210, "stagger": 5},
            "2300812": {"name": "Vakron", "enrage": 300}, "2400001": {"name": "Gg Boss", "enrage": 480},
            "2400002": {"name": "Gg Twin", "enrage": 300}, "2400003": {"name": "Gg Twin", "enrage": 600},
            "2400004": {"name": "No Timer", "stagger": 5}}"#).unwrap());
        assert_eq!(m.enrage_for(2300582, "Vakron"), Some((300, "community")), "ours wins over aion2.gg");
        assert_eq!(m.enrage_for(2300812, "Vakron"), Some((300, "aion2.gg")));
        assert_eq!(m.enrage_for(2300999, "Vakron"), Some((300, "community")), "ours first by name");
        assert_eq!(m.enrage_for(2400001, "Gg Boss"), Some((480, "aion2.gg")));
        assert_eq!(m.enrage_for(2409999, "gg boss"), Some((480, "aion2.gg")), "variant by aion2.gg name");
        assert_eq!(m.enrage_for(2409999, "Gg Twin"), None, "aion2.gg names disagree");
        assert_eq!(m.enrage_for(2400004, "No Timer"), None);
        assert!(Names::from_aion2gg("{}").is_err());
        let gg = Names::from_aion2gg(include_str!("../../../definitions/aion2gg/boss-fights.json")).unwrap();
        assert!(gg.berserk_gg.len() > 300);
        // The bundled names.json parses and carries the timers.
        let bundled = Names::from_json(include_str!("../../../definitions/names.json")).unwrap();
        assert_eq!(bundled.berserk.get(&2300582), Some(&300));
    }
}
