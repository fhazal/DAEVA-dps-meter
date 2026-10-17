//! Minimal JSON value, parser and writer. Keeps the core free of external
//! dependencies so it builds and tests anywhere.

use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.get(key),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Str(s) => parse_number_str(s),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        self.as_f64().filter(|n| *n >= 0.0).map(|n| n as u64)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Obj(m) => Some(m),
            _ => None,
        }
    }
    pub fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).and_then(|v| v.as_str()).unwrap_or(default)
    }
    pub fn u64_or(&self, key: &str, default: u64) -> u64 {
        self.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
    }
    pub fn f64_or(&self, key: &str, default: f64) -> f64 {
        self.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
    }
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn to_string(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self, None, 0);
        s
    }
    pub fn to_string_pretty(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self, Some(2), 0);
        s
    }
}

/// Accepts decimal or 0x-prefixed hex numbers stored as strings
/// (opcodes are often easier to read in hex).
pub fn parse_number_str(s: &str) -> Option<f64> {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok().map(|v| v as f64)
    } else {
        t.parse::<f64>().ok()
    }
}

// ---------- builder helpers ----------

pub struct Obj(BTreeMap<String, Value>);
impl Obj {
    pub fn new() -> Self {
        Obj(BTreeMap::new())
    }
    pub fn set(mut self, k: &str, v: impl Into<Value>) -> Self {
        self.0.insert(k.to_string(), v.into());
        self
    }
    pub fn put(&mut self, k: &str, v: impl Into<Value>) {
        self.0.insert(k.to_string(), v.into());
    }
    pub fn build(self) -> Value {
        Value::Obj(self.0)
    }
}
impl Default for Obj {
    fn default() -> Self {
        Self::new()
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
macro_rules! num_from {
    ($($t:ty),*) => {$(impl From<$t> for Value { fn from(v: $t) -> Self { Value::Num(v as f64) } })*};
}
num_from!(u8, u16, u32, u64, usize, i32, i64, f32, f64);
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Str(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(v)
    }
}
impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::Arr(v)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Value::Null)
    }
}

// ---------- writer ----------

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, v: &Value, indent: Option<usize>, depth: usize) {
    let nl = |out: &mut String, d: usize| {
        if let Some(i) = indent {
            out.push('\n');
            for _ in 0..i * d {
                out.push(' ');
            }
        }
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => {
            if !n.is_finite() {
                out.push_str("0");
            } else if n.fract() == 0.0 && n.abs() < 9.0e15 {
                let _ = write!(out, "{}", *n as i64);
            } else {
                let _ = write!(out, "{}", (n * 1000.0).round() / 1000.0);
            }
        }
        Value::Str(s) => write_str(out, s),
        Value::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                write_value(out, x, indent, depth + 1);
            }
            if !a.is_empty() {
                nl(out, depth);
            }
            out.push(']');
        }
        Value::Obj(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                write_str(out, k);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_value(out, x, indent, depth + 1);
            }
            if !m.is_empty() {
                nl(out, depth);
            }
            out.push('}');
        }
    }
}

// ---------- parser ----------

#[derive(Debug)]
pub struct ParseError {
    pub pos: usize,
    pub msg: String,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON error at byte {}: {}", self.pos, self.msg)
    }
}
impl std::error::Error for ParseError {}

pub fn parse(src: &str) -> Result<Value, ParseError> {
    let mut p = Parser { b: src.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, m: &str) -> ParseError {
        ParseError { pos: self.i, msg: m.to_string() }
    }
    /// Skips whitespace and `//` line comments (handy in hand-edited definition files).
    fn ws(&mut self) {
        loop {
            while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
                self.i += 1;
            }
            if self.b[self.i..].starts_with(b"//") {
                while self.i < self.b.len() && self.b[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            break;
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > 128 {
            return Err(self.err("nesting too deep"));
        }
        match self.b.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'{') => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(m));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') {
                        // tolerate trailing comma
                        self.i += 1;
                        return Ok(Value::Obj(m));
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(m));
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Value::Arr(a));
                    }
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(a));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'n') => self.lit("null", Value::Null),
            Some(_) => self.number(),
        }
    }
    fn lit(&mut self, s: &str, v: Value) -> Result<Value, ParseError> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            Err(self.err("invalid literal"))
        }
    }
    fn number(&mut self) -> Result<Value, ParseError> {
        let st = self.i;
        while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[st..self.i]).unwrap_or("");
        s.parse::<f64>().map(Value::Num).map_err(|_| ParseError { pos: st, msg: "invalid number".into() })
    }
    fn hex4(&mut self) -> Result<u32, ParseError> {
        if self.i + 4 > self.b.len() {
            return Err(self.err("bad escape"));
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|_| self.err("bad escape"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("bad escape"))?;
        self.i += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, ParseError> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(self.err("expected string"));
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or_else(|| self.err("unterminated string"))?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or_else(|| self.err("bad escape"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid utf-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let src = r#"{ // comment
            "a": [1, 2.5, -3e2, "x\nyé"], "b": {"c": true, "d": null,}, "h": "0x1F" }"#;
        let v = parse(src).unwrap();
        assert_eq!(v.get("h").unwrap().as_u64(), Some(31));
        assert_eq!(v.get("a").unwrap().as_arr().unwrap()[2].as_f64(), Some(-300.0));
        let again = parse(&v.to_string()).unwrap();
        assert_eq!(v, again);
        assert_eq!(parse(&v.to_string_pretty()).unwrap(), v);
    }
}
