//! Definitions-driven protocol decoder.
//!
//! The wire format of the game is described in a JSON definitions file
//! (see `definitions/aion2.json`). That keeps protocol knowledge out of the
//! binary: after a game patch the community edits the file, not the code.

use crate::events::{flags, ActorKind, GameEvent};
use crate::json::{self, Value};
use crate::net::FlowKey;
use crate::wire::{Reader, WireErr};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

// ---------------------------------------------------------------- types

#[derive(Debug, Clone)]
pub enum FType {
    U8,
    U16le,
    U16be,
    U32le,
    U32be,
    U64le,
    I32le,
    F32le,
    Varint,
    Svarint,
    Skip(usize),
    /// Jump to the next occurrence of a byte pattern (minus `back` bytes).
    /// An `optional` seek doesn't fail the message when the pattern is
    /// missing: it stays put and records 1 (found) or 0 under its name, so
    /// later fields can pick another layout.
    Seek { pattern: Vec<u8>, back: usize, optional: bool },
    /// Group member names from a party list: entries start with the byte
    /// `marker`, an index, a u32 id, u16 0, u16 server, then a u8-length
    /// UTF-8 name. Reads to the end; names are joined with newlines.
    MemberNames { marker: u8, header: bool },
    /// Force members (0x9602 list, 0x960a join): each entry is an entity id
    /// u32, 4 bytes, then a "$GUID" (37 ASCII bytes), the member key u32,
    /// 2 bytes, server u16, u8-length UTF-8 name and a class number u32.
    /// Reads to the end; one "name\tclass\tid\tserver\tkey" line each.
    ForceMembers,
    Bytes(LenSpec),
    StrVarint,
    StrU16Utf16,
    StrUtf16z,
    Repeat { count: LenSpec, fields: Vec<FieldDef> },
}

#[derive(Debug, Clone)]
pub enum LenSpec {
    Fixed(usize),
    Field(String),
    Rest,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub ty: FType,
    /// Field is only present when this expression is non-zero.
    pub cond: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FVal {
    U(u64),
    I(i64),
    F(f64),
    S(String),
    B(Vec<u8>),
    List(Vec<Scope>),
}

impl FVal {
    pub fn as_u64(&self) -> u64 {
        match self {
            FVal::U(v) => *v,
            FVal::I(v) => *v as u64,
            FVal::F(v) => *v as u64,
            FVal::S(s) => s.parse().unwrap_or(0),
            FVal::B(b) => b.len() as u64,
            FVal::List(l) => l.len() as u64,
        }
    }
    pub fn as_string(&self) -> String {
        match self {
            FVal::S(s) => s.clone(),
            FVal::U(v) => v.to_string(),
            FVal::I(v) => v.to_string(),
            FVal::F(v) => v.to_string(),
            FVal::B(b) => crate::wire::hex(b),
            FVal::List(l) => format!("[{} items]", l.len()),
        }
    }
    pub fn to_json(&self) -> Value {
        match self {
            FVal::U(v) => Value::from(*v),
            FVal::I(v) => Value::from(*v),
            FVal::F(v) => Value::from(*v),
            FVal::S(s) => Value::from(s.as_str()),
            FVal::B(b) => Value::from(crate::wire::hex(b)),
            FVal::List(l) => Value::Arr(l.iter().map(scope_json).collect()),
        }
    }
}

pub type Scope = Vec<(String, FVal)>;

pub fn scope_json(s: &Scope) -> Value {
    let mut o = json::Obj::new();
    for (k, v) in s {
        o.put(k, v.to_json());
    }
    o.build()
}

fn lookup<'a>(scopes: &[&'a Scope], name: &str) -> Option<&'a FVal> {
    for s in scopes.iter().rev() {
        if let Some((_, v)) = s.iter().rev().find(|(k, _)| k == name) {
            return Some(v);
        }
    }
    None
}

// ---------------------------------------------------------------- expressions

/// Tiny expression language used in definitions:
/// `field`, `field&0x4`, `field>>8&0xff`, `field==3`, `field!=0`,
/// numeric literals (`12`, `0x1f`) and string literals (`'player'`).
#[derive(Debug, Clone)]
pub struct Expr {
    base: ExprBase,
    ops: Vec<(Op, u64)>,
    /// `a&&b&&c`: all parts must be non-zero.
    all: Vec<Expr>,
}

#[derive(Debug, Clone)]
enum ExprBase {
    Field(String),
    Num(u64),
    Str(String),
}

#[derive(Debug, Clone, Copy)]
enum Op {
    And,
    Shr,
    Shl,
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
    Add,
    Sub,
    Mul,
}

impl Expr {
    pub fn parse(src: &str) -> Result<Expr, String> {
        let s = src.trim();
        if s.contains("&&") {
            let all = s.split("&&").map(Expr::parse).collect::<Result<Vec<_>, _>>()?;
            return Ok(Expr { base: ExprBase::Num(0), ops: vec![], all });
        }
        if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
            return Ok(Expr { base: ExprBase::Str(s[1..s.len() - 1].to_string()), ops: vec![], all: vec![] });
        }
        let ops_at = |t: &str| -> Option<usize> {
            ["&", ">>", "<<", "==", "!=", ">=", "<=", ">", "<", "+", "-", "*"].iter().filter_map(|o| t.find(o)).min()
        };
        let (head, mut rest) = match ops_at(s) {
            Some(i) => (&s[..i], &s[i..]),
            None => (s, ""),
        };
        let head = head.trim();
        let base = if let Some(n) = json::parse_number_str(head).filter(|_| head.chars().next().is_some_and(|c| c.is_ascii_digit())) {
            ExprBase::Num(n as u64)
        } else if !head.is_empty() && head.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            ExprBase::Field(head.to_string())
        } else {
            return Err(format!("bad expression '{src}'"));
        };
        let mut ops = Vec::new();
        while !rest.is_empty() {
            let (op, len) = if rest.starts_with(">>") {
                (Op::Shr, 2)
            } else if rest.starts_with("<<") {
                (Op::Shl, 2)
            } else if rest.starts_with("==") {
                (Op::Eq, 2)
            } else if rest.starts_with("!=") {
                (Op::Ne, 2)
            } else if rest.starts_with(">=") {
                (Op::Ge, 2)
            } else if rest.starts_with("<=") {
                (Op::Le, 2)
            } else if rest.starts_with('>') {
                (Op::Gt, 1)
            } else if rest.starts_with('<') {
                (Op::Lt, 1)
            } else if rest.starts_with('&') {
                (Op::And, 1)
            } else if rest.starts_with('+') {
                (Op::Add, 1)
            } else if rest.starts_with('-') {
                (Op::Sub, 1)
            } else if rest.starts_with('*') {
                (Op::Mul, 1)
            } else {
                return Err(format!("bad operator in '{src}'"));
            };
            rest = &rest[len..];
            let end = ops_at(rest).unwrap_or(rest.len());
            let num = json::parse_number_str(&rest[..end]).ok_or_else(|| format!("bad number in '{src}'"))?;
            ops.push((op, num as u64));
            rest = &rest[end..];
        }
        Ok(Expr { base, ops, all: vec![] })
    }

    pub fn eval(&self, scopes: &[&Scope]) -> Option<FVal> {
        if !self.all.is_empty() {
            return Some(FVal::U(self.all.iter().all(|e| e.truthy(scopes)) as u64));
        }
        let mut v = match &self.base {
            ExprBase::Num(n) => FVal::U(*n),
            ExprBase::Str(s) => return Some(FVal::S(s.clone())),
            ExprBase::Field(f) => lookup(scopes, f)?.clone(),
        };
        for (op, n) in &self.ops {
            let x = v.as_u64();
            let n = *n;
            v = FVal::U(match op {
                Op::And => x & n,
                Op::Shr => x.checked_shr(n as u32).unwrap_or(0),
                Op::Shl => x.checked_shl(n as u32).unwrap_or(0),
                Op::Eq => (x == n) as u64,
                Op::Ne => (x != n) as u64,
                Op::Ge => (x >= n) as u64,
                Op::Le => (x <= n) as u64,
                Op::Gt => (x > n) as u64,
                Op::Lt => (x < n) as u64,
                Op::Add => x.wrapping_add(n),
                Op::Sub => x.wrapping_sub(n),
                Op::Mul => x.wrapping_mul(n),
            });
        }
        Some(v)
    }
    pub fn eval_u64(&self, scopes: &[&Scope]) -> Option<u64> {
        self.eval(scopes).map(|v| v.as_u64())
    }
    pub fn truthy(&self, scopes: &[&Scope]) -> bool {
        self.eval_u64(scopes).unwrap_or(0) != 0
    }
}

// ---------------------------------------------------------------- definitions

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthCounts {
    /// Length value covers the whole frame including the header.
    Frame,
    /// Length value covers everything after the length field.
    AfterLength,
    /// Length value covers the payload after the header.
    Payload,
}

