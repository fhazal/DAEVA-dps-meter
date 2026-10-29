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
    /// Bits 10..14: number of sub-hits this result stands for (0 = 1).
    pub const MULTI_SHIFT: u32 = 10;
    pub const MULTI_MASK: u32 = 0x1f;
    /// An effect that isn't the attacker's own skill (e.g. a boss mechanic
    /// that heals the boss whenever it's hit): counts as damage taken when a
    /// monster deals it, never as a player's damage.
    pub const NPC_ONLY: u32 = 1 << 15;
    /// Bits 16..30 carry the cast id (+1, 0 = unknown) so hits can be
    /// grouped into casts without widening the event.
    pub const CAST_SHIFT: u32 = 16;
    pub const CAST_MASK: u32 = 0x7fff;
    /// The result has the game's hit block (direction, parry, block): an
    /// attack. A party heal or buff reported as a skill result never has
    /// it (Urugugu 2026-10-26: 0 of 487; PvP hits: 134 of 148), so a hit
    /// between two players is PvP only with it.
    pub const AIMED: u32 = 1 << 31;
}

/// One member of a party list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PartyMember {
    pub name: String,
    /// Party-list class number (see names.json `party_class_ranges`).
    pub code: Option<u32>,
    pub id: Option<u64>,
    pub server: Option<u32>,
    pub gs: Option<u32>,
    pub cp: Option<u32>,
    /// Dungeon-room status byte (0x02 = Ready); none in an open-world list.
    pub status: Option<u8>,
}

/// One member of a Force list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ForceMember {
    pub name: String,
    /// Entity id in your zone (the id their hits carry).
    pub id: Option<u64>,
    /// Party-list class number (see names.json `party_class_ranges`).
    pub code: Option<u32>,
    pub server: Option<u32>,
    /// The game's key for this member; "left the Force" names only this.
    pub key: u32,
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
    /// A player's faction (the game's race id: 1 = Elyos, 2 = Asmodian).
    ActorRace { id: u64, race: u8 },
    /// Your Odyle: base amount and the extra (+N) shown in brackets.
    Odyle { cur: Option<u64>, bonus: Option<u64> },
    /// Shugo Festival Reward Keys you hold.
    Keys { cur: u64 },
    /// Names of your group (party) members, you included, and each member's
    /// class number from the same list (follows the class, see Names::party_codes).
    Party { names: Vec<String>, codes: Vec<Option<u32>> },
    /// The same party list with what the party panel shows (sent together).
    PartyInfo { title: Option<String>, leader: Option<u64>, members: Vec<PartyMember> },
    /// Someone joined the dungeon room (sent the moment they join).
    PartyJoin { member: PartyMember },
    /// The Force (several parties together, at field bosses): every member,
    /// sent when you join one. Replaces the last list.
    ForceInfo { members: Vec<ForceMember> },
    /// Someone joined your Force.
    ForceJoin { member: ForceMember },
    /// Someone left your Force: their member key (see ForceMember).
    ForceLeave { key: u32 },
    /// A room member clicked Ready (or took it back).
    PartyReady { id: u64, ready: bool },
    /// You clicked Ready (or Cancel Ready): the game doesn't send your own
    /// click as a PartyReady.
    SelfReady { ready: bool },
    /// The room leader clicked Enter dungeon (the teleport follows).
    PartyEntering,
    /// You joined someone's dungeon room.
    RoomEntered,
    /// You were removed from the dungeon room (kicked while queueing).
    RoomLeft,
    /// You left the dungeon room yourself (also sent when leaving the
    /// dungeon, just before DungeonLeft).
    RoomExit,
    /// You left the dungeon (its room closes for you).
    DungeonLeft,
    /// "Party gone" (0x8d3f): ends a dungeon party; an open-world party
    /// ignores it.
    PartyGone,
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
            GameEvent::ActorRace { .. } => "actor_race",
            GameEvent::Odyle { .. } => "odyle",
            GameEvent::Keys { .. } => "keys",
            GameEvent::Party { .. } => "party",
            GameEvent::PartyInfo { .. } => "party_info",
            GameEvent::PartyJoin { .. } => "party_join",
            GameEvent::ForceInfo { .. } => "force_info",
            GameEvent::ForceJoin { .. } => "force_join",
            GameEvent::ForceLeave { .. } => "force_leave",
            GameEvent::PartyReady { .. } => "party_ready",
            GameEvent::SelfReady { .. } => "self_ready",
            GameEvent::PartyEntering => "party_entering",
            GameEvent::RoomEntered => "room_entered",
            GameEvent::RoomLeft => "room_left",
            GameEvent::RoomExit => "room_exit",
            GameEvent::DungeonLeft => "dungeon_left",
            GameEvent::PartyGone => "party_gone",
            GameEvent::Fresh => "fresh",
        }
    }
}
