//! Link/IP/TCP header parsing for captured frames.

use std::fmt;
use std::net::Ipv4Addr;

/// pcap link-layer types we understand.
pub const DLT_NULL: u32 = 0;
pub const DLT_EN10MB: u32 = 1;
pub const DLT_RAW: u32 = 101;
pub const DLT_RAW_ALT: u32 = 12;
pub const DLT_LOOP: u32 = 108;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Endpoint {
    pub ip: Ipv4Addr,
    pub port: u16,
}
impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

/// One direction of a TCP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowKey {
    pub src: Endpoint,
    pub dst: Endpoint,
}
impl FlowKey {
    pub fn reversed(&self) -> FlowKey {
        FlowKey { src: self.dst, dst: self.src }
    }
}
impl fmt::Display for FlowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}", self.src, self.dst)
    }
}

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_ACK: u8 = 0x10;

#[derive(Debug, Clone)]
pub struct TcpSegment<'a> {
    pub flow: FlowKey,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub payload: &'a [u8],
}

/// Parses a captured frame into a TCP segment. Returns `None` for anything
/// that is not IPv4/TCP (IPv6, UDP, ARP, fragments...).
pub fn parse_frame(linktype: u32, data: &[u8]) -> Option<TcpSegment<'_>> {
    let ip = match linktype {
        DLT_EN10MB => {
            if data.len() < 14 {
                return None;
            }
            let mut off = 12;
            let mut ethertype = u16::from_be_bytes([data[off], data[off + 1]]);
            off += 2;
            // VLAN tags
            while ethertype == 0x8100 || ethertype == 0x88a8 {
                if data.len() < off + 4 {
                    return None;
                }
                ethertype = u16::from_be_bytes([data[off + 2], data[off + 3]]);
                off += 4;
            }
            if ethertype != 0x0800 {
                return None;
            }
            &data[off..]
        }
        DLT_NULL | DLT_LOOP => {
            if data.len() < 4 {
                return None;
            }
            &data[4..]
        }
        DLT_RAW | DLT_RAW_ALT => data,
        _ => return None,
    };
    parse_ipv4(ip)
}

pub fn parse_ipv4(ip: &[u8]) -> Option<TcpSegment<'_>> {
    if ip.len() < 20 || ip[0] >> 4 != 4 {
        return None;
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    let total = u16::from_be_bytes([ip[2], ip[3]]) as usize;
    let frag = u16::from_be_bytes([ip[6], ip[7]]);
    if ip[9] != 6 || ihl < 20 || ip.len() < ihl {
        return None;
    }
    // Fragmented packets are rare for game TCP; skip non-first fragments.
    if frag & 0x1fff != 0 {
        return None;
    }
    // total may be 0 with TSO offload; fall back to captured length.
    let end = if total >= ihl && total <= ip.len() { total } else { ip.len() };
    let src = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    let dst = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);
    let tcp = &ip[ihl..end];
    if tcp.len() < 20 {
        return None;
    }
    let sport = u16::from_be_bytes([tcp[0], tcp[1]]);
    let dport = u16::from_be_bytes([tcp[2], tcp[3]]);
    let seq = u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]);
    let ack = u32::from_be_bytes([tcp[8], tcp[9], tcp[10], tcp[11]]);
    let doff = ((tcp[12] >> 4) as usize) * 4;
    let flags = tcp[13];
    if doff < 20 || tcp.len() < doff {
        return None;
    }
    Some(TcpSegment {
        flow: FlowKey { src: Endpoint { ip: src, port: sport }, dst: Endpoint { ip: dst, port: dport } },
        seq,
        ack,
        flags,
        payload: &tcp[doff..],
    })
}

/// Builds a synthetic Ethernet/IPv4/TCP frame. Used by tests, the simulator
/// and the recorder when it needs to write a marker-free reconstruction.
pub fn build_frame(flow: &FlowKey, seq: u32, flags: u8, payload: &[u8]) -> Vec<u8> {
    build_frame_ack(flow, seq, 0, flags, payload)
}

/// Like [`build_frame`] but keeps the acknowledgement number (needed to
/// measure latency from recordings).
pub fn build_frame_ack(flow: &FlowKey, seq: u32, ack: u32, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(54 + payload.len());
    f.extend_from_slice(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0x08, 0x00]);
    let total = (40 + payload.len()) as u16;
    f.extend_from_slice(&[0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0x40, 0, 64, 6, 0, 0]);
    f.extend_from_slice(&flow.src.ip.octets());
    f.extend_from_slice(&flow.dst.ip.octets());
    f.extend_from_slice(&flow.src.port.to_be_bytes());
    f.extend_from_slice(&flow.dst.port.to_be_bytes());
    f.extend_from_slice(&seq.to_be_bytes());
    f.extend_from_slice(&ack.to_be_bytes());
    f.extend_from_slice(&[0x50, flags, 0xff, 0xff, 0, 0, 0, 0]);
    f.extend_from_slice(payload);
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_roundtrip() {
        let flow = FlowKey {
            src: Endpoint { ip: Ipv4Addr::new(1, 2, 3, 4), port: 7777 },
            dst: Endpoint { ip: Ipv4Addr::new(192, 168, 1, 5), port: 50000 },
        };
        let f = build_frame(&flow, 1000, TCP_ACK, b"hello");
        let s = parse_frame(DLT_EN10MB, &f).unwrap();
        assert_eq!(s.flow, flow);
        assert_eq!(s.seq, 1000);
        assert_eq!(s.payload, b"hello");
    }
}