#[derive(Debug, Clone)]
pub struct Framing {
    pub header: Vec<FieldDef>,
    pub counts: LengthCounts,
    pub adjust: i64,
    pub min_frame: usize,
    pub max_frame: usize,
    /// If set and true for a frame's header, the payload is decompressed
    /// before the message is decoded.
    pub compressed_if: Option<Expr>,
    pub compression: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dir {
    Server,
    Client,
    Any,
}

#[derive(Debug, Clone)]
pub struct Emit {
    pub event: String,
    pub each: Option<String>,
    pub map: Vec<(String, Expr)>,
    pub when: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct MessageDef {
    pub opcode: u64,
    pub name: String,
    pub dir: Dir,
    pub fields: Vec<FieldDef>,
    pub emits: Vec<Emit>,
    /// Payload is itself a sequence of frames (optionally compressed).
    pub container: bool,
    pub decompress: bool,
    pub skip: usize,
}

#[derive(Debug, Clone)]
pub struct Definitions {
    pub name: String,
    pub version: String,
    pub framing: Framing,
    pub messages: HashMap<u64, Vec<MessageDef>>,
    /// Server ports that identify game traffic (optional hint).
    pub ports: Vec<u16>,
    pub process_names: Vec<String>,
}

fn parse_ftype(v: &Value, ctx: &str) -> Result<FType, String> {
    let t = v.str_or("type", "");
    let len_spec = |key: &str| -> Result<LenSpec, String> {
        match v.get(key) {
            None => Ok(LenSpec::Rest),
            Some(Value::Num(n)) => Ok(LenSpec::Fixed(*n as usize)),
            Some(Value::Str(s)) if s == "rest" => Ok(LenSpec::Rest),
            Some(Value::Str(s)) => match json::parse_number_str(s) {
                Some(n) if s.chars().next().is_some_and(|c| c.is_ascii_digit()) => Ok(LenSpec::Fixed(n as usize)),
                _ => Ok(LenSpec::Field(s.clone())),
            },
            _ => Err(format!("{ctx}: bad '{key}'")),
        }
    };
    Ok(match t {
        "u8" => FType::U8,
        "u16" | "u16le" => FType::U16le,
        "u16be" => FType::U16be,
        "u32" | "u32le" => FType::U32le,
        "u32be" => FType::U32be,
        "u64" | "u64le" => FType::U64le,
        "i32" | "i32le" => FType::I32le,
        "f32" | "f32le" => FType::F32le,
        "varint" => FType::Varint,
        "svarint" | "zigzag" => FType::Svarint,
        "skip" => FType::Skip(v.u64_or("len", 0) as usize),
        "member_names" => FType::MemberNames { marker: v.u64_or("marker", 0x1e) as u8, header: v.u64_or("header", 1) != 0 },
        "force_members" => FType::ForceMembers,
        "seek" => {
            let pattern: Vec<u8> = v
                .str_or("pattern", "")
                .split_whitespace()
                .map(|h| u8::from_str_radix(h, 16).map_err(|_| format!("{ctx}: bad seek pattern")))
                .collect::<Result<_, _>>()?;
            if pattern.is_empty() {
                return Err(format!("{ctx}: seek needs a 'pattern'"));
            }
            FType::Seek { pattern, back: v.u64_or("back", 0) as usize, optional: v.bool_or("optional", false) }
        }
        "bytes" => FType::Bytes(len_spec("len")?),
        "str" | "str_varint" => FType::StrVarint,
        "str_u16_utf16" | "wstr" => FType::StrU16Utf16,
        "str_utf16z" | "wstrz" => FType::StrUtf16z,
        "repeat" => FType::Repeat { count: len_spec("count")?, fields: parse_fields(v.get("fields"), ctx)? },
        other => return Err(format!("{ctx}: unknown field type '{other}'")),
    })
}

/// Fields may be given as objects or as compact strings: `"name:type"`.
fn parse_fields(v: Option<&Value>, ctx: &str) -> Result<Vec<FieldDef>, String> {
    let mut out = Vec::new();
    let Some(arr) = v.and_then(|v| v.as_arr()) else { return Ok(out) };
    for (i, f) in arr.iter().enumerate() {
        let c = format!("{ctx} field #{i}");
        match f {
            Value::Str(s) => {
                let (name, ty) = s.split_once(':').ok_or_else(|| format!("{c}: expected 'name:type'"))?;
                let mut o = json::Obj::new().set("type", ty.trim());
                if let Some(n) = ty.trim().strip_prefix("skip") {
                    o = json::Obj::new().set("type", "skip").set("len", n.trim_matches(|c| c == '(' || c == ')').parse::<u64>().unwrap_or(0));
                }
                out.push(FieldDef { name: name.trim().to_string(), ty: parse_ftype(&o.build(), &c)?, cond: None });
            }
            Value::Obj(_) => {
                let cond = match f.get("if").and_then(|x| x.as_str()) {
                    Some(e) => Some(Expr::parse(e).map_err(|e| format!("{c}: {e}"))?),
                    None => None,
                };
                out.push(FieldDef { name: f.str_or("name", "_").to_string(), ty: parse_ftype(f, &c)?, cond });
            }
            _ => return Err(format!("{c}: invalid field")),
        }
    }
    Ok(out)
}

impl Definitions {
    pub fn from_json(src: &str) -> Result<Definitions, String> {
        let v = json::parse(src).map_err(|e| e.to_string())?;
        let fr = v.get("framing").ok_or("missing 'framing'")?;
        let header = parse_fields(fr.get("header"), "framing.header")?;
        if !header.iter().any(|f| f.name == "length") {
            return Err("framing.header needs a 'length' field".into());
        }
        let counts = match fr.str_or("length_counts", "frame") {
            "frame" => LengthCounts::Frame,
            "after_length" => LengthCounts::AfterLength,
            "payload" => LengthCounts::Payload,
            o => return Err(format!("unknown length_counts '{o}'")),
        };
        let compressed_if = match fr.get("compressed_if").and_then(|x| x.as_str()) {
            Some(e) => Some(Expr::parse(e)?),
            None => None,
        };
        let framing = Framing {
            header,
            counts,
            adjust: fr.f64_or("length_adjust", 0.0) as i64,
            min_frame: fr.u64_or("min_frame", 2) as usize,
            max_frame: fr.u64_or("max_frame", 1 << 20) as usize,
            compressed_if,
            compression: fr.str_or("compression", "zlib").to_string(),
        };
        let mut messages: HashMap<u64, Vec<MessageDef>> = HashMap::new();
        if let Some(arr) = v.get("messages").and_then(|m| m.as_arr()) {
            for (i, m) in arr.iter().enumerate() {
                let opcode = m.get("opcode").and_then(|o| o.as_u64()).ok_or(format!("message #{i}: missing opcode"))?;
                let name = m.str_or("name", "?").to_string();
                let ctx = format!("message {name} (0x{opcode:x})");
                let dir = match m.str_or("dir", "server") {
                    "server" | "s2c" => Dir::Server,
                    "client" | "c2s" => Dir::Client,
                    _ => Dir::Any,
                };
                let mut emits = Vec::new();
                let emit_vals: Vec<&Value> = match m.get("emit") {
                    Some(Value::Arr(a)) => a.iter().collect(),
                    Some(o @ Value::Obj(_)) => vec![o],
                    _ => vec![],
                };
                for e in emit_vals {
                    let obj = e.as_obj().unwrap();
                    let mut map = Vec::new();
                    for (k, x) in obj {
                        if matches!(k.as_str(), "event" | "each" | "when") {
                            continue;
                        }
                        let s = match x {
                            Value::Str(s) => s.clone(),
                            Value::Num(n) => format!("{}", *n as u64),
                            _ => return Err(format!("{ctx}: emit '{k}' must be a string or number")),
                        };
                        map.push((k.clone(), Expr::parse(&s).map_err(|e| format!("{ctx}: {e}"))?));
                    }
                    let when = match e.get("when").and_then(|w| w.as_str()) {
                        Some(w) => Some(Expr::parse(w)?),
                        None => None,
                    };
                    emits.push(Emit {
                        event: e.str_or("event", "").to_string(),
                        each: e.get("each").and_then(|x| x.as_str()).map(String::from),
                        map,
                        when,
                    });
                }
                let def = MessageDef {
                    opcode,
                    name,
                    dir,
                    fields: parse_fields(m.get("fields"), &ctx)?,
                    emits,
                    container: m.bool_or("container", false),
                    decompress: m.bool_or("decompress", false),
                    skip: m.u64_or("skip", 0) as usize,
                };
                messages.entry(opcode).or_default().push(def);
            }
        }
        let ports = v
            .get("ports")
            .and_then(|p| p.as_arr())
            .map(|a| a.iter().filter_map(|x| x.as_u64()).map(|x| x as u16).collect())
            .unwrap_or_default();
        let process_names = v
            .get("process_names")
            .and_then(|p| p.as_arr())
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect())
            .unwrap_or_default();
        Ok(Definitions {
            name: v.str_or("name", "unnamed").to_string(),
            version: v.str_or("version", "0").to_string(),
            framing,
            messages,
            ports,
            process_names,
        })
    }
}

// ---------------------------------------------------------------- field reader

fn read_fields(r: &mut Reader<'_>, defs: &[FieldDef], parents: &[&Scope], out: &mut Scope, depth: usize) -> Result<(), WireErr> {
    for f in defs {
        if let Some(c) = &f.cond {
            let mut sc: Vec<&Scope> = parents.to_vec();
            sc.push(out);
            if !c.truthy(&sc) {
                continue;
            }
        }
        let v = match &f.ty {
            FType::U8 => FVal::U(r.u8()? as u64),
            FType::U16le => FVal::U(r.u16le()? as u64),
            FType::U16be => FVal::U(r.u16be()? as u64),
            FType::U32le => FVal::U(r.u32le()? as u64),
            FType::U32be => FVal::U(r.u32be()? as u64),
            FType::U64le => FVal::U(r.u64le()?),
            FType::I32le => FVal::I(r.u32le()? as i32 as i64),
            FType::F32le => FVal::F(r.f32le()? as f64),
            FType::Varint => FVal::U(r.varint()?),
            FType::Svarint => FVal::I(r.svarint()?),
            FType::Skip(n) => {
                r.skip(*n)?;
                continue;
            }
            FType::MemberNames { marker, header } => {
                let b = r.rest();
                let mut names: Vec<String> = Vec::new();
                let mut i = 0;
                // An entry: [marker, slot,] id u32, 0 0, server u16, name
                // length, name, class number u32. Marker 0 = no marker and
                // slot (the open-world party list), so the server and class
                // number must look right instead.
                let h = if *marker == 0 { 0 } else { 2 };
                while i + h + 10 <= b.len() {
                    let len = b[i + h + 8] as usize;
                    let head = if *marker == 0 {
                        let server = u16::from_le_bytes([b[i + 6], b[i + 7]]);
                        (1000..=9999).contains(&server)
                    } else {
                        // Entries start 0x0c, with status bits on top: 0x02 =
                        // Ready in a dungeon room, 0x10 seen once in the
                        // dungeon (0x0c, 0x0e and 0x1e all occur).
                        b[i] | 0x12 == *marker | 0x12 && (1..=16).contains(&b[i + 1])
                    };
                    let name_at = i + h + 9;
                    if head && b[i + h + 4] == 0 && b[i + h + 5] == 0 && (2..=48).contains(&len) && name_at + len <= b.len() {
                        if let Ok(n) = std::str::from_utf8(&b[name_at..name_at + len]) {
                            // The u32 after the name follows the member's class
                            // ("Name\t22"); the engine matches it to fighters.
                            let at = name_at + len;
                            let code = b.get(at..at + 4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                            let code_ok = *marker != 0 || code.map_or(false, |c| (1..=64).contains(&c));
                            if code_ok && !n.chars().any(|c| c.is_control()) && !names.iter().any(|x| x.split('\t').next() == Some(n)) {
                                // For the party panel: id, server, gear score
                                // (after the class and the level), combat
                                // power (after a 0x04 a few bytes on) and
                                // the status byte (dungeon room only).
                                let u32at = |k: usize| b.get(k..k + 4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                                let id = u32at(i + h).unwrap_or(0);
                                let server = u16::from_le_bytes([b[i + h + 6], b[i + h + 7]]);
                                // Open-world lists have two varints (2 or 3 bytes
                                // each, e.g. `ab 7f ab 7f` or `f0 95 01 f0 95 01`)
                                // after the class where dungeon lists have the
                                // level; skip them by their real length.
                                let gs_at = if *marker == 0 {
                                    let mut k = at + 4;
                                    for _ in 0..2 {
                                        while b.get(k).map_or(false, |x| x & 0x80 != 0) {
                                            k += 1;
                                        }
                                        k += 1;
                                    }
                                    k
                                } else {
                                    at + 8
                                };
                                let gs = u32at(gs_at).filter(|g| (1..=20_000).contains(g)).unwrap_or(0);
                                let cp = (at + 12..(at + 22).min(b.len().saturating_sub(4)))
                                    .find(|j| b[*j] == 0x04 && u32at(j + 1).map_or(false, |c| (1_000..=5_000_000).contains(&c)))
                                    .and_then(|j| u32at(j + 1))
                                    .unwrap_or(0);
                                let status = if *marker == 0 { String::new() } else { b[i].to_string() };
                                let c = code.map(|c| c.to_string()).unwrap_or_default();
                                names.push(format!("{n}\t{c}\t{id}\t{server}\t{gs}\t{cp}\t{status}"));
                                i = at;
                                continue;
                            }
                        }
                    }
                    i += 1;
                }
                // The dungeon list starts with the room: its title (what the
                // leader typed) and, a few bytes on, the leader's id.
                if *marker != 0 && *header && !names.is_empty() {
                    let len = b.get(4).copied().unwrap_or(0) as usize;
                    let title = b.get(5..5 + len).and_then(|t| std::str::from_utf8(t).ok()).filter(|t| !t.chars().any(|c| c.is_control())).unwrap_or("");
                    let leader = (b.get(5 + len + 6) == Some(&0x03)).then(|| b.get(5 + len + 7..5 + len + 11)).flatten().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).unwrap_or(0);
                    names.insert(0, format!("@\t{}\t{leader}", title.replace(['\t', '\n'], " ")));
                }
                r.pos = r.buf.len();
                FVal::S(names.join("\n"))
            }
            FType::ForceMembers => {
                let b = r.rest();
                let u32_at = |i: usize| b.get(i..i + 4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                let guid = |i: usize| {
                    b.get(i..i + 37).map_or(false, |g| {
                        g[0] == b'$' && g[1..].iter().enumerate().all(|(k, c)| if [8, 13, 18, 23].contains(&k) { *c == b'-' } else { c.is_ascii_hexdigit() })
                    })
                };
                let mut lines: Vec<String> = Vec::new();
                let mut i = 8;
                while i + 37 <= b.len() {
                    if !guid(i) {
                        i += 1;
                        continue;
                    }
                    let e = i + 37;
                    let id = u32_at(i - 8).unwrap_or(0);
                    let key = u32_at(e).unwrap_or(0);
                    let server = b.get(e + 6..e + 8).map(|c| u16::from_le_bytes([c[0], c[1]])).unwrap_or(0);
                    let len = b.get(e + 8).copied().unwrap_or(0) as usize;
                    let name = b.get(e + 9..e + 9 + len).and_then(|n| std::str::from_utf8(n).ok()).filter(|n| !n.is_empty() && !n.chars().any(|c| c.is_control()));
                    if let Some(name) = name {
                        let code = u32_at(e + 9 + len).unwrap_or(0);
                        lines.push(format!("{name}\t{code}\t{id}\t{server}\t{key}"));
                    }
                    i = e;
                }
                r.pos = r.buf.len();
                FVal::S(lines.join("\n"))
            }
            FType::Seek { pattern, back, optional } => {
                let rest = r.rest();
                let at = rest.windows(pattern.len()).position(|w| w == pattern.as_slice()).filter(|i| *i >= *back);
                match at {
                    Some(i) => r.pos += i - back,
                    None if *optional => {}
                    None => return Err(WireErr::Eof),
                }
                if !*optional {
                    continue;
                }
                FVal::U(at.is_some() as u64)
            }
            FType::Bytes(l) => {
                let n = resolve_len(l, r, parents, out);
                FVal::B(r.take(n)?.to_vec())
            }
            FType::StrVarint => FVal::S(r.str_varint()?),
            FType::StrU16Utf16 => FVal::S(r.str_u16_utf16()?),
            FType::StrUtf16z => FVal::S(r.str_utf16z()?),
            FType::Repeat { count, fields } => {
                if depth > 8 {
                    return Err(WireErr::Eof);
                }
                let rest = matches!(count, LenSpec::Rest);
                let n = if rest { usize::MAX } else { resolve_len(count, r, parents, out).min(4096) };
                let mut items = Vec::new();
                let mut sc: Vec<&Scope> = parents.to_vec();
                sc.push(out);
                let mut i = 0;
                while i < n && !(rest && r.remaining() == 0) {
                    let mut item = Scope::new();
                    read_fields(r, fields, &sc, &mut item, depth + 1)?;
                    items.push(item);
                    i += 1;
                }
                FVal::List(items)
            }
        };
        out.push((f.name.clone(), v));
    }
    Ok(())
}

fn resolve_len(l: &LenSpec, r: &Reader<'_>, parents: &[&Scope], cur: &Scope) -> usize {
    match l {
        LenSpec::Fixed(n) => *n,
        LenSpec::Rest => r.remaining(),
        LenSpec::Field(f) => {
            let mut sc: Vec<&Scope> = parents.to_vec();
            sc.push(cur);
            lookup(&sc, f).map(|v| v.as_u64() as usize).unwrap_or(0)
        }
    }
}

// ---------------------------------------------------------------- framer

#[derive(Debug, Clone)]
pub struct Frame {
    pub header: Scope,
    pub opcode: u64,
    /// Whole frame bytes (header + payload).
    pub raw: Vec<u8>,
    pub payload_offset: usize,
}

impl Frame {
    pub fn payload(&self) -> &[u8] {
        &self.raw[self.payload_offset..]
    }
}

enum Try {
    Frame(Frame),
    NeedMore,
    /// Header read; waiting for a frame of this total size and opcode.
    Waiting(usize, u64),
    Invalid,
}

/// Frames this large are only waited for when their opcode is known: a
/// misread length must not stall the stream for hundreds of KB.
const BIG_FRAME: usize = 128 * 1024;

fn try_frame(fr: &Framing, buf: &[u8]) -> Try {
    let mut r = Reader::new(buf);
    let mut header = Scope::new();
    let mut length_end = 0;
    for f in &fr.header {
        let mut one = Scope::new();
        match read_fields(&mut r, std::slice::from_ref(f), &[&header], &mut one, 0) {
            Ok(()) => header.extend(one),
            Err(_) => return Try::NeedMore,
        }
        if f.name == "length" {
            length_end = r.pos;
        }
    }
    let header_len = r.pos;
    let len = lookup(&[&header], "length").map(|v| v.as_u64()).unwrap_or(0) as i64 + fr.adjust;
    let total = match fr.counts {
        LengthCounts::Frame => len,
        LengthCounts::AfterLength => len + length_end as i64,
        LengthCounts::Payload => len + header_len as i64,
    };
    if total < header_len as i64 || total < fr.min_frame as i64 || total > fr.max_frame as i64 {
        return Try::Invalid;
    }
    let total = total as usize;
    let opcode = lookup(&[&header], "opcode").map(|v| v.as_u64()).unwrap_or(0);
    if buf.len() < total {
        return Try::Waiting(total, opcode);
    }
    Try::Frame(Frame { header, opcode, raw: buf[..total].to_vec(), payload_offset: header_len })
}

/// Splits a byte stream into frames, resynchronising after gaps.
pub struct Framer {
    buf: Vec<u8>,
    synced: bool,
    pub resyncs: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Framer { buf: Vec::new(), synced: true, resyncs: 0 }
    }

    pub fn push(&mut self, fr: &Framing, known: &dyn Fn(u64) -> bool, bytes: &[u8], gap: bool, out: &mut Vec<Frame>) {
        if gap {
            self.buf.clear();
            self.synced = false;
        }
        self.buf.extend_from_slice(bytes);
        let mut pos = 0;
        loop {
            if !self.synced {
                match find_sync(fr, known, &self.buf[pos..]) {
                    Some(off) => {
                        pos += off;
                        self.synced = true;
                        self.resyncs += 1;
                    }
                    None => {
                        // keep a tail in case the header is split
                        let keep = 64.min(self.buf.len() - pos);
                        let cut = self.buf.len() - keep;
                        if cut > pos && self.buf.len() - pos > fr.max_frame.min(1 << 20) {
                            pos = cut;
                        }
                        break;
                    }
                }
            }
            match try_frame(fr, &self.buf[pos..]) {
                Try::Frame(f) => {
                    pos += f.raw.len();
                    out.push(f);
                }
                Try::NeedMore => break,
                Try::Waiting(total, op) if total > BIG_FRAME && !known(op) => {
                    self.synced = false;
                    pos += 1;
                }
                Try::Waiting(..) => break,
                Try::Invalid => {
                    self.synced = false;
                    pos += 1;
                }
            }
        }
        self.buf.drain(..pos);
    }
}

/// Finds an offset where at least three consecutive frames parse (or the
/// chain lands exactly on the buffer end) and the first opcode is known.
fn find_sync(fr: &Framing, known: &dyn Fn(u64) -> bool, buf: &[u8]) -> Option<usize> {
    let limit = buf.len().min(1 << 16);
    'outer: for start in 0..limit {
        let mut p = start;
        let mut n = 0;
        let mut first_known = false;
        loop {
            match try_frame(fr, &buf[p..]) {
                Try::Frame(f) => {
                    if n == 0 {
                        first_known = known(f.opcode);
                    }
                    p += f.raw.len();
                    n += 1;
                    if n >= 3 || p == buf.len() {
                        // A packet that is exactly one whole message (a new connection's
                        // first, unknown login message) starts at a boundary.
                        if first_known || n >= 3 || (start == 0 && p == buf.len()) {
                            return Some(start);
                        }
                        continue 'outer;
                    }
                }
                Try::Waiting(total, op) if total > BIG_FRAME && !known(op) => continue 'outer,
                // A packet that starts a known message longer than itself: the
                // first data of a connection we joined without its handshake is
                // often the zone's big spawn bundle, split over several packets
                // (Vakron, 2026-10-25: skipping it lost every name in the zone).
                Try::Waiting(_, op) if start == 0 && n == 0 && known(op) => return Some(0),
                Try::NeedMore | Try::Waiting(..) => {
                    if n >= 1 && first_known {
                        return Some(start);
                    }
                    if n == 0 && p == start && start + 64 > buf.len() {
                        return None; // not enough data to judge yet
                    }
                    continue 'outer;
                }
                Try::Invalid => continue 'outer,
            }
        }
    }
    None
}

/// One Force member line, "name\tclass\tid\tserver\tkey" (0 = not known).
fn force_member(l: &str) -> crate::events::ForceMember {
    let opt = |s: Option<&str>| s.and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0);
    let f: Vec<&str> = l.split('\t').collect();
    crate::events::ForceMember {
        name: f[0].to_string(),
        code: opt(f.get(1).copied()).map(|v| v as u32),
        id: opt(f.get(2).copied()),
        server: opt(f.get(3).copied()).map(|v| v as u32),
        key: f.get(4).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0),
    }
}

