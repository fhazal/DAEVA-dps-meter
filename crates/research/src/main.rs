//! daeva-research: offline toolkit for decoding the game protocol from
//! captures recorded with the meter's Research mode (or Wireshark).

use meter_core::engine::{Engine, EngineConfig, Names};
use meter_core::json::{self, Value};
use meter_core::net::{self, FlowKey};
use meter_core::pcapfile::{read_capture_file, Packet};
use meter_core::protocol::{scope_json, Decoder, Definitions};
use meter_core::tcp::{Reassembler, StreamEvent};
use meter_core::wire::{encode_varint, hex};
use meter_core::Pipeline;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "daeva-research <command> <capture.pcap> [options]

Commands:
  flows    <pcap>                         List TCP flows (pick the game one)
  dump     <pcap> --flow N [--out F]      Reassembled stream as hexdump (or raw file)
  framing  <pcap> --flow N                Guess the frame header layout
  search   <pcap> --value V [--value V]   Find numbers in all encodings
           [--flow N] [--markers M.json] [--context 24]
  ops      <pcap> --defs D.json [--flow N] Opcode histogram with samples
  frames   <pcap> --defs D.json --op 0xNN [--limit 20]  Dump frames of one opcode
  near     <pcap> --defs D.json --markers M.json [--window 1.5]
                                          Opcodes seen right after each marker
  decode   <pcap> --defs D.json [--names N.json] [--self ID]  Run the full meter offline
  import-abyss <AbyssDPSMeter.exe> --out names.json [--merge existing.json]
                                          PERSONAL USE: build a local names file from
                                          your own copy of Abyss DPS Meter. Do not
                                          commit or redistribute the output.

--flow is an index from `flows` (default: the busiest non-HTTPS flow).
Directions: append s or c to the index (e.g. 3s = server->client, default s).";

struct Args {
    cmd: String,
    pcap: PathBuf,
    opts: HashMap<String, Vec<String>>,
}

impl Args {
    fn parse() -> Result<Args, String> {
        let mut it = std::env::args().skip(1);
        let cmd = it.next().ok_or(USAGE)?;
        if cmd == "-h" || cmd == "--help" {
            return Err(USAGE.into());
        }
        let pcap = PathBuf::from(it.next().ok_or(USAGE)?);
        let mut opts: HashMap<String, Vec<String>> = HashMap::new();
        while let Some(a) = it.next() {
            let k = a.strip_prefix("--").ok_or(format!("unexpected argument '{a}'"))?.to_string();
            let v = it.next().ok_or(format!("--{k} needs a value"))?;
            opts.entry(k).or_default().push(v);
        }
        Ok(Args { cmd, pcap, opts })
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.opts.get(k).and_then(|v| v.first()).map(|s| s.as_str())
    }
    fn all(&self, k: &str) -> Vec<&str> {
        self.opts.get(k).map(|v| v.iter().map(|s| s.as_str()).collect()).unwrap_or_default()
    }
    fn num(&self, k: &str, d: f64) -> f64 {
        self.get(k).and_then(json::parse_number_str).unwrap_or(d)
    }
}

// ------------------------------------------------------------ streams

struct Chunk {
    ts: f64,
    offset: usize,
    gap: bool,
}

struct Stream {
    flow: FlowKey,
    bytes: Vec<u8>,
    chunks: Vec<Chunk>,
    packets: usize,
    first: f64,
    last: f64,
}

impl Stream {
    fn time_at(&self, off: usize) -> f64 {
        match self.chunks.binary_search_by(|c| c.offset.cmp(&off)) {
            Ok(i) => self.chunks[i].ts,
            Err(0) => self.first,
            Err(i) => self.chunks[i - 1].ts,
        }
    }
}

struct FlowPair {
    /// [server->client, client->server]
    dirs: [Option<Stream>; 2],
    server: net::Endpoint,
}

