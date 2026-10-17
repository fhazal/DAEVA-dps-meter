//! Daeva Meter core: everything that does not touch the OS.
//!
//! capture frames -> [`net`] -> [`tcp::Reassembler`] -> [`protocol::Decoder`]
//! -> [`events::GameEvent`] -> [`engine::Engine`] -> JSON for the overlay.

pub mod engine;
pub mod events;
pub mod json;
pub mod net;
pub mod pcapfile;
pub mod protocol;
pub mod sim;
pub mod tcp;
pub mod wire;

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
    events: Vec<GameEvent>,
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
