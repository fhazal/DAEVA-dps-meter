//! Writes a synthetic capture with a made-up protocol, for exercising the
//! research toolkit: `cargo run --example synth_capture -- out.pcap markers.json`
use meter_core::net::{build_frame, Endpoint, FlowKey, TCP_ACK, TCP_SYN};
use meter_core::pcapfile::PcapWriter;
use meter_core::wire::encode_varint;
use std::net::Ipv4Addr;

fn frame(op: u16, body: &[u8]) -> Vec<u8> {
    let mut f = ((body.len() + 4) as u16).to_le_bytes().to_vec();
    f.extend_from_slice(&op.to_le_bytes());
    f.extend_from_slice(body);
    f
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or("synth.pcap".into());
    let mk = args.get(2).cloned().unwrap_or("synth_markers.json".into());
    let mut w = PcapWriter::new(std::fs::File::create(&out).unwrap(), 1).unwrap();
    let game = FlowKey { src: Endpoint { ip: Ipv4Addr::new(43, 1, 2, 3), port: 7777 }, dst: Endpoint { ip: Ipv4Addr::new(192, 168, 1, 20), port: 50123 } };
    let web = FlowKey { src: Endpoint { ip: Ipv4Addr::new(142, 1, 1, 1), port: 443 }, dst: Endpoint { ip: Ipv4Addr::new(192, 168, 1, 20), port: 50200 } };
    let t0 = 1_760_000_000.0;
    w.write(t0, &build_frame(&game.reversed(), 999, TCP_SYN, b"")).unwrap();
    w.write(t0, &build_frame(&game, 4999, TCP_SYN | TCP_ACK, b"")).unwrap();
    let mut seq = 5000u32;
    let mut markers = Vec::new();
    let damages = [1234u64, 1180, 2410];
    for i in 0..200u32 {
        let t = t0 + i as f64 * 0.1;
        let mut stream = Vec::new();
        // heartbeat
        stream.extend(frame(0x0010, &i.to_le_bytes()));
        // movement noise
        let mut mv = Vec::new();
        encode_varint(55555, &mut mv);
        mv.extend_from_slice(&(i as f32).to_le_bytes());
        stream.extend(frame(0x0044, &mv));
        if i % 50 == 25 {
            let d = damages[(i / 50) as usize % 3];
            markers.push(format!("{{\"t\": {:.3}, \"label\": \"cast Ferocious Strike, saw {d}\"}}", t - 0.2));
            let mut b = Vec::new();
            encode_varint(1001, &mut b); // src
            encode_varint(90002, &mut b); // tgt
            b.extend_from_slice(&11011u32.to_le_bytes());
            b.push(1);
            encode_varint(d, &mut b);
            b.push(if d > 2000 { 1 } else { 0 });
            stream.extend(frame(0x0101, &b));
        }
        for c in stream.chunks(90) {
            w.write(t, &build_frame(&game, seq, TCP_ACK, c)).unwrap();
            seq = seq.wrapping_add(c.len() as u32);
        }
        w.write(t, &build_frame(&web, 1 + i * 100, TCP_ACK, &[0x17; 100])).unwrap();
    }
    std::fs::write(&mk, format!("{{\"markers\": [{}]}}", markers.join(","))).unwrap();
    println!("wrote {out} and {mk}");
}