/// One member line, "name\tclass\tid\tserver\tgs\tcp\tstatus" (empty or 0 = not known).
fn party_member(l: &str) -> crate::events::PartyMember {
    let opt = |s: Option<&str>| s.and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0);
    let f: Vec<&str> = l.split('\t').collect();
    crate::events::PartyMember {
        name: f[0].to_string(),
        code: f.get(1).and_then(|v| v.parse::<u32>().ok()),
        id: opt(f.get(2).copied()),
        server: opt(f.get(3).copied()).map(|v| v as u32),
        gs: opt(f.get(4).copied()).map(|v| v as u32),
        cp: opt(f.get(5).copied()).map(|v| v as u32),
        status: f.get(6).and_then(|v| v.parse::<u8>().ok()),
    }
}

// ---------------------------------------------------------------- decoder

pub type Decompressor = Arc<dyn Fn(&str, &[u8]) -> Option<Vec<u8>> + Send + Sync>;

#[derive(Debug, Clone, Default)]
pub struct OpStat {
    pub count: u64,
    pub bytes: u64,
    pub decoded: u64,
    pub errors: u64,
    pub last: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub ts: f64,
    pub from_server: bool,
    pub opcode: u64,
    pub name: Option<String>,
    pub len: usize,
    pub fields: Option<Scope>,
    pub raw_head: Vec<u8>,
}

