//! Daeva Meter core: everything that does not touch the OS.
//!
//! capture frames -> [`net`] -> [`tcp::Reassembler`] -> [`protocol::Decoder`]
//! -> [`events::GameEvent`] -> [`engine::Engine`] -> JSON for the overlay.

pub mod chime;
pub mod engine;
pub mod gamingtools;
pub mod events;
pub mod json;
pub mod lz4;
pub mod net;
pub mod pcapfile;
pub mod protocol;
pub mod schedule;
pub mod sim;
pub mod tcp;
pub mod wire;
pub mod zipstore;

use engine::{Engine, Fight};
use events::GameEvent;
use net::{Endpoint, FlowKey};
use protocol::Decoder;
use std::collections::HashSet;
use std::net::Ipv4Addr;
use tcp::{Reassembler, StreamEvent};

/// Glues reassembly, decoding and the engine together.
pub struct Pipeline {
    pub reasm: Reassembler,
    pub decoder: Decoder,
    pub engine: Engine,
    /// Remote endpoints known to be the game server (from port detection).
    pub server_endpoints: HashSet<Endpoint>,
    /// Server ports from the definitions file or settings.
    pub server_ports: HashSet<u16>,
    /// Only decode traffic involving a known server endpoint/port.
    pub strict: bool,
    pub packets: u64,
    pub game_packets: u64,
    pub game_bytes: u64,
    pub last_ts: f64,
    pub last_game_ts: f64,
    /// Last time the server sent us any payload (for the stall check).
    pub last_server_data_ts: f64,
    /// How often the stall check restarted reading (shown in reports).
    pub stall_heals: u64,
    stall_frames: u64,
    stall_since: f64,
    pub ping: Ping,
    events: Vec<GameEvent>,
}

/// Passive latency from TCP: time between a client segment leaving and the
/// server acknowledging it. Delayed/piggy-backed ACKs only ever add time,
/// so the minimum over a short window is the best estimate of network RTT.
#[derive(Default)]
pub struct Ping {
    outstanding: std::collections::HashMap<FlowKey, std::collections::VecDeque<(u32, f64)>>,
    samples: std::collections::VecDeque<(f64, f64)>,
    pub last_sample_ts: f64,
}

impl Ping {
    pub const WINDOW: f64 = 5.0;

    fn client_sent(&mut self, flow: FlowKey, end_seq: u32, ts: f64) {
        let q = self.outstanding.entry(flow).or_default();
        // keep the first transmission time for a given end sequence
        if q.back().map_or(true, |(s, _)| (end_seq.wrapping_sub(*s) as i32) > 0) {
            q.push_back((end_seq, ts));
        }
        while q.len() > 256 {
            q.pop_front();
        }
    }

    fn server_acked(&mut self, client_flow: FlowKey, ack: u32, ts: f64) {
        let Some(q) = self.outstanding.get_mut(&client_flow) else { return };
        let mut newest: Option<f64> = None;
        while let Some((end, sent)) = q.front().copied() {
            if (ack.wrapping_sub(end) as i32) >= 0 {
                newest = Some(sent);
                q.pop_front();
            } else {
                break;
            }
        }
        if let Some(sent) = newest {
            let rtt = ts - sent;
            if (0.0..5.0).contains(&rtt) {
                self.samples.push_back((ts, rtt));
                self.last_sample_ts = ts;
            }
        }
        while self.samples.front().map_or(false, |(t, _)| ts - t > Self::WINDOW) {
            self.samples.pop_front();
        }
    }

    /// Best current estimate in milliseconds, if there are fresh samples.
    pub fn ms(&self) -> Option<f64> {
        self.samples.iter().map(|(_, r)| *r).fold(None, |m: Option<f64>, r| Some(m.map_or(r, |m| m.min(r)))).map(|r| r * 1000.0)
    }
}

impl Pipeline {
    pub fn new(decoder: Decoder, engine: Engine) -> Self {
        let server_ports = decoder.defs.ports.iter().copied().collect();
        Pipeline {
            reasm: Reassembler::new(),
            decoder,
            engine,
            server_endpoints: HashSet::new(),
            server_ports,
            strict: true,
            packets: 0,
            game_packets: 0,
            game_bytes: 0,
            last_ts: 0.0,
            last_game_ts: 0.0,
            last_server_data_ts: 0.0,
            stall_heals: 0,
            stall_frames: 0,
            stall_since: 0.0,
            ping: Ping::default(),
            events: Vec::new(),
        }
    }

    fn is_server(&self, ep: &Endpoint) -> bool {
        self.server_endpoints.contains(ep) || self.server_ports.contains(&ep.port)
    }