fn build_streams(pkts: &[Packet]) -> Vec<FlowPair> {
    let mut r = Reassembler::new();
    r.gap_timeout = 3.0;
    let mut map: BTreeMap<FlowKey, Stream> = BTreeMap::new();
    let mut syn_from: HashMap<FlowKey, bool> = HashMap::new();
    for p in pkts {
        let Some(seg) = net::parse_frame(p.linktype, &p.data) else { continue };
        if seg.flags & net::TCP_SYN != 0 && seg.flags & net::TCP_ACK == 0 {
            syn_from.insert(seg.flow, true); // client opened this direction
        }
        let mut ev = Vec::new();
        r.push(&seg, p.ts, &mut ev);
        let e = map.entry(seg.flow).or_insert(Stream { flow: seg.flow, bytes: Vec::new(), chunks: Vec::new(), packets: 0, first: p.ts, last: p.ts });
        e.packets += 1;
        e.last = p.ts;
        for x in ev {
            if let StreamEvent::Data { flow, bytes, gap, ts } = x {
                let s = map.entry(flow).or_insert(Stream { flow, bytes: Vec::new(), chunks: Vec::new(), packets: 0, first: ts, last: ts });
                s.chunks.push(Chunk { ts, offset: s.bytes.len(), gap });
                s.bytes.extend_from_slice(&bytes);
            }
        }
    }
    // pair directions
    let mut pairs: Vec<FlowPair> = Vec::new();
    let mut used: std::collections::HashSet<FlowKey> = Default::default();
    let keys: Vec<FlowKey> = map.keys().copied().collect();
    for k in keys {
        if used.contains(&k) {
            continue;
        }
        used.insert(k);
        used.insert(k.reversed());
        // decide which side is the server
        let client_to_server = if syn_from.contains_key(&k) {
            true
        } else if syn_from.contains_key(&k.reversed()) {
            false
        } else {
            k.src.port > k.dst.port
        };
        let (c2s, s2c) = if client_to_server { (k, k.reversed()) } else { (k.reversed(), k) };
        let a = map.remove(&s2c);
        let b = map.remove(&c2s);
        pairs.push(FlowPair { dirs: [a, b], server: s2c.src });
    }
    pairs.sort_by_key(|p| std::cmp::Reverse(p.dirs.iter().flatten().map(|s| s.bytes.len()).sum::<usize>()));
    pairs
}

fn pick<'a>(pairs: &'a [FlowPair], a: &Args) -> Result<(usize, &'a Stream, bool), String> {
    let spec = a.get("flow");
    let (idx, dir) = match spec {
        Some(s) => {
            let (n, d) = if let Some(n) = s.strip_suffix('c') { (n, 1) } else { (s.trim_end_matches('s'), 0) };
            (n.parse::<usize>().map_err(|_| format!("bad --flow '{s}'"))?, d)
        }
        None => {
            let i = pairs.iter().position(|p| p.server.port != 443 && p.server.port != 80).unwrap_or(0);
            (i, 0)
        }
    };
    let p = pairs.get(idx).ok_or(format!("no flow #{idx}"))?;
    let s = p.dirs[dir].as_ref().ok_or(format!("flow #{idx} has no data in that direction"))?;
    Ok((idx, s, dir == 0))
}

fn load_defs(a: &Args) -> Result<Arc<Definitions>, String> {
    let path = a.get("defs").ok_or("--defs <definitions.json> is required")?;
    let src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(Arc::new(Definitions::from_json(&src)?))
}

fn load_markers(a: &Args) -> Result<Vec<(f64, String)>, String> {
    let Some(path) = a.get("markers") else { return Ok(vec![]) };
    let src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let v = json::parse(&src).map_err(|e| e.to_string())?;
    let arr = v.get("markers").and_then(|m| m.as_arr()).or(v.as_arr()).ok_or("markers file: expected an array")?;
    Ok(arr.iter().map(|m| (m.f64_or("t", 0.0), m.str_or("label", "").to_string())).collect())
}

fn hexdump(b: &[u8], base: usize, out: &mut String) {
    for (i, row) in b.chunks(16).enumerate() {
        let asc: String = row.iter().map(|c| if c.is_ascii_graphic() { *c as char } else { '.' }).collect();
        out.push_str(&format!("{:08x}  {:<48} {}\n", base + i * 16, hex(row), asc));
    }
}

// ------------------------------------------------------------ commands