pub struct Decoder {
    pub defs: Arc<Definitions>,
    framers: HashMap<FlowKey, Framer>,
    decompress: Option<Decompressor>,
    pub stats: HashMap<(bool, u64), OpStat>,
    /// Ring buffer of recent frames for the research view.
    pub recent: VecDeque<DecodedFrame>,
    pub keep_recent: usize,
    /// Bytes of each recent frame to keep (96 in the app, everything offline).
    pub keep_bytes: usize,
    pub frames_total: u64,
    /// Frames read from the server's side only (the stall check: your own
    /// outgoing messages keep flowing even when the server stream is stuck).
    pub frames_server: u64,
    resyncs_gone: u64,
}

impl Decoder {
    pub fn new(defs: Arc<Definitions>, decompress: Option<Decompressor>) -> Self {
        Decoder {
            defs,
            framers: HashMap::new(),
            decompress,
            stats: HashMap::new(),
            recent: VecDeque::new(),
            keep_recent: 0,
            keep_bytes: 96,
            frames_total: 0,
            frames_server: 0,
            resyncs_gone: 0,
        }
    }

    /// Drops all partial frames (used to recover from a stalled stream).
    pub fn reset_all(&mut self) {
        self.resyncs_gone += self.framers.values().map(|f| f.resyncs).sum::<u64>();
        self.framers.clear();
    }

    pub fn reset_flow(&mut self, flow: &FlowKey) {
        if let Some(f) = self.framers.remove(flow) {
            self.resyncs_gone += f.resyncs;
        }
    }

    /// Resyncs so far, including streams that have since closed or been
    /// reset (the count never goes down, so per-fight gap counts hold).
    pub fn resyncs(&self) -> u64 {
        self.resyncs_gone + self.framers.values().map(|f| f.resyncs).sum::<u64>()
    }

    pub fn push(&mut self, flow: FlowKey, from_server: bool, bytes: &[u8], gap: bool, ts: f64, out: &mut Vec<GameEvent>) {
        let defs = self.defs.clone();
        let framer = self.framers.entry(flow).or_default();
        let mut frames = Vec::new();
        let known = |op: u64| defs.messages.is_empty() || defs.messages.contains_key(&op);
        framer.push(&defs.framing, &known, bytes, gap, &mut frames);
        for f in frames {
            self.handle_frame(&defs, f, from_server, ts, out, 0);
        }
    }

    fn handle_frame(&mut self, defs: &Definitions, f: Frame, from_server: bool, ts: f64, out: &mut Vec<GameEvent>, depth: usize) {
        self.frames_total += 1;
        if from_server {
            self.frames_server += 1;
        }
        let mut payload: Vec<u8> = f.payload().to_vec();
        if let Some(c) = &defs.framing.compressed_if {
            if c.truthy(&[&f.header]) {
                match self.decompress.as_ref().and_then(|d| d(&defs.framing.compression, &payload)) {
                    Some(p) => payload = p,
                    None => {
                        self.stats.entry((from_server, f.opcode)).or_default().errors += 1;
                        return;
                    }
                }
            }
        }
        let st = self.stats.entry((from_server, f.opcode)).or_default();
        st.count += 1;
        st.bytes += f.raw.len() as u64;
        st.last = f.raw.iter().take(256).copied().collect();

        let def = defs.messages.get(&f.opcode).and_then(|v| {
            v.iter().find(|d| match d.dir {
                Dir::Any => true,
                Dir::Server => from_server,
                Dir::Client => !from_server,
            })
        });
        let mut decoded_fields = None;
        if let Some(def) = def {
            let mut body: &[u8] = &payload;
            let owned;
            if def.skip > 0 {
                body = body.get(def.skip..).unwrap_or(&[]);
            }
            if def.decompress {
                match self.decompress.as_ref().and_then(|d| d(&defs.framing.compression, body)) {
                    Some(p) => {
                        owned = p;
                        body = &owned;
                    }
                    None => {
                        self.stats.entry((from_server, f.opcode)).or_default().errors += 1;
                        return;
                    }
                }
            }
            if def.container {
                if depth < 4 {
                    let mut fr = Framer::new();
                    let mut inner = Vec::new();
                    fr.push(&defs.framing, &|_| true, body, false, &mut inner);
                    for i in inner {
                        self.handle_frame(defs, i, from_server, ts, out, depth + 1);
                    }
                }
                return;
            }
            let mut r = Reader::new(body);
            let mut scope = Scope::new();
            match read_fields(&mut r, &def.fields, &[&f.header], &mut scope, 0) {
                Ok(()) => {
                    let st = self.stats.entry((from_server, f.opcode)).or_default();
                    st.decoded += 1;
                    for e in &def.emits {
                        emit(e, &f.header, &scope, ts, out);
                    }
                    decoded_fields = Some(scope);
                }
                Err(_) => {
                    self.stats.entry((from_server, f.opcode)).or_default().errors += 1;
                }
            }
        }
        if self.keep_recent > 0 {
            self.recent.push_back(DecodedFrame {
                ts,
                from_server,
                opcode: f.opcode,
                name: def.map(|d| d.name.clone()),
                len: f.raw.len(),
                fields: decoded_fields,
                raw_head: f.raw.iter().take(self.keep_bytes).copied().collect(),
            });
            while self.recent.len() > self.keep_recent {
                self.recent.pop_front();
            }
        }
    }
}

