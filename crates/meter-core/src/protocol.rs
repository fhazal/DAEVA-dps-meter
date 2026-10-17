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
    Seek { pattern: Vec<u8>, back: usize },
    /// Group member names from a party list: entries start with the byte
    /// `marker`, an index, a u32 id, u16 0, u16 server, then a u8-length
    /// UTF-8 name. Reads to the end; names are joined with newlines.
    MemberNames { marker: u8 },
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
        "member_names" => FType::MemberNames { marker: v.u64_or("marker", 0x1e) as u8 },
        "seek" => {
            let pattern: Vec<u8> = v
                .str_or("pattern", "")
                .split_whitespace()
                .map(|h| u8::from_str_radix(h, 16).map_err(|_| format!("{ctx}: bad seek pattern")))
                .collect::<Result<_, _>>()?;
            if pattern.is_empty() {
                return Err(format!("{ctx}: seek needs a 'pattern'"));
            }
            FType::Seek { pattern, back: v.u64_or("back", 0) as usize }
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
            FType::MemberNames { marker } => {
                let b = r.rest();
                let mut names: Vec<String> = Vec::new();
                let mut i = 0;
                while i + 12 <= b.len() {
                    let idx = b[i + 1];
                    let len = b[i + 10] as usize;
                    if b[i] == *marker && (1..=16).contains(&idx) && b[i + 6] == 0 && b[i + 7] == 0 && (2..=48).contains(&len) && i + 11 + len <= b.len() {
                        if let Ok(n) = std::str::from_utf8(&b[i + 11..i + 11 + len]) {
                            if !n.chars().any(|c| c.is_control()) && !names.iter().any(|x| x == n) {
                                names.push(n.to_string());
                                i += 11 + len;
                                continue;
                            }
                        }
                    }
                    i += 1;
                }
                r.pos = r.buf.len();
                FVal::S(names.join("\n"))
            }
            FType::Seek { pattern, back } => {
                let rest = r.rest();
                let i = rest.windows(pattern.len()).position(|w| w == pattern.as_slice()).ok_or(WireErr::Eof)?;
                if i < *back {
                    return Err(WireErr::Eof);
                }
                r.pos += i - back;
                continue;
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
                        if first_known || n >= 3 {
                            return Some(start);
                        }
                        continue 'outer;
                    }
                }
                Try::Waiting(total, op) if total > BIG_FRAME && !known(op) => continue 'outer,
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
        }
    }

    /// Drops all partial frames (used to recover from a stalled stream).
    pub fn reset_all(&mut self) {
        self.framers.clear();
    }

    pub fn reset_flow(&mut self, flow: &FlowKey) {
        self.framers.remove(flow);
    }

    pub fn resyncs(&self) -> u64 {
        self.framers.values().map(|f| f.resyncs).sum()
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
            "party" => GameEvent::Party { names: txt("members").map(|s| s.split('\n').map(String::from).collect()).unwrap_or_default() },
            "keys" => match num("cur") {
                Some(cur) if cur < 10_000 => GameEvent::Keys { cur },
                _ => continue,
            },
            "odyle" => match (num("cur"), num("bonus")) {
                (None, None) => continue,
                (cur, bonus) => GameEvent::Odyle { cur, bonus },
            },
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
        assert_eq!(ody, vec![(Some(160), Some(40)), (Some(160), None)]);
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

    /// A real 0x9702 group list (Bakarma run).
    #[test]
    fn party_names_from_real_frame() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let mut d = Decoder::new(Arc::new(Definitions::from_json(&src).unwrap()), None);
        let flow = FlowKey { src: Endpoint { ip: Ipv4Addr::new(9, 9, 9, 9), port: 13328 }, dst: Endpoint { ip: Ipv4Addr::new(10, 0, 0, 2), port: 5000 } };
        let hex = "b7 02 02 97 63 17 10 00 06 31 20 42 6f 73 73 05 e0 27 09 00 00 03 bc 7b 03 00 00 00 e1 05 ff 02 02 05 1e 01 bc 7b 03 00 00 00 e1 05 08 4c 6f 6c 6c 69 70 6f 70 22 00 00 00 2d 00 00 00 41 03 00 00 e1 05 9a 11 04 8a a4 00 00 00 00 00 00 00 1a 00 00 00 00 00 00 00 01 01 1e 02 4f c8 02 00 00 00 e1 05 07 55 76 57 68 69 74 65 0e 00 00 00 2d 00 00 00 4d 04 00 00 e1 05 9a 11 04 cd b4 00 00 00 00 00 00 00 a9 00 00 00 00 00 00 00 01 02 1e 03 d7 a5 02 00 00 00 c8 09 07 58 65 79 6c 79 6e 65 18 00 00 00 2d 00 00 00 09 09 00 00 c8 09 9a 11 04 a5 a6 01 00 00 00 00 00 00 9a 00 00 00 00 00 00 00 01 02 1e 04 ae e4 03 00 00 00 cc 09 09 44 65 77 69 48 69 6a 61 75 24 00 00 00 2d 00 00 00 72 04 00 00 07 cc 09 9a 11 04 d0 df 00 00 00 00 00 00 00 20 00 00 00 00 00 00 00 01 03 1e 05 37 65 00 00 00 00 e5 05 06 6d 61 6b 6f 74 69 0e 00 00 00 2d 00 00 00 3b 08 00 00 e5 05 9a 11 04 06 a0 01 00 00 00 00 00 00 b4 00 00 00 00 00 00 00 01 01 00 09";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut out = Vec::new();
        d.push(flow, true, &bytes, false, 1.0, &mut out);
        let names = out.iter().find_map(|e| match e { GameEvent::Party { names } => Some(names.clone()), _ => None }).expect("party event");
        assert_eq!(names, vec!["Lollipop", "UvWhite", "Xeylyne", "DewiHijau", "makoti"]);
    }
}