fn cmd_flows(pairs: &[FlowPair]) {
    println!("{:>3}  {:<22} {:<22} {:>8} {:>11} {:>11} {:>8}", "#", "server", "client", "pkts", "s->c bytes", "c->s bytes", "secs");
    for (i, p) in pairs.iter().enumerate() {
        let s = p.dirs[0].as_ref();
        let c = p.dirs[1].as_ref();
        let client = s.map(|s| s.flow.dst).or(c.map(|c| c.flow.src)).map(|e| e.to_string()).unwrap_or_default();
        let pk = s.map_or(0, |s| s.packets) + c.map_or(0, |c| c.packets);
        let first = s.map_or(f64::MAX, |s| s.first).min(c.map_or(f64::MAX, |c| c.first));
        let last = s.map_or(0.0, |s| s.last).max(c.map_or(0.0, |c| c.last));
        println!(
            "{:>3}  {:<22} {:<22} {:>8} {:>11} {:>11} {:>8.1}",
            i,
            p.server.to_string(),
            client,
            pk,
            s.map_or(0, |s| s.bytes.len()),
            c.map_or(0, |c| c.bytes.len()),
            (last - first).max(0.0)
        );
    }
}

fn cmd_dump(s: &Stream, a: &Args) -> Result<(), String> {
    if let Some(out) = a.get("out") {
        std::fs::write(out, &s.bytes).map_err(|e| e.to_string())?;
        println!("wrote {} bytes to {out}", s.bytes.len());
        return Ok(());
    }
    let limit = a.num("limit", 4096.0) as usize;
    let mut out = String::new();
    for (i, c) in s.chunks.iter().enumerate() {
        if c.offset >= limit {
            break;
        }
        let end = s.chunks.get(i + 1).map_or(s.bytes.len(), |n| n.offset).min(limit);
        out.push_str(&format!("--- t={:.3}{} ---\n", c.ts - s.first, if c.gap { " [GAP]" } else { "" }));
        hexdump(&s.bytes[c.offset..end], c.offset, &mut out);
    }
    print!("{out}");
    Ok(())
}

/// Tries many header layouts and scores how well each one chains through
/// the stream.
fn cmd_framing(s: &Stream) {
    let data = &s.bytes[..s.bytes.len().min(2 << 20)];
    let start = s.chunks.iter().find(|c| !c.gap).map_or(0, |c| c.offset);
    let len_types = ["u16le", "u16be", "u32le", "u32be", "varint"];
    let counts = ["frame", "after_length", "payload"];
    let op_types = ["", "u8", "u16le", "u16be", "u32le", "varint"];
    let mut results: Vec<(f64, String, usize, usize)> = Vec::new();
    for lt in len_types {
        for prefix_skip in [0usize, 1, 2, 4] {
            for ot in op_types {
                for ct in counts {
                    for adj in [-4i64, -2, 0, 2, 4] {
                        let mut header = Vec::new();
                        if prefix_skip > 0 {
                            header.push(format!("\"pre:skip{prefix_skip}\""));
                        }
                        header.push(format!("\"length:{lt}\""));
                        if !ot.is_empty() {
                            header.push(format!("\"opcode:{ot}\""));
                        }
                        let defs = format!(
                            r#"{{"framing": {{"header": [{}], "length_counts": "{ct}", "length_adjust": {adj}, "min_frame": 3, "max_frame": 262144}}}}"#,
                            header.join(",")
                        );
                        let Ok(d) = Definitions::from_json(&defs) else { continue };
                        // chain from the first non-gap chunk
                        let mut p = start;
                        let mut n = 0usize;
                        let mut ops: HashMap<u64, usize> = HashMap::new();
                        let mut fr = meter_core::protocol::Framer::new();
                        let mut frames = Vec::new();
                        fr.push(&d.framing, &|_| true, &data[p..], false, &mut frames);
                        for f in &frames {
                            p += f.raw.len();
                            n += 1;
                            *ops.entry(f.opcode).or_default() += 1;
                        }
                        let _ = p;
                        let covered: usize = frames.iter().map(|f| f.raw.len()).sum();
                        let coverage = covered as f64 / (data.len() - start).max(1) as f64;
                        // Real protocols have a modest number of distinct opcodes.
                        let distinct = ops.len();
                        let op_score = if ot.is_empty() { 0.9 } else if distinct > 1 && distinct < n / 3 + 2 { 1.0 } else { 0.6 };
                        let score = coverage * op_score * (fr.resyncs as f64 + 1.0).recip();
                        if n >= 5 {
                            results.push((score, defs, n, distinct));
                        }
                    }
                }
            }
        }
    }
    results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    results.dedup_by(|a, b| a.1 == b.1);
    println!("Top framing candidates (score = byte coverage x opcode plausibility):\n");
    for (score, d, n, distinct) in results.iter().take(12) {
        println!("{:.3}  frames={:<7} opcodes={:<5} {}", score, n, distinct, d);
    }
    println!("\nPaste the best \"framing\" object into your definitions file, then run `ops`.");
}