fn emit(e: &Emit, header: &Scope, scope: &Scope, ts: f64, out: &mut Vec<GameEvent>) {
    let empty = Scope::new();
    let items: Vec<&Scope> = match &e.each {
        Some(name) => match lookup(&[header, scope], name) {
            Some(FVal::List(l)) => l.iter().collect(),
            _ => return,
        },
        None => vec![&empty],
    };
    for item in items {
        let sc: [&Scope; 3] = [header, scope, item];
        if let Some(w) = &e.when {
            if !w.truthy(&sc) {
                continue;
            }
        }
        let get = |k: &str| e.map.iter().find(|(n, _)| n == k).and_then(|(_, x)| x.eval(&sc));
        let num = |k: &str| get(k).map(|v| v.as_u64());
        let txt = |k: &str| get(k).map(|v| v.as_string()).filter(|s| !s.is_empty());
        let mut fl = num("flags").unwrap_or(0) as u32;
        for (k, bit) in [
            ("crit", flags::CRIT),
            ("back", flags::BACK),
            ("perfect", flags::PERFECT),
            ("parry", flags::PARRY),
            ("block", flags::BLOCK),
            ("evade", flags::EVADE),
            ("dot", flags::DOT),
            ("perfect_block", flags::PERFECT_BLOCK),
            ("double", flags::DOUBLE),
            ("front", flags::FRONT),
            ("npc_only", flags::NPC_ONLY),
        ] {
            if num(k).unwrap_or(0) != 0 {
                fl |= bit;
            }
        }
        if let Some(m) = num("multi") {
            fl |= (m as u32).min(flags::MULTI_MASK) << flags::MULTI_SHIFT;
        }
        if let Some(c) = num("cast") {
            fl |= ((c as u32 & 0xffff) + 1) << flags::CAST_SHIFT;
        }
        let ev = match e.event.as_str() {
            "damage" => GameEvent::Damage {
                ts,
                src: num("src").unwrap_or(0),
                tgt: num("tgt").unwrap_or(0),
                skill: num("skill").unwrap_or(0) as u32,
                amount: num("amount").unwrap_or(0),
                flags: fl,
            },
            "heal" => GameEvent::Heal {
                ts,
                src: num("src").unwrap_or(0),
                tgt: num("tgt").unwrap_or(0),
                skill: num("skill").unwrap_or(0) as u32,
                amount: num("amount").unwrap_or(0),
                flags: fl,
            },
            "death" => GameEvent::Death { ts, target: num("target").unwrap_or(0), killer: num("killer") },
            "actor" => GameEvent::Actor {
                id: num("id").unwrap_or(0),
                name: txt("name"),
                class: txt("class"),
                kind: txt("kind").map(|k| ActorKind::parse(&k)).unwrap_or(ActorKind::Unknown),
                npc_id: num("npc_id").map(|x| x as u32),
                owner: num("owner").filter(|o| *o != 0),
                max_hp: num("max_hp"),
            },
            "self" => GameEvent::SelfId { id: num("id").unwrap_or(0), name: txt("name") },
            "self_stats" => GameEvent::SelfStats { cp: num("cp") },
            "actor_server" => match (num("id"), num("server")) {
                (Some(id), Some(server)) if id != 0 && server != 0 => GameEvent::ActorServer { id, server: server as u32 },
                _ => continue,
            },
            "fresh" => GameEvent::Fresh,
            "party" => {
                let lines: Vec<String> = txt("members").map(|s| s.split('\n').filter(|l| !l.is_empty()).map(String::from).collect()).unwrap_or_default();
                // A list with no member read isn't "you left": only messages
                // without a list (the leave messages) clear the party.
                if lines.is_empty() && e.map.iter().any(|(k, _)| k == "members") {
                    continue;
                }
                // "@\ttitle\tleader" (dungeon list), then one line per member:
                // name, class number, id, server, gear score, combat power,
                // status. Empty or 0 means not known.
                let opt = |s: Option<&str>| s.and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0);
                let head = lines.first().filter(|l| l.starts_with("@\t")).map(|l| l.split('\t').collect::<Vec<_>>());
                let members: Vec<crate::events::PartyMember> = lines.iter().filter(|l| !l.starts_with("@\t")).map(|l| party_member(l)).collect();
                if !members.is_empty() || head.is_some() || lines.is_empty() {
                    out.push(GameEvent::PartyInfo {
                        title: head.as_ref().and_then(|h| h.get(1)).map(|s| s.to_string()).filter(|s| !s.trim().is_empty()),
                        leader: opt(head.as_ref().and_then(|h| h.get(2)).copied()),
                        members: members.clone(),
                    });
                }
                GameEvent::Party { names: members.iter().map(|m| m.name.clone()).collect(), codes: members.iter().map(|m| m.code).collect() }
            }
            // One room entry, the moment someone joins.
            "party_join" => match txt("members").and_then(|s| s.split('\n').find(|l| !l.is_empty() && !l.starts_with("@\t")).map(party_member)) {
                Some(member) => GameEvent::PartyJoin { member },
                None => continue,
            },
            "force" | "force_join" => {
                let members: Vec<crate::events::ForceMember> = txt("members").map(|s| s.split('\n').filter(|l| !l.is_empty()).map(force_member).collect()).unwrap_or_default();
                match (e.event.as_str(), members.into_iter()) {
                    ("force", m) => GameEvent::ForceInfo { members: m.collect() },
                    (_, mut m) => match m.next() {
                        Some(member) => GameEvent::ForceJoin { member },
                        None => continue,
                    },
                }
            }
            "force_leave" => match num("key") {
                Some(key) if key > 0 => GameEvent::ForceLeave { key: key as u32 },
                _ => continue,
            },
            "party_ready" => match num("id") {
                Some(id) if id > 0 => GameEvent::PartyReady { id, ready: num("ready").unwrap_or(0) != 0 },
                _ => continue,
            },
            "self_ready" => GameEvent::SelfReady { ready: num("ready").unwrap_or(0) != 0 },
            "party_entering" => GameEvent::PartyEntering,
            "room_entered" => GameEvent::RoomEntered,
            "room_left" => GameEvent::RoomLeft,
            "room_exit" => GameEvent::RoomExit,
            "dungeon_left" => GameEvent::DungeonLeft,
            "keys" => match num("cur") {
                Some(cur) if cur < 10_000 => GameEvent::Keys { cur },
                _ => continue,
            },
            // The game leaves a counter value out when it is 0 (the flags only
            // mark the values sent): an update with just the bonus means the
            // base is 0, as the game shows "0 (+35)".
            "odyle" => GameEvent::Odyle { cur: Some(num("cur").unwrap_or(0)), bonus: Some(num("bonus").unwrap_or(0)) },
            "hp" => GameEvent::Hp { ts, id: num("id").unwrap_or(0), hp: num("hp").unwrap_or(0), max_hp: num("max_hp") },
            "zone" => GameEvent::Zone { ts, zone_id: num("zone").unwrap_or(0) as u32 },
            "loot" => GameEvent::Loot {
                ts,
                who: num("who").unwrap_or(0),
                item_id: num("item").unwrap_or(0) as u32,
                grade: num("grade").map(|g| g as u32),
                count: num("count").unwrap_or(1) as u32,
            },
            "buff" => GameEvent::Buff {
                ts,
                src: num("src").unwrap_or(0),
                tgt: num("tgt").unwrap_or(0),
                buff_id: num("buff").unwrap_or(0) as u32,
                applied: num("applied").unwrap_or(1) != 0,
                duration_ms: num("duration_ms"),
            },
            _ => continue,
        };
        out.push(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Endpoint;
    use crate::wire::encode_varint;
    use std::net::Ipv4Addr;

    pub const TEST_DEFS: &str = r#"{
      "name": "test", "version": "1",
      "framing": { "header": ["length:u16le", "opcode:u16le"], "length_counts": "frame" },
      "messages": [
        { "opcode": "0x0101", "name": "S_HIT",
          "fields": ["src:varint", "tgt:varint", "skill:u32", "n:u8",
                     {"name": "hits", "type": "repeat", "count": "n", "fields": ["dmg:varint", "fl:u8"]}],
          "emit": {"event": "damage", "each": "hits", "src": "src", "tgt": "tgt", "skill": "skill",
                   "amount": "dmg", "crit": "fl&1", "back": "fl&2"} },
        { "opcode": 514, "name": "S_ACTOR",
          "fields": ["id:varint", "kind:u8", "name:str", {"name": "cls", "type": "u8", "if": "kind==1"}],
          "emit": {"event": "actor", "id": "id", "name": "name", "kind": "kind", "class": "cls"} }
      ]
    }"#;

    fn frame(op: u16, body: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&((body.len() + 4) as u16).to_le_bytes());
        f.extend_from_slice(&op.to_le_bytes());
        f.extend_from_slice(body);
        f
    }

    fn hit_body() -> Vec<u8> {
        let mut b = Vec::new();
        encode_varint(1001, &mut b);
        encode_varint(5000, &mut b);
        b.extend_from_slice(&77u32.to_le_bytes());
        b.push(2);
        encode_varint(1234, &mut b);
        b.push(1);
        encode_varint(999, &mut b);
        b.push(2);
        b
    }

    fn flow() -> FlowKey {
        FlowKey {
            src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 7777 },
            dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 50000 },
        }
    }

    #[test]
    fn decode_split_and_resync() {
        let defs = Arc::new(Definitions::from_json(TEST_DEFS).unwrap());
        let mut d = Decoder::new(defs, None);
        let mut stream = frame(0x0101, &hit_body());
        let mut actor = Vec::new();
        encode_varint(1001, &mut actor);
        actor.push(1);
        encode_varint(4, &mut actor);
        actor.extend_from_slice(b"Dewa");
        actor.push(7);
        stream.extend(frame(514, &actor));
        let mut out = Vec::new();
        // split into awkward chunks
        for (i, c) in stream.chunks(3).enumerate() {
            d.push(flow(), true, c, i == 0 && false, 1.0, &mut out);
        }
        assert_eq!(out.len(), 3, "{out:?}");
        match &out[0] {
            GameEvent::Damage { amount, flags, skill, .. } => {
                assert_eq!(*amount, 1234);
                assert_eq!(*flags, crate::events::flags::CRIT);
                assert_eq!(*skill, 77);
            }
            e => panic!("{e:?}"),
        }
        match &out[2] {
            GameEvent::Actor { name, class, kind, .. } => {
                assert_eq!(name.as_deref(), Some("Dewa"));
                assert_eq!(class.as_deref(), Some("7"));
                assert_eq!(*kind, ActorKind::Player);
            }
            e => panic!("{e:?}"),
        }

        // Join mid-stream: garbage then valid frames.
        let mut d2 = Decoder::new(d.defs.clone(), None);
        let mut s2 = vec![0xde, 0xad, 0xbe];
        for _ in 0..4 {
            s2.extend(frame(0x0101, &hit_body()));
        }
        let mut out2 = Vec::new();
        d2.push(flow(), true, &s2, true, 1.0, &mut out2);
        assert_eq!(out2.len(), 8);

        // A new connection joined without its handshake: the first packet is
        // one whole message the meter doesn't know (the game's login message),
        // and somewhere inside it the bytes happen to look like a known
        // message ending exactly at the packet's end. Start at the real
        // message, not inside it (that hid the Shugo return's monsters).
        let mut d3 = Decoder::new(d.defs.clone(), None);
        let mut login = b"\x00\x2bsession-text".to_vec();
        login.extend(frame(0x0101, &hit_body()));
        let mut out3 = Vec::new();
        d3.push(flow(), true, &frame(0x3615, &login), true, 1.0, &mut out3);
        assert!(out3.is_empty(), "nothing read from inside the login message: {out3:?}");
        assert_eq!(d3.stats.get(&(true, 0x3615)).map(|s| s.count), Some(1), "read as the login message: {:?}", d3.stats.keys());
        let mut rest = Vec::new();
        for _ in 0..4 {
            rest.extend(frame(0x0101, &hit_body()));
        }
        d3.push(flow(), true, &rest, false, 1.1, &mut out3);
        assert_eq!(out3.len(), 8, "{out3:?}");

        // Joined without the handshake, and the first message is longer than
        // the first packet (the zone's spawn bundle, Vakron 2026-10-25): wait
        // for the rest of it instead of looking for a start inside it.
        let mut d4 = Decoder::new(d.defs.clone(), None);
        let mut actor = Vec::new();
        encode_varint(1001, &mut actor);
        actor.push(1);
        let name = "Palacicih".repeat(12);
        encode_varint(name.len() as u64, &mut actor);
        actor.extend_from_slice(name.as_bytes());
        actor.push(7);
        let mut s4 = frame(514, &actor);
        for _ in 0..3 {
            s4.extend(frame(0x0101, &hit_body()));
        }
        let mut out4 = Vec::new();
        d4.push(flow(), true, &s4[..80], true, 1.0, &mut out4);
        d4.push(flow(), true, &s4[80..], false, 1.1, &mut out4);
        assert!(matches!(&out4[0], GameEvent::Actor { name: Some(n), .. } if *n == name), "{out4:?}");
        assert_eq!(out4.len(), 7, "{out4:?}");
    }

    #[test]
    fn expressions() {
        let s: Scope = vec![("a".into(), FVal::U(0x1234))];
        assert_eq!(Expr::parse("a>>8&0xff").unwrap().eval_u64(&[&s]), Some(0x12));
        assert_eq!(Expr::parse("a==4660").unwrap().eval_u64(&[&s]), Some(1));
        assert_eq!(Expr::parse("0x10").unwrap().eval_u64(&[&s]), Some(16));
        assert!(matches!(Expr::parse("'npc'").unwrap().eval(&[&s]), Some(FVal::S(_))));
        assert_eq!(Expr::parse("a&0x4&&a==4660").unwrap().eval_u64(&[&s]), Some(1));
        assert_eq!(Expr::parse("a&0x4 && a==1").unwrap().eval_u64(&[&s]), Some(0));
        assert_eq!(Expr::parse("a>=4660").unwrap().eval_u64(&[&s]), Some(1));
        assert_eq!(Expr::parse("a<100").unwrap().eval_u64(&[&s]), Some(0));
        assert_eq!(Expr::parse("a>>4>0x100").unwrap().eval_u64(&[&s]), Some(1));
    }
}