    /// Which side of a flow is the server, if this flow is game traffic.
    pub fn classify(&self, flow: &FlowKey) -> Option<bool> {
        if self.is_server(&flow.src) {
            return Some(true);
        }
        if self.is_server(&flow.dst) {
            return Some(false);
        }
        if self.strict {
            return None;
        }
        // Heuristic: the private address is us.
        Some(!is_private(flow.src.ip) && is_private(flow.dst.ip))
    }

    /// Feed one captured frame. Returns fights that finished as a result.
    pub fn push_frame(&mut self, linktype: u32, data: &[u8], ts: f64) -> Vec<Fight> {
        self.packets += 1;
        self.last_ts = self.last_ts.max(ts);
        let Some(seg) = net::parse_frame(linktype, data) else { return Vec::new() };
        let Some(from_server) = self.classify(&seg.flow) else { return Vec::new() };
        self.game_packets += 1;
        self.game_bytes += seg.payload.len() as u64;
        self.last_game_ts = ts;
        self.engine.see_time(ts);
        if from_server && !seg.payload.is_empty() {
            self.last_server_data_ts = ts;
        }
        if from_server {
            if seg.flags & net::TCP_ACK != 0 {
                self.ping.server_acked(seg.flow.reversed(), seg.ack, ts);
            }
        } else if !seg.payload.is_empty() {
            self.ping.client_sent(seg.flow, seg.seq.wrapping_add(seg.payload.len() as u32), ts);
        }
        let mut sev = Vec::new();
        self.reasm.push(&seg, ts, &mut sev);
        let mut evs = std::mem::take(&mut self.events);
        for e in sev {
            match e {
                StreamEvent::Data { flow, bytes, gap, ts } => {
                    let fs = if flow == seg.flow { from_server } else { self.classify(&flow).unwrap_or(from_server) };
                    self.decoder.push(flow, fs, &bytes, gap, ts, &mut evs);
                }
                StreamEvent::Closed { flow } => self.decoder.reset_flow(&flow),
            }
        }
        let mut done = Vec::new();
        for ev in evs.drain(..) {
            if let Some(f) = self.engine.handle(&ev) {
                done.push(f);
            }
        }
        self.events = evs;
        done
    }

    /// Feed already-decoded events (demo mode, tests).
    pub fn push_events(&mut self, evs: &[GameEvent]) -> Vec<Fight> {
        evs.iter().filter_map(|e| self.engine.handle(e)).collect()
    }

    /// Seconds the server may keep sending without anything decoding
    /// before reading restarts.
    pub const STALL_SECS: f64 = 15.0;

    /// Self-heal for a stuck stream: when the server keeps sending but none
    /// of it reads as a frame for `STALL_SECS`, the stream has lost sync
    /// (e.g. on a server switch) and would otherwise stay deaf. Restart
    /// reading from the next packet. Only the server's side counts: your
    /// own outgoing messages keep decoding while the server's are stuck,
    /// which once hid a stall for minutes. Returns true when it restarted.
    pub fn heal_stall(&mut self, now: f64) -> bool {
        let frames = self.decoder.frames_server;
        let flowing = self.last_server_data_ts > 0.0 && now - self.last_server_data_ts < 5.0;
        if frames != self.stall_frames || !flowing {
            self.stall_frames = frames;
            self.stall_since = now;
            return false;
        }
        if now - self.stall_since > Self::STALL_SECS {
            self.reasm.reset();
            self.decoder.reset_all();
            self.stall_since = now;
            self.stall_heals += 1;
            return true;
        }
        false
    }

    pub fn tick(&mut self, now: f64) -> Option<Fight> {
        self.engine.tick(now)
    }
}