fn encodings(v: u64) -> Vec<(&'static str, Vec<u8>)> {
    let mut e = Vec::new();
    if v <= u16::MAX as u64 {
        e.push(("u16le", (v as u16).to_le_bytes().to_vec()));
        e.push(("u16be", (v as u16).to_be_bytes().to_vec()));
    }
    if v <= u32::MAX as u64 {
        e.push(("u32le", (v as u32).to_le_bytes().to_vec()));
        e.push(("u32be", (v as u32).to_be_bytes().to_vec()));
        e.push(("f32le", (v as f32).to_le_bytes().to_vec()));
    }
    e.push(("u64le", v.to_le_bytes().to_vec()));
    let mut b = Vec::new();
    encode_varint(v, &mut b);
    e.push(("varint", b));
    let mut z = Vec::new();
    encode_varint(v << 1, &mut z);
    e.push(("zigzag", z));
    // keep only distinct, reasonably specific patterns
    let mut seen = std::collections::HashSet::new();
    e.retain(|(_, b)| b.len() >= 2 && seen.insert(b.clone()));
    e
}

fn cmd_search(pairs: &[FlowPair], a: &Args) -> Result<(), String> {
    let values: Vec<u64> = a.all("value").iter().filter_map(|v| json::parse_number_str(v).map(|x| x as u64)).collect();
    if values.is_empty() {
        return Err("give at least one --value".into());
    }
    let ctx = a.num("context", 24.0) as usize;
    let markers = load_markers(a)?;
    let only = a.get("flow").map(|_| pick(pairs, a)).transpose()?;
    for v in &values {
        println!("=== {v} ===");
        let mut hits = 0;
        for (pi, p) in pairs.iter().enumerate() {
            for (di, s) in p.dirs.iter().enumerate() {
                let Some(s) = s else { continue };
                if let Some((_, os, _)) = &only {
                    if !std::ptr::eq(*os, s) {
                        continue;
                    }
                }
                for (enc, pat) in encodings(*v) {
                    let mut i = 0;
                    while let Some(pos) = find(&s.bytes[i..], &pat) {
                        let at = i + pos;
                        let t = s.time_at(at);
                        let lo = at.saturating_sub(ctx);
                        let hi = (at + pat.len() + ctx).min(s.bytes.len());
                        let mk = nearest_marker(&markers, t);
                        println!(
                            "flow {pi}{} @{at:<8} t={:<9.3} {enc:<6} {}  [{}] {}{}",
                            if di == 0 { 's' } else { 'c' },
                            t - s.first,
                            hex(&s.bytes[lo..at]),
                            hex(&s.bytes[at..at + pat.len()]),
                            hex(&s.bytes[at + pat.len()..hi]),
                            mk.map(|(d, l)| format!("   <- {:+.2}s after '{l}'", d)).unwrap_or_default()
                        );
                        hits += 1;
                        i = at + 1;
                        if hits > 400 {
                            println!("(too many hits; narrow with --flow)");
                            break;
                        }
                    }
                }
            }
        }
        if hits == 0 {
            println!("not found in any plain encoding — the stream may be compressed or encrypted (check `framing`/`ops` first)");
        }
    }
    Ok(())
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    h.windows(n.len()).position(|w| w == n)
}

fn nearest_marker(m: &[(f64, String)], t: f64) -> Option<(f64, &str)> {
    m.iter().filter(|(mt, _)| t >= *mt && t - mt < 10.0).max_by(|a, b| a.0.partial_cmp(&b.0).unwrap()).map(|(mt, l)| (t - mt, l.as_str()))
}

fn decompressor() -> meter_core::protocol::Decompressor {
    Arc::new(|method: &str, data: &[u8]| match method {
        "lz4" => meter_core::lz4::decompress_size_prepended(data),
        _ => None,
    })
}