#[cfg(test)]
mod skill_split {
    use super::*;
    #[test]
    fn player_vs_npc_skill_expression() {
        let e = Expr::parse("skill+6777216>>24").unwrap();
        for (skill, player) in [(18010340u64, true), (10000000, true), (9999999, false), (1231880, false), (0, false)] {
            let s: Scope = vec![("skill".into(), FVal::U(skill))];
            assert_eq!(e.truthy(&[&s]), player, "{skill}");
        }
    }
}

#[cfg(test)]
mod odyle_tests {
    use super::*;
    use crate::net::Endpoint;
    use std::net::Ipv4Addr;

    #[test]
    fn odyle_from_real_frames() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "14 0c 61 01 0c 01 87 93 03 a0 01 28 01 28 00 00 00 \
                   0f 0c 61 00 04 01 87 93 03 a0 01 03 \
                   0e 0c 61 00 04 0a 00 00 00 0e 02 \
                   0e 0c 61 00 04 65 87 93 03 0e 02 \
                   0e 0c 61 00 04 01 00 00 00 15 02";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        let ody: Vec<_> = out.iter().filter_map(|e| match e { GameEvent::Odyle { cur, bonus } => Some((*cur, *bonus)), _ => None }).collect();
        // The second update sends no bonus: the game leaves zeros out.
        assert_eq!(ody, vec![(Some(160), Some(40)), (Some(160), Some(0))]);
        assert!(out.iter().any(|e| matches!(e, GameEvent::Keys { cur: 21 })), "{out:?}");
    }

    #[test]
    fn odyle_from_login_list() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        // Excerpt of a real 0x610b: other counters, then Odyle 175 (+40).
        let body = "0b 61 49 04 01 00 00 00 15 01 03 00 00 00 8a 96 98 00 00 8b 96 98 00 00 8c 96 98 00 0c 01 87 93 03 af 01 28 04 02 87 93 03 05 04 03 87 93 03 08";
        let mut bytes: Vec<u8> = body.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        bytes.insert(0, (bytes.len() + 1 + 3) as u8);
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        let ody: Vec<_> = out.iter().filter_map(|e| match e { GameEvent::Odyle { cur, bonus } => Some((*cur, *bonus)), _ => None }).collect();
        assert_eq!(ody, vec![(Some(175), Some(40))], "{out:?}");
        assert!(out.iter().any(|e| matches!(e, GameEvent::Keys { cur: 21 })), "{out:?}");
    }
}

#[cfg(test)]
mod party_tests {
    use super::*;
    use crate::net::Endpoint;
    use std::net::Ipv4Addr;

