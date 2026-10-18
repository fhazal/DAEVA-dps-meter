//! Game events produced by the protocol decoder and consumed by the engine.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Unknown,
    Player,
    Npc,
    Summon,
}

impl ActorKind {
    pub fn parse(s: &str) -> ActorKind {
        match s.to_ascii_lowercase().as_str() {
            "player" | "pc" | "1" => ActorKind::Player,
            "npc" | "monster" | "mob" | "2" => ActorKind::Npc,
            "summon" | "pet" | "3" => ActorKind::Summon,
            _ => ActorKind::Unknown,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ActorKind::Unknown => "unknown",
            ActorKind::Player => "player",
            ActorKind::Npc => "npc",
            ActorKind::Summon => "summon",
        }
    }
}

/// Damage flags. Bit meanings are assigned by the definitions file, but the
/// engine understands these common ones.
pub mod flags {
    pub const CRIT: u32 = 1 << 0;
    pub const BACK: u32 = 1 << 1;
    pub const PERFECT: u32 = 1 << 2;
    pub const PARRY: u32 = 1 << 3;
    pub const BLOCK: u32 = 1 << 4;
    pub const EVADE: u32 = 1 << 5;
    pub const PERFECT_BLOCK: u32 = 1 << 7;
    pub const DOT: u32 = 1 << 6;
    pub const DOUBLE: u32 = 1 << 8;
    pub const FRONT: u32 = 1 << 9;
    /// Bits 10..15: number of sub-hits this result stands for (0 = 1).
    pub const MULTI_SHIFT: u32 = 10;
    pub const MULTI_MASK: u32 = 0x3f;
    /// Bits 16..31 carry the cast id (+1, 0 = unknown) so hits can be
    /// grouped into casts without widening the event.
    pub const CAST_SHIFT: u32 = 16;
}

#[derive(Debug, Clone, PartialEq)]
pub enum GameEvent {
    Damage { ts: f64, src: u64, tgt: u64, skill: u32, amount: u64, flags: u32 },
    Heal { ts: f64, src: u64, tgt: u64, skill: u32, amount: u64, flags: u32 },
    Death { ts: f64, target: u64, killer: Option<u64> },
    Actor {
        id: u64,
        name: Option<String>,
        class: Option<String>,
        kind: ActorKind,
        npc_id: Option<u32>,
        owner: Option<u64>,
        max_hp: Option<u64>,
    },
    /// The local player's entity id.
    SelfId { id: u64, name: Option<String> },
    Hp { ts: f64, id: u64, hp: u64, max_hp: Option<u64> },
    Zone { ts: f64, zone_id: u32 },
    Loot { ts: f64, who: u64, item_id: u32, grade: Option<u32>, count: u32 },
    Buff { ts: f64, src: u64, tgt: u64, buff_id: u32, applied: bool, duration_ms: Option<u64> },
    /// Stats about the local player (combat power, ...).
    SelfStats { cp: Option<u64> },
    /// The game server (world) an actor belongs to.
    ActorServer { id: u64, server: u32 },
    /// Your Odyle: base amount and the extra (+N) shown in brackets.
    Odyle { cur: Option<u64>, bonus: Option<u64> },
    /// Shugo Festival Reward Keys you hold.
    Keys { cur: u64 },
    /// Names of your group (party) members, you included.
    Party { names: Vec<String> },
    /// The game just sent a full picture (login or zone change).
    Fresh,
}

impl GameEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            GameEvent::Damage { .. } => "damage",
            GameEvent::Heal { .. } => "heal",
            GameEvent::Death { .. } => "death",
            GameEvent::Actor { .. } => "actor",
            GameEvent::SelfId { .. } => "self",
            GameEvent::Hp { .. } => "hp",
            GameEvent::Zone { .. } => "zone",
            GameEvent::Loot { .. } => "loot",
            GameEvent::Buff { .. } => "buff",
            GameEvent::SelfStats { .. } => "self_stats",
            GameEvent::ActorServer { .. } => "actor_server",
            GameEvent::Odyle { .. } => "odyle",
            GameEvent::Keys { .. } => "keys",
            GameEvent::Party { .. } => "party",
            GameEvent::Fresh => "fresh",
        }
    }
}