pub fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, Names};
    use crate::protocol::Definitions;
    use std::sync::Arc;

    #[test]
    fn end_to_end_from_frames() {
        let defs = Arc::new(
            Definitions::from_json(
                r#"{"framing": {"header": ["length:u16le", "opcode:u16le"]}, "ports": [7777],
                    "messages": [{"opcode": 1, "fields": ["src:u32", "tgt:u32", "skill:u32", "dmg:u32", "f:u8"],
                                  "emit": {"event": "damage", "src": "src", "tgt": "tgt", "skill": "skill", "amount": "dmg", "crit": "f&1"}}]}"#,
            )
            .unwrap(),
        );
        let mut p = Pipeline::new(Decoder::new(defs, None), Engine::new(EngineConfig::default(), Names::default()));
        let flow = FlowKey {
            src: Endpoint { ip: Ipv4Addr::new(52, 1, 1, 1), port: 7777 },
            dst: Endpoint { ip: Ipv4Addr::new(192, 168, 0, 10), port: 51000 },
        };
        let mut stream = Vec::new();
        for i in 0..10u32 {
            let mut body = Vec::new();
            for v in [5u32, 99, 42, 100 + i] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            body.push((i % 2) as u8);
            stream.extend_from_slice(&((body.len() + 4) as u16).to_le_bytes());
            stream.extend_from_slice(&1u16.to_le_bytes());
            stream.extend_from_slice(&body);
        }
        p.push_frame(net::DLT_EN10MB, &net::build_frame(&flow, 1, net::TCP_SYN, b""), 0.0);
        let mut seq = 2u32;
        for chunk in stream.chunks(37) {
            p.push_frame(net::DLT_EN10MB, &net::build_frame(&flow, seq, net::TCP_ACK, chunk), 1.0);
            seq += chunk.len() as u32;
        }
        let f = p.engine.current.as_ref().unwrap();
        assert_eq!(f.stats[&5].dmg, (100..110).sum::<u64>());
        assert_eq!(f.stats[&5].crits, 5);
    }

    /// Leaving the Beritra air raid: the server stream lost sync and sat
    /// waiting for a bogus frame while your own outgoing messages kept
    /// decoding, so the old stall check (all frames) never fired and the
    /// meter stayed deaf for minutes. Only the server's frames count now.
    #[test]
    fn stuck_server_stream_heals_while_client_still_decodes() {
        let defs = Arc::new(
            Definitions::from_json(
                r#"{"framing": {"header": ["length:u16le", "opcode:u16le"]}, "ports": [7777],
                    "messages": [{"opcode": 1, "fields": ["src:u32", "tgt:u32", "skill:u32", "dmg:u32", "f:u8"],
                                  "emit": {"event": "damage", "src": "src", "tgt": "tgt", "skill": "skill", "amount": "dmg", "crit": "f&1"}}]}"#,
            )
            .unwrap(),
        );
        let mut p = Pipeline::new(Decoder::new(defs, None), Engine::new(EngineConfig::default(), Names::default()));
        let server = FlowKey {
            src: Endpoint { ip: Ipv4Addr::new(52, 1, 1, 1), port: 7777 },
            dst: Endpoint { ip: Ipv4Addr::new(192, 168, 0, 10), port: 51000 },
        };
        let client = server.reversed();
        let hit = |dmg: u32| {
            let mut body = Vec::new();
            for v in [5u32, 99, 42, dmg] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            body.push(0);
            let mut f = ((body.len() + 4) as u16).to_le_bytes().to_vec();
            f.extend_from_slice(&1u16.to_le_bytes());
            f.extend_from_slice(&body);
            f
        };
        let ping = [6u8, 0, 2, 0, 0, 0]; // a 6-byte client frame, opcode 2
        let (mut sseq, mut cseq) = (1u32, 1u32);
        let send = |p: &mut Pipeline, flow: &FlowKey, seq: &mut u32, data: &[u8], t: f64| {
            p.push_frame(net::DLT_EN10MB, &net::build_frame(flow, *seq, net::TCP_ACK, data), t);
            *seq += data.len() as u32;
        };
        send(&mut p, &server, &mut sseq, &hit(100), 0.0);
        assert!(!p.heal_stall(0.0));
        // A known opcode claiming 60000 bytes: the framer waits for it.
        send(&mut p, &server, &mut sseq, &[0x60, 0xea, 1, 0], 0.5);
        let mut healed_at = None;
        let mut t = 1.0;
        while t < 30.0 {
            send(&mut p, &server, &mut sseq, &[0x11; 40], t);
            send(&mut p, &client, &mut cseq, &ping, t);
            if p.heal_stall(t) && healed_at.is_none() {
                healed_at = Some(t);
            }
            if healed_at.is_some() {
                break;
            }
            t += 1.0;
        }
        let healed_at = healed_at.expect("a stuck server stream must restart");
        assert!(healed_at > Pipeline::STALL_SECS && healed_at < Pipeline::STALL_SECS + 3.0, "{healed_at}");
        assert_eq!(p.stall_heals, 1);
        // Reading restarts: the next hits count again.
        for i in 0..3 {
            send(&mut p, &server, &mut sseq, &hit(1000), healed_at + 1.0 + i as f64 * 0.1);
        }
        let f = p.engine.current.as_ref().unwrap();
        assert!(f.stats[&5].dmg >= 2000, "{}", f.stats[&5].dmg);
    }

    /// A quiet server (only keep-alives) is not a stall.
    #[test]
    fn quiet_server_is_not_a_stall() {
        let defs = Arc::new(Definitions::from_json(r#"{"framing": {"header": ["length:u16le", "opcode:u16le"]}, "ports": [7777]}"#).unwrap());
        let mut p = Pipeline::new(Decoder::new(defs, None), Engine::new(EngineConfig::default(), Names::default()));
        let server = FlowKey {
            src: Endpoint { ip: Ipv4Addr::new(52, 1, 1, 1), port: 7777 },
            dst: Endpoint { ip: Ipv4Addr::new(192, 168, 0, 10), port: 51000 },
        };
        let mut seq = 1u32;
        for i in 0..60 {
            let t = i as f64;
            p.push_frame(net::DLT_EN10MB, &net::build_frame(&server, seq, net::TCP_ACK, &[6, 0, 0, 0x36, 0, 0]), t);
            seq += 6;
            assert!(!p.heal_stall(t), "keep-alives decode as frames: no restart");
        }
        // Nothing at all from the server (you're idle, or offline): no restart either.
        for i in 60..120 {
            assert!(!p.heal_stall(i as f64));
        }
    }

    #[test]
    fn ping_from_acks() {
        let mut p = Ping::default();
        let c = FlowKey {
            src: Endpoint { ip: Ipv4Addr::new(192, 168, 0, 2), port: 50000 },
            dst: Endpoint { ip: Ipv4Addr::new(48, 1, 1, 1), port: 13328 },
        };
        p.client_sent(c, 1100, 10.000);
        p.server_acked(c, 1100, 10.045); // 45 ms
        p.client_sent(c, 1200, 10.200);
        p.server_acked(c, 1200, 10.330); // delayed ack, 130 ms
        assert!((p.ms().unwrap() - 45.0).abs() < 0.5);
    }

    #[test]
    fn simulator_runs() {
        let mut p = Pipeline::new(
            Decoder::new(Arc::new(Definitions::from_json(r#"{"framing": {"header": ["length:u16le"]}}"#).unwrap()), None),
            Engine::new(EngineConfig::default(), Names::from_json(sim::DEMO_NAMES).unwrap()),
        );
        let mut s = sim::Simulator::new(7);
        let mut finished = 0;
        let mut t = 0.0;
        while t < 240.0 {
            let ev = s.step(t);
            finished += p.push_events(&ev).len();
            if p.tick(t).is_some() {
                finished += 1;
            }
            t += 0.25;
        }
        assert!(finished >= 1, "simulator should finish at least one fight");
        let f = p.engine.history.back().unwrap();
        let txt = p.engine.summary_text(f, 6);
        assert!(txt.contains("DewaPutih"), "{txt}");
    }
}

#[cfg(test)]
mod shipped_files {
    #[test]
    fn bundled_definitions_parse() {
        let d = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        crate::protocol::Definitions::from_json(&d).unwrap();
        let n = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/names.json")).unwrap();
        crate::engine::Names::from_json(&n).unwrap();
    }
}

#[cfg(test)]
mod aion2_framing {
    /// Bytes copied from a real capture (server -> client).
    #[test]
    fn splits_real_sample() {
        let d = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let defs = crate::protocol::Definitions::from_json(&d).unwrap();
        let hex = "0e 00 36 aa 21 fd 47 a1 01 00 00 1e 04 38 d1 5d 00 00 d1 5d bb 8e 15 01 eb 02 17 c1 6b 6c 01 00 00 00 e6 56 01 00 \
                   1e 04 38 d1 5d 00 00 d1 5d 95 19 15 01 ec 02 3f fe 3d 6c 01 00 00 00 e6 56 01 00 0e 00 36 46 97 fa 47 a1 01 00 00 \
                   11 1d 37 b3 04 2f 03 45 b6 05 c7 de c7 de";
        let bytes: Vec<u8> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let mut fr = crate::protocol::Framer::new();
        let mut out = Vec::new();
        fr.push(&defs.framing, &|_| true, &bytes, false, &mut out);
        let ops: Vec<u64> = out.iter().map(|f| f.opcode).collect();
        assert_eq!(ops, vec![0x3600, 0x3804, 0x3804, 0x3600, 0x371d]);
    }

    /// A misread length claiming ~200 KB must not stall the stream.
    #[test]
    fn bogus_big_length_does_not_stall() {
        let d = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../definitions/aion2.json")).unwrap();
        let defs = crate::protocol::Definitions::from_json(&d).unwrap();
        let good = "0e 00 36 aa 21 fd 47 a1 01 00 00 1e 04 38 d1 5d 00 00 d1 5d bb 8e 15 01 eb 02 17 c1 6b 6c 01 00 00 00 e6 56 01 00 \
                    0e 00 36 46 97 fa 47 a1 01 00 00 11 1d 37 b3 04 2f 03 45 b6 05 c7 de c7 de";
        let mut bytes: Vec<u8> = vec![0xff, 0xff, 0x0c, 0x77, 0x55];
        bytes.extend(good.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()));
        let known = |op: u64| defs.messages.contains_key(&op);
        let mut fr = crate::protocol::Framer::new();
        let mut out = Vec::new();
        fr.push(&defs.framing, &known, &bytes, false, &mut out);
        let ops: Vec<u64> = out.iter().map(|f| f.opcode).collect();
        assert!(ops.ends_with(&[0x3600, 0x3804, 0x3600, 0x371d]), "{ops:x?}");
    }
}