    /// A real 0x9702 group list (Bakarma run); other players' names replaced
    /// with same-length placeholders.
    #[test]
    fn party_names_from_real_frame() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "b7 02 02 97 63 17 10 00 06 31 20 42 6f 73 73 05 e0 27 09 00 00 03 bc 7b 03 00 00 00 e1 05 ff 02 02 05 1e 01 bc 7b 03 00 00 00 e1 05 08 4d 65 6d 62 65 72 30 31 22 00 00 00 2d 00 00 00 41 03 00 00 e1 05 9a 11 04 8a a4 00 00 00 00 00 00 00 1a 00 00 00 00 00 00 00 01 01 1e 02 4f c8 02 00 00 00 e1 05 07 4d 65 6d 62 65 72 32 0e 00 00 00 2d 00 00 00 4d 04 00 00 e1 05 9a 11 04 cd b4 00 00 00 00 00 00 00 a9 00 00 00 00 00 00 00 01 02 1e 03 d7 a5 02 00 00 00 c8 09 07 4d 65 6d 62 65 72 33 18 00 00 00 2d 00 00 00 09 09 00 00 c8 09 9a 11 04 a5 a6 01 00 00 00 00 00 00 9a 00 00 00 00 00 00 00 01 02 1e 04 ae e4 03 00 00 00 cc 09 09 44 65 77 69 48 69 6a 61 75 24 00 00 00 2d 00 00 00 72 04 00 00 07 cc 09 9a 11 04 d0 df 00 00 00 00 00 00 00 20 00 00 00 00 00 00 00 01 03 1e 05 37 65 00 00 00 00 e5 05 06 4d 65 6d 62 72 34 0e 00 00 00 2d 00 00 00 3b 08 00 00 e5 05 9a 11 04 06 a0 01 00 00 00 00 00 00 b4 00 00 00 00 00 00 00 01 01 00 09";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        let (names, codes) = out.iter().find_map(|e| match e { GameEvent::Party { names, codes } => Some((names.clone(), codes.clone())), _ => None }).expect("party event");
        assert_eq!(names, vec!["Member01", "Member2", "Member3", "DewiHijau", "Membr4"]);
        // Each member's class number (DewiHijau is a Chanter: 36).
        assert_eq!(codes, vec![Some(34), Some(14), Some(24), Some(36), Some(14)]);
    }

    /// A real dungeon-room list (Draupnir lobby, other players renamed): the
    /// party panel's title, leader, gear score, combat power and Ready.
    #[test]
    fn dungeon_room_list_for_the_party_panel() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "96 02 02 97 a9 eb 1c 00 10 31 31 31 31 31 31 31 31 31 31 31 31 31 31 31 31 05 e0 27 09 00 00 03 29 06 04 00 00 00 de 05 ff 02 02 05 0c 01 29 06 04 00 00 00 de 05 05 4d 65 6d 30 31 1e 00 00 00 2d 00 00 00 e9 03 00 00 de 05 de 05 04 32 db 00 00 00 00 00 00 00 01 01 0e 02 8a c8 03 00 00 00 ca 09 05 4d 65 6d 30 32 0b 00 00 00 2d 00 00 00 2d 05 00 00 ca 09 ca 09 04 c8 f6 00 00 00 00 00 00 00 01 01 0e 03 2d 09 04 00 00 00 c6 09 07 4d 65 6d 62 65 72 33 18 00 00 00 2d 00 00 00 70 07 00 00 c6 09 c6 09 04 c8 61 01 00 00 00 00 00 00 01 02 0e 04 ae e4 03 00 00 00 cc 09 09 44 65 77 69 48 69 6a 61 75 24 00 00 00 2d 00 00 00 fc 04 00 00 cc 09 cc 09 04 59 f3 00 00 00 00 00 00 00 01 02 0e 05 36 23 04 00 00 00 de 05 08 4d 65 6d 62 65 72 30 34 22 00 00 00 2d 00 00 00 dc 07 00 00 03 de 05 de 05 04 60 63 01 00 00 00 00 00 00 01 01 00 0a";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        let (title, leader, members) = out.iter().find_map(|e| match e { GameEvent::PartyInfo { title, leader, members } => Some((title.clone(), *leader, members.clone())), _ => None }).expect("party info");
        assert_eq!(title.as_deref(), Some("1111111111111111"));
        assert_eq!(members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["Mem01", "Mem02", "Member3", "DewiHijau", "Member04"]);
        // The leader is the first member; not Ready (0x0c), the others are (0x0e).
        assert_eq!(leader, members[0].id);
        assert_eq!(members[0].status, Some(0x0c));
        assert_eq!(members[3].status, Some(0x0e));
        let me = &members[3];
        assert_eq!((me.server, me.gs, me.cp, me.code), (Some(2508), Some(1276), Some(62297), Some(36)));
        assert_eq!((members[2].gs, members[2].cp), (Some(1904), Some(90568)));
        assert_eq!(members[4].gs, Some(2012));
        assert!(members[4].cp.map_or(false, |c| (1_000..=5_000_000).contains(&c)), "{:?}", members[4]);
    }

    /// A real 0x9200 (names of the other two replaced): your own entry has
    /// 3-byte varints after the class, the others 2-byte ones, so the gear
    /// score sits at a different offset (report 1792657582: you showed none).
    #[test]
    fn open_world_party_gear_score_with_varints() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "ef 04 00 92 08 2a 75 04 00 83 24 d5 00 27 ed 03 00 00 00 cc 09 05 00 00 00 02 01 00 02 05 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 03 03 03 ea 09 00 00 cc 09 cc 09 24 30 41 42 42 34 30 41 33 2d 41 30 39 33 2d 34 34 36 46 2d 42 35 39 38 2d 34 32 36 36 30 41 42 30 35 32 31 33 f5 06 03 00 00 00 cc 09 08 42 61 6a 75 42 65 73 69 07 00 00 00 f0 95 01 f0 95 01 76 0d 00 00 76 0d 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 56 04 00 00 00 00 00 00 31 04 00 00 c1 ed 08 48 c2 19 ba c7 00 98 94 46 ff 05 66 69 69 73 74 02 00 00 00 2d 00 00 00 00 00 00 00 8b 05 00 00 1f 06 01 00 00 00 00 00 01 03 02 e8 1a 00 00 cc 09 cc 09 24 33 32 44 36 37 32 38 32 2d 34 32 36 45 2d 34 34 43 31 2d 39 45 39 30 2d 46 44 42 42 43 38 39 34 33 44 39 32 a5 e4 03 00 00 00 cc 09 09 4d 65 6d 62 65 72 30 30 39 20 00 00 00 ab 7f ab 7f ec 10 00 00 ec 10 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 85 03 00 00 14 0c 00 00 85 03 00 00 97 2c 54 47 dc a8 15 47 80 fa b2 47 05 66 69 69 73 74 02 00 00 00 2d 00 00 00 00 00 00 00 c1 05 00 00 6d 27 01 00 00 00 00 00 01 07 01 37 1a 00 00 cc 09 cc 09 24 45 34 35 39 37 46 36 41 2d 36 45 41 41 2d 34 34 45 35 2d 41 31 43 44 2d 32 31 46 36 33 35 31 39 46 33 36 41 27 ed 03 00 00 00 cc 09 0c 4d 65 6d 62 65 72 30 30 30 30 31 32 20 00 00 00 a1 7e a1 7e 32 12 00 00 32 12 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 56 04 00 00 00 00 00 00 eb 03 00 00 34 ba 2a c7 bd 37 88 c7 00 c6 a0 46 05 66 69 69 73 74 02 00 00 00 2d 00 00 00 00 00 00 00 99 06 00 00 01 01 00 b0 59 09 01 01 c4 41 01 00 00 00 00 00 01 01";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        let members = out.iter().find_map(|e| match e { GameEvent::PartyInfo { members, .. } => Some(members.clone()), _ => None }).expect("party info");
        assert_eq!(members.iter().map(|m| (m.name.as_str(), m.gs)).collect::<Vec<_>>(), vec![("BajuBesi", Some(3446)), ("Member009", Some(4332)), ("Member000012", Some(4658))]);
    }

    /// The real 0x9715 sent when you clicked Ready (report 1792800005).
    #[test]
    fn your_ready_click_real_frame() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let mut out = Vec::new();
        d.push(flow, true, &[0x09, 0x15, 0x97, 0x00, 0x00, 0x01], false, 1.0, &mut out);
        assert!(out.iter().any(|e| matches!(e, GameEvent::SelfReady { ready: true })), "{out:?}");
    }

    /// A real 0x9200 from an open-world party of two (another player's name
    /// and the account ids replaced), then the empty 0x9213 sent on leaving.
    #[test]
    fn open_world_party_join_and_leave() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "d6 02 00 92 00 fa 5b 1f 00 40 d3 24 9a e6 0b 03 00 00 00 cc 09 05 00 00 00 02 01 00 02 02 03 01 00 00 00 00 cc 09 b4 0d 24 30 30 30 30 30 30 30 30 2d 30 30 30 30 2d 30 30 30 30 2d 30 30 30 30 2d 30 30 30 30 30 30 30 30 30 30 30 30 e6 0b 03 00 00 00 cc 09 08 4d 65 6d 62 65 72 30 38 0b 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 14 00 00 00 03 00 00 00 60 02 00 00 33 20 79 46 af 8a 22 c7 00 d4 2b 46 3f 05 66 69 69 73 74 02 00 00 00 2d 00 00 00 00 00 00 00 52 09 00 00 eb f5 01 00 00 00 00 00 02 07 02 3e 22 00 00 cc 09 cc 09 24 30 30 30 30 30 30 30 30 2d 30 30 30 30 2d 30 30 30 30 2d 30 30 30 30 2d 30 30 30 30 30 30 30 30 30 30 30 30 e0 6a 03 00 00 00 cc 09 09 44 65 77 61 50 75 74 69 68 1f 00 00 00 aa 77 aa 77 22 12 00 00 22 12 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 56 04 00 00 00 00 00 00 38 04 00 00 7e e6 09 48 1d 4c ce 47 00 70 91 45 05 66 69 69 73 74 02 00 00 00 2d 00 00 00 00 00 00 00 91 06 00 00 01 00 ba 59 09 01 05 e1 21 01 00 00 00 00 00 02 01 08 13 92 00 00";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        let parties: Vec<(Vec<String>, Vec<Option<u32>>)> = out.iter().filter_map(|e| match e { GameEvent::Party { names, codes } => Some((names.clone(), codes.clone())), _ => None }).collect();
        assert_eq!(parties.len(), 2, "{out:?}");
        // A Templar (11) and you, a Cleric (31).
        assert_eq!(parties[0].0, vec!["Member08", "DewaPutih"]);
        assert_eq!(parties[0].1, vec![Some(11), Some(31)]);
        // Leaving: an empty list.
        assert!(parties[1].0.is_empty());
    }

    /// The real empty 0x972a sent when the room leader kicks you while
    /// queueing: no party list follows, so it has to empty the party itself.
    #[test]
    fn kicked_from_dungeon_room() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let mut out = Vec::new();
        d.push(flow, true, &[0x07, 0x2a, 0x97, 0x00], false, 1.0, &mut out);
        assert!(out.iter().any(|e| matches!(e, GameEvent::RoomLeft)), "{out:?}");
    }

    /// The real 0x971d sent when you leave a waiting room yourself.
    #[test]
    fn room_exit_message() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let mut out = Vec::new();
        d.push(flow, true, &[0x08, 0x1d, 0x97, 0x00, 0x00], false, 1.0, &mut out);
        assert!(out.iter().any(|e| matches!(e, GameEvent::RoomExit)), "{out:?}");
    }

    /// The real 0x971b sent when you leave a dungeon (room id 0x0cc108, the
    /// same as in that room's 0x9702 header).
    #[test]
    fn dungeon_exit_message() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let mut out = Vec::new();
        d.push(flow, true, &[0x0a, 0x1b, 0x97, 0x08, 0xc1, 0x0c, 0x00], false, 1.0, &mut out);
        assert!(out.iter().any(|e| matches!(e, GameEvent::DungeonLeft)), "{out:?}");
    }

    /// Real Force messages (Watcher Kaira, capture 1793040837): Aboos joins
    /// the Force (0x960a), then Krocs leaves it (0x9622, member key only).
    #[test]
    fn force_join_and_leave_messages() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "83 01 0a 96 fa 19 1a 00 83 24 d5 00 00 03 94 39 00 00 cc 09 b4 0d 24 37 37 38 32 31 35 41 43 2d 36 44 42 43 2d 34 36 32 34 2d 41 42 30 42 2d 31 32 32 32 31 45 43 41 33 32 45 42 d8 65 04 00 00 00 cc 09 05 41 62 6f 6f 73 23 00 00 00 ec 8b 01 ec 8b 01 14 00 00 00 01 00 00 00 5a 02 00 00 03 2d 00 00 00 00 00 00 00 00 06 00 00 16 fb af c7 14 0e f5 c7 00 70 05 47 f0 17 01 00 00 00 00 00 01 \
                   17 22 96 86 56 02 00 00 00 cc 09 72 19 1a 00 83 24 d5 00 03";
        let bytes: Vec<u8> = hex.split_whitespace().filter(|h| *h != "\\").map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        assert!(
            out.iter().any(|e| matches!(e, GameEvent::ForceJoin { member } if member.name == "Aboos" && member.id == Some(14_740) && member.code == Some(35) && member.server == Some(2508) && member.key == 288_216)),
            "{out:?}"
        );
        assert!(out.iter().any(|e| matches!(e, GameEvent::ForceLeave { key: 153_222 })), "{out:?}");
    }

    /// A real 0x920d: Palabuto (Templar, GS 3158) accepted your invite.
    #[test]
    fn invite_accepted_message() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "a0 01 0d 92 03 02 31 2f 00 00 cc 09 cc 09 24 33 32 44 36 37 32 38 32 2d 34 32 36 45 2d 34 34 43 31 2d 39 45 39 30 2d 46 44 42 42 43 38 39 34 33 44 39 32 e6 0b 03 00 00 00 cc 09 08 50 61 6c 61 62 75 74 6f 0b 00 00 00 c2 cf 01 c2 cf 01 56 0c 00 00 56 0c 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 85 03 00 00 14 0c 00 00 85 03 00 00 c8 ec 62 47 ea ac 17 47 80 d8 b7 47 07 05 66 69 69 73 74 02 00 00 00 2d 00 00 00 00 00 00 00 4b 0a 00 00 34 46 02 00 00 00 00 00 01";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        assert!(
            out.iter().any(|e| matches!(e, GameEvent::PartyJoin { member } if member.name == "Palabuto" && member.code == Some(11) && member.id == Some(199_654) && member.gs == Some(3158))),
            "{out:?}"
        );
    }

    /// Start of a real 0x3633 sent on a teleport (same entity id as before).
    #[test]
    fn self_info_on_teleport() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let body = "33 36 8d 69 5f a1 c1 08 37 08 42 61 6a 75 42 65 73 69 cc 09 07 00 00 00 02 2d 00 00 00";
        let mut bytes: Vec<u8> = body.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        bytes.insert(0, (bytes.len() + 1 + 3) as u8);
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        assert!(out.iter().any(|e| matches!(e, GameEvent::SelfId { id: 13453, name: Some(n) } if n == "BajuBesi")), "{out:?}");
        assert!(out.iter().any(|e| matches!(e, GameEvent::Fresh)), "{out:?}");
    }

    /// A real 0x3641 from the Beritra air raid: a Sorcerer's Bittercold Wind
    /// (a==95) that carries the caster's entity id after "07 02 06" instead
    /// of a name. The name seek used to fail the whole spawn, so each cast
    /// showed as its own "Player #12345" row. (A player name in the frame is
    /// replaced with a same-length placeholder.)
    #[test]
    fn ground_skill_owner_by_id_real_frame() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "ce 01 41 36 e5 d1 04 5f 00 00 cd 90 2c 00 40 02 31 11 fa c3 49 9f 7f 44 00 00 98 41 44 7a 18 43 6e 6c 01 cf 83 01 cf 83 01 6e 11 00 00 6e 11 00 00 00 00 00 00 00 00 00 00 00 00 00 00 78 5d 02 00 64 00 00 00 f0 49 02 00 01 00 00 00 00 00 00 00 a0 86 01 00 00 00 00 00 74 ae 0a 00 01 02 01 11 01 81 96 98 00 ff ff ff ff ff ff ff ff 80 75 d5 2a bb 03 00 00 e5 d1 04 01 02 31 11 fa c3 49 9f 7f 44 00 00 98 41 07 02 06 04 15 00 00 68 03 00 00 00 00 cb 09 0b 50 6c 61 79 65 72 2d 4e 61 6d 65 02 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 03 cd 00 12 02 00 00 d0 00 7a 01 00 00 d6 00 32 00 00 00 2d 00 00 00 00";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        assert!(
            out.iter().any(|e| matches!(e, GameEvent::Actor { id: 76005, kind: ActorKind::Summon, owner: Some(5380), npc_id: Some(2920653), .. })),
            "{out:?}"
        );
    }

    /// An optional seek records whether it found its pattern, so the next
    /// fields can choose between layouts instead of failing the message.
    #[test]
    fn optional_seek_picks_layout() {
        let defs = Arc::new(
            Definitions::from_json(
                r#"{"framing": {"header": ["length:u16le", "opcode:u16le"], "length_counts": "frame"},
                    "messages": [{"opcode": 9, "fields": ["id:u8",
                        {"name": "named", "type": "seek", "pattern": "aa aa", "optional": true},
                        {"name": "s1", "type": "skip", "len": 2, "if": "named"},
                        {"name": "nm", "type": "str", "if": "named"},
                        {"name": "byid", "type": "seek", "pattern": "bb bb", "optional": true, "if": "named==0"},
                        {"name": "s2", "type": "skip", "len": 2, "if": "named==0&&byid"},
                        {"name": "own", "type": "u8", "if": "named==0&&byid"}],
                      "emit": [{"event": "actor", "when": "named", "id": "id", "kind": "'summon'", "name": "nm"},
                               {"event": "actor", "when": "named==0&&own>0", "id": "id", "kind": "'summon'", "owner": "own"}]}]}"#,
            )
            .unwrap(),
        );
        let mut d = Decoder::new(defs, None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 7777 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let frame = |op: u16, body: &[u8]| {
            let mut f = ((body.len() + 4) as u16).to_le_bytes().to_vec();
            f.extend_from_slice(&op.to_le_bytes());
            f.extend_from_slice(body);
            f
        };
        let mut out = Vec::new();
        d.push(flow, true, &frame(9, &[1, 0, 0xaa, 0xaa, 2, b'H', b'i']), false, 1.0, &mut out);
        d.push(flow, true, &frame(9, &[2, 0, 0xbb, 0xbb, 7]), false, 1.0, &mut out);
        d.push(flow, true, &frame(9, &[3, 0, 0]), false, 1.0, &mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(matches!(&out[0], GameEvent::Actor { id: 1, name: Some(n), .. } if n == "Hi"), "{out:?}");
        assert!(matches!(&out[1], GameEvent::Actor { id: 2, owner: Some(7), .. }), "{out:?}");
    }
}