fn decode_stream(defs: &Arc<Definitions>, s: &Stream, from_server: bool) -> Decoder {
    let mut d = Decoder::new(defs.clone(), Some(decompressor()));
    d.keep_recent = 2_000_000;
    let mut evs = Vec::new();
    for (i, c) in s.chunks.iter().enumerate() {
        let end = s.chunks.get(i + 1).map_or(s.bytes.len(), |n| n.offset);
        d.push(s.flow, from_server, &s.bytes[c.offset..end], c.gap, c.ts, &mut evs);
    }
    d
}

fn cmd_ops(pairs: &[FlowPair], a: &Args) -> Result<(), String> {
    let defs = load_defs(a)?;
    let (idx, s, srv) = pick(pairs, a)?;
    let d = decode_stream(&defs, s, srv);
    let mut stats: Vec<_> = d.stats.iter().collect();
    stats.sort_by(|a, b| b.1.count.cmp(&a.1.count));
    println!("flow {idx} {}: {} frames, {} resyncs\n", if srv { "server->client" } else { "client->server" }, d.frames_total, d.resyncs());
    println!("{:<10} {:>7} {:>9} {:>7} {:>6}  {:<18} sample", "opcode", "count", "avg len", "decoded", "errors", "name");
    for ((_, op), st) in stats.iter().take(a.num("limit", 60.0) as usize) {
        let name = defs.messages.get(op).and_then(|v| v.first()).map(|m| m.name.as_str()).unwrap_or("");
        println!(
            "0x{:<8x} {:>7} {:>9.1} {:>7} {:>6}  {:<18} {}",
            op,
            st.count,
            st.bytes as f64 / st.count as f64,
            st.decoded,
            st.errors,
            name,
            hex(&st.last[..st.last.len().min(28)])
        );
    }
    Ok(())
}

fn cmd_frames(pairs: &[FlowPair], a: &Args) -> Result<(), String> {
    let defs = load_defs(a)?;
    let op = a.get("op").and_then(json::parse_number_str).ok_or("--op is required")? as u64;
    let (_, s, srv) = pick(pairs, a)?;
    let d = decode_stream(&defs, s, srv);
    let limit = a.num("limit", 20.0) as usize;
    let markers = load_markers(a)?;
    let mut out = String::new();
    for f in d.recent.iter().filter(|f| f.opcode == op).take(limit) {
        let mk = nearest_marker(&markers, f.ts).map(|(d, l)| format!("  <- {:+.2}s after '{l}'", d)).unwrap_or_default();
        out.push_str(&format!("t={:.3} len={}{}\n", f.ts - s.first, f.len, mk));
        if let Some(fields) = &f.fields {
            out.push_str(&format!("  {}\n", scope_json(fields).to_string()));
        }
        hexdump(&f.raw_head, 0, &mut out);
    }
    print!("{out}");
    Ok(())
}

fn cmd_near(pairs: &[FlowPair], a: &Args) -> Result<(), String> {
    let defs = load_defs(a)?;
    let markers = load_markers(a)?;
    if markers.is_empty() {
        return Err("--markers is required".into());
    }
    let win = a.num("window", 1.5);
    let (_, s, srv) = pick(pairs, a)?;
    let d = decode_stream(&defs, s, srv);
    // baseline rate per opcode
    let dur = (s.last - s.first).max(1.0);
    let mut base: HashMap<u64, f64> = HashMap::new();
    for f in &d.recent {
        *base.entry(f.opcode).or_default() += 1.0 / dur;
    }
    for (t, label) in &markers {
        let mut c: HashMap<u64, usize> = HashMap::new();
        for f in d.recent.iter().filter(|f| f.ts >= *t && f.ts <= t + win) {
            *c.entry(f.opcode).or_default() += 1;
        }
        let mut v: Vec<(u64, usize, f64)> = c.into_iter().map(|(op, n)| (op, n, n as f64 / (base.get(&op).copied().unwrap_or(0.01) * win).max(0.01))).collect();
        v.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        println!("[{:.2}] {label}", t - s.first);
        for (op, n, lift) in v.iter().take(8) {
            println!("    0x{op:<6x} x{n:<3} lift {lift:.1}");
        }
    }
    println!("\nHigh 'lift' = the opcode is unusually frequent right after the marker.");
    Ok(())
}

