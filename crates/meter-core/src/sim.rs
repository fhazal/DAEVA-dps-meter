//! Synthetic combat for demo mode, UI work and tests.

use crate::events::{flags, ActorKind, GameEvent};

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

struct SimPlayer {
    id: u64,
    power: f64,
    crit: f64,
    skills: &'static [u32],
    next: f64,
}

pub struct Simulator {
    rng: Rng,
    players: Vec<SimPlayer>,
    boss: u64,
    boss_hp: u64,
    boss_max: u64,
    fight_start: f64,
    pause_until: f64,
    started: bool,
    loot_done: bool,
}

const PARTY: [(&str, &str, f64, f64, &[u32]); 6] = [
    ("DewaPutih", "Gladiator", 1.25, 0.32, &[1101, 1102, 1105, 1110, 0]),
    ("Seraphine", "Sorcerer", 1.4, 0.28, &[2201, 2203, 2207, 2210]),
    ("Kaelthas", "Ranger", 1.1, 0.38, &[3301, 3302, 3306, 0]),
    ("Mirelle", "Cleric", 0.45, 0.2, &[4401, 4402]),
    ("Torvald", "Templar", 0.7, 0.25, &[5501, 5503, 0]),
    ("Yuna", "Chanter", 0.6, 0.3, &[6601, 6604, 0]),
];

impl Simulator {
    pub fn new(seed: u64) -> Self {
        Simulator { rng: Rng::new(seed), players: Vec::new(), boss: 90001, boss_hp: 0, boss_max: 0, fight_start: 0.0, pause_until: 0.0, started: false, loot_done: false }
    }

    /// Advances the simulation to `now` (seconds) and returns new events.
    pub fn step(&mut self, now: f64) -> Vec<GameEvent> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(GameEvent::SelfId { id: 1001, name: Some(PARTY[0].0.into()) });
            for (i, p) in PARTY.iter().enumerate() {
                let id = 1001 + i as u64;
                out.push(GameEvent::Actor { id, name: Some(p.0.into()), class: Some(p.1.into()), kind: ActorKind::Player, npc_id: None, owner: None, max_hp: Some(40000) });
                self.players.push(SimPlayer { id, power: p.2, crit: p.3, skills: p.4, next: now + self.rng.f() });
            }
            out.push(GameEvent::Actor { id: 2005, name: None, class: None, kind: ActorKind::Summon, npc_id: None, owner: Some(1002), max_hp: None });
            self.new_boss(now, &mut out);
        }
        if now < self.pause_until {
            return out;
        }
        if self.boss_hp == 0 {
            self.new_boss(now, &mut out);
        }
        let mut order: Vec<usize> = (0..self.players.len()).collect();
        order.sort_by(|a, b| self.players[*a].next.partial_cmp(&self.players[*b].next).unwrap());
        for i in order {
            while self.players[i].next <= now && self.boss_hp > 0 {
                let t = self.players[i].next;
                let p = &self.players[i];
                let skill = p.skills[(self.rng.next() % p.skills.len() as u64) as usize];
                let base = if skill == 0 { 900.0 } else { 2200.0 + (skill % 10) as f64 * 650.0 };
                let mut amount = base * p.power * (0.8 + self.rng.f() * 0.4);
                let mut fl = 0;
                if self.rng.f() < p.crit {
                    amount *= 1.6;
                    fl |= flags::CRIT;
                }
                if self.rng.f() < 0.15 {
                    amount *= 1.2;
                    fl |= flags::BACK;
                }
                if self.rng.f() < 0.05 {
                    fl |= flags::PERFECT;
                }
                let amount = (amount as u64).min(self.boss_hp);
                let src = if p.id == 1002 && self.rng.f() < 0.2 { 2005 } else { p.id };
                let pid = p.id;
                self.boss_hp -= amount;
                out.push(GameEvent::Damage { ts: t, src, tgt: self.boss, skill: skill * 10 + 1, amount, flags: fl });
                if pid == 1004 && self.rng.f() < 0.5 {
                    let tgt = 1001 + self.rng.range(0, 5);
                    out.push(GameEvent::Heal { ts: t, src: pid, tgt, skill: 44011, amount: self.rng.range(1500, 4200), flags: 0 });
                }
                if self.rng.f() < 0.08 {
                    let tgt = 1001 + self.rng.range(0, 5);
                    out.push(GameEvent::Damage { ts: t, src: self.boss, tgt, skill: 0, amount: self.rng.range(800, 3000), flags: 0 });
                }
                self.players[i].next = t + 0.6 + self.rng.f() * 0.9;
            }
        }
        out.push(GameEvent::Hp { ts: now, id: self.boss, hp: self.boss_hp, max_hp: Some(self.boss_max) });
        if self.boss_hp == 0 {
            out.push(GameEvent::Death { ts: now, target: self.boss, killer: Some(1001) });
            if !self.loot_done {
                self.loot_done = true;
                out.push(GameEvent::Loot { ts: now + 0.5, who: 1001, item_id: 100000901, grade: Some(5), count: 1 });
            }
            self.pause_until = now + 6.0;
            for p in self.players.iter_mut() {
                p.next = self.pause_until + 0.5;
            }
        }
        let _ = self.fight_start;
        out
    }

    fn new_boss(&mut self, now: f64, out: &mut Vec<GameEvent>) {
        self.boss += 1;
        self.boss_max = 1_200_000 + self.rng.range(0, 800_000);
        self.boss_hp = self.boss_max;
        self.fight_start = now;
        self.loot_done = false;
        out.push(GameEvent::Actor { id: self.boss, name: None, class: None, kind: ActorKind::Npc, npc_id: Some(2400001 + (self.boss % 3) as u32), owner: None, max_hp: Some(self.boss_max) });
    }
}

pub const DEMO_NAMES: &str = r#"{
  "npcs": {"2400001": "Training Golem", "2400002": "Ancient Drakan Sentinel", "2400003": "Frost Wyrm Matriarch"},
  "skills": {"11011": "Ferocious Strike", "11021": "Weakening Blow", "11051": "Shockwave", "11101": "Seismic Billow",
             "22011": "Flame Bolt", "22031": "Ice Chain", "22071": "Summon Wind Spirit", "22101": "Inferno",
             "33011": "Swift Shot", "33021": "Aimed Arrow", "33061": "Arrow Storm",
             "44011": "Healing Light", "44021": "Smite", "55011": "Shield Bash", "55031": "Judgement",
             "66011": "Word of Wind", "66041": "Thunder Mantra", "1": "Auto attack"},
  "grades": {"1": "Common", "2": "Superior", "3": "Heroic", "4": "Fabled", "5": "Eternal", "6": "Mythic"},
  "items": {"100000901": {"name": "Demo Eternal Greatsword", "grade": 5}}
}"#;