fn cmd_decode(pkts: &[Packet], a: &Args) -> Result<(), String> {
    let defs = load_defs(a)?;
    let names = match a.get("names") {
        Some(p) => Names::from_json(&std::fs::read_to_string(p).map_err(|e| e.to_string())?)?,
        None => Names::default(),
    };
    let mut p = Pipeline::new(Decoder::new(defs, Some(decompressor())), Engine::new(EngineConfig::default(), names));
    p.strict = !p.server_ports.is_empty();
    if let Some(id) = a.get("self").and_then(json::parse_number_str) {
        p.engine.handle(&meter_core::events::GameEvent::SelfId { id: id as u64, name: None });
    }
    let mut fights = Vec::new();
    for pk in pkts {
        fights.extend(p.push_frame(pk.linktype, &pk.data, pk.ts));
    }
    if let Some(f) = p.engine.finish() {
        fights.push(f);
    }
    println!("{} packets, {} game packets, {} events, {} fights\n", p.packets, p.game_packets, p.engine.events_seen, fights.len());
    for f in &fights {
        println!("{}", p.engine.summary_text(f, 12));
    }
    if let Some(out) = a.get("out") {
        let arr: Vec<Value> = fights.iter().map(|f| p.engine.fight_record(f)).collect();
        std::fs::write(out, Value::Arr(arr).to_string_pretty()).map_err(|e| e.to_string())?;
        println!("fight records written to {out}");
    }
    Ok(())
}

// ------------------------------------------------------------ abyss import

/// Finds every JSON object embedded in a binary (Go programs embed their
/// data files verbatim) and returns the ones that parse.
fn embedded_json_objects(b: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 < b.len() {
        // documents start with `{` + newline + two-space indented key
        if b[i] == b'{' && b[i + 1] == b'\n' && b[i + 2] == b' ' && b[i + 3] == b' ' && b[i + 4] == b'"' {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut esc = false;
            let mut j = i;
            let mut end = None;
            while j < b.len() && j - i < 64 << 20 {
                let c = b[j];
                if in_str {
                    if esc {
                        esc = false;
                    } else if c == b'\\' {
                        esc = true;
                    } else if c == b'"' {
                        in_str = false;
                    }
                } else if c == b'"' {
                    in_str = true;
                } else if c == b'{' || c == b'[' {
                    depth += 1;
                } else if c == b'}' || c == b']' {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(j + 1);
                        break;
                    }
                } else if c == 0 {
                    break;
                }
                j += 1;
            }
            if let Some(e) = end {
                if let Ok(s) = std::str::from_utf8(&b[i..e]) {
                    if let Ok(v) = json::parse(s) {
                        out.push(v);
                        i = e;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// Higher = more likely English: ASCII and few Romance/Germanic particles.
fn english_score(texts: &[&str]) -> f64 {
    if texts.is_empty() {
        return f64::MIN;
    }
    let mut ascii = 0usize;
    let mut total = 0usize;
    let mut foreign = 0usize;
    let mut english = 0usize;
    for t in texts {
        total += t.len();
        ascii += t.bytes().filter(|c| c.is_ascii()).count();
        let l = format!(" {} ", t.to_lowercase());
        for w in [" de ", " der ", " des ", " die ", " du ", " la ", " le ", " del ", " da ", " do ", " dos ", " von ", " und "] {
            if l.contains(w) {
                foreign += 1;
            }
        }
        for w in [" of ", " the ", "guard", "attack", "strike", "blow", "shield", "mole", "spirit"] {
            if l.contains(w) {
                english += 1;
            }
        }
    }
    (ascii as f64 / total.max(1) as f64) * 10.0 + (english as f64 - 3.0 * foreign as f64) / texts.len() as f64 * 10.0
}

fn cmd_import_abyss(a: &Args) -> Result<(), String> {
    let out_path = a.get("out").ok_or("--out names.json is required")?;
    let bytes = std::fs::read(&a.pcap).map_err(|e| e.to_string())?;
    let docs = embedded_json_objects(&bytes);
    let mut npc_best: Option<(f64, &Value)> = None;
    let mut skill_tables: Vec<(f64, &Value)> = Vec::new();
    for d in &docs {
        let Some(m) = d.as_obj() else { continue };
        if m.len() < 50 || !m.keys().all(|k| k.bytes().all(|c| c.is_ascii_digit())) {
            continue;
        }
        let first = m.values().next().unwrap();
        if first.get("isBoss").is_some() {
            let names: Vec<&str> = m.values().filter_map(|v| v.get("name").and_then(|n| n.as_str())).collect();
            let sc = english_score(&names);
            if npc_best.map_or(true, |(b, _)| sc > b) {
                npc_best = Some((sc, d));
            }
        } else if first.as_str().is_some() {
            let names: Vec<&str> = m.values().filter_map(|v| v.as_str()).collect();
            let avg = names.iter().map(|s| s.len()).sum::<usize>() as f64 / names.len().max(1) as f64;
            let skillish = m.keys().filter(|k| (8..=9).contains(&k.len()) && k.starts_with('1')).count() * 2 > m.len();
            // Name tables, not effect descriptions ("-N% Resist for Ns").
            let namelike = names.iter().filter(|n| !n.contains('%') && !n.contains(" for N") && n.split_whitespace().count() <= 5).count() as f64
                / names.len().max(1) as f64;
            if skillish && avg < 32.0 && namelike > 0.97 {
                skill_tables.push((english_score(&names), d));
            }
        }
    }
    // keep only the English skill tables (within a margin of the best one)
    let best_skill = skill_tables.iter().map(|t| t.0).fold(f64::MIN, f64::max);
    let mut base = match a.get("merge") {
        Some(p) => json::parse(&std::fs::read_to_string(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?,
        None => json::parse("{}").unwrap(),
    };
    let Value::Obj(root) = &mut base else { return Err("merge file must be a JSON object".into()) };
    let mut npcs = 0;
    let mut bosses = Vec::new();
    if let Some((_, d)) = npc_best {
        let Value::Obj(target) = root.entry("npcs".into()).or_insert(Value::Obj(Default::default())) else { unreachable!() };
        for (k, v) in d.as_obj().unwrap() {
            if let Some(n) = v.get("name").and_then(|n| n.as_str()) {
                target.entry(k.clone()).or_insert(Value::from(n));
                npcs += 1;
                if v.bool_or("isBoss", false) {
                    bosses.push(Value::from(k.as_str()));
                }
            }
        }
    }
    let mut skills = 0;
    {
        let Value::Obj(target) = root.entry("skills".into()).or_insert(Value::Obj(Default::default())) else { unreachable!() };
        for (sc, d) in &skill_tables {
            if *sc < best_skill - 1.0 {
                continue;
            }
            for (k, v) in d.as_obj().unwrap() {
                if let Some(n) = v.as_str() {
                    if !n.is_empty() && !target.contains_key(k) {
                        target.insert(k.clone(), Value::from(n));
                        skills += 1;
                    }
                }
            }
        }
    }
    root.insert("bosses".into(), Value::Arr(bosses));
    root.insert(
        "_note".into(),
        Value::from("Generated locally by daeva-research import-abyss for personal testing. Do not commit or redistribute."),
    );
    std::fs::write(out_path, base.to_string_pretty()).map_err(|e| e.to_string())?;
    println!("{} embedded JSON documents scanned; wrote {npcs} NPC names and {skills} skill names to {out_path}", docs.len());
    Ok(())
}

fn run() -> Result<(), String> {
    let a = Args::parse()?;
    if a.cmd == "import-abyss" {
        return cmd_import_abyss(&a);
    }
    let pkts = read_capture_file(&a.pcap).map_err(|e| format!("{}: {e}", a.pcap.display()))?;
    if a.cmd == "decode" {
        return cmd_decode(&pkts, &a);
    }
    let pairs = build_streams(&pkts);
    match a.cmd.as_str() {
        "flows" => cmd_flows(&pairs),
        "dump" => cmd_dump(pick(&pairs, &a)?.1, &a)?,
        "framing" => cmd_framing(pick(&pairs, &a)?.1),
        "search" => cmd_search(&pairs, &a)?,
        "ops" => cmd_ops(&pairs, &a)?,
        "frames" => cmd_frames(&pairs, &a)?,
        "near" => cmd_near(&pairs, &a)?,
        other => return Err(format!("unknown command '{other}'\n\n{USAGE}")),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
