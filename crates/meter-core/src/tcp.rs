//! TCP stream reassembly. Turns captured segments into ordered byte chunks
//! per direction, tolerating retransmits, reordering and capture loss.

use crate::net::{FlowKey, TcpSegment, TCP_FIN, TCP_RST, TCP_SYN};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Contiguous bytes for a flow. `gap` is true when bytes were lost
    /// before this chunk and the decoder must resynchronise.
    Data { flow: FlowKey, bytes: Vec<u8>, gap: bool, ts: f64 },
    Closed { flow: FlowKey },
}

struct Pending {
    seq: u32,
    data: Vec<u8>,
    since: f64,
}

struct FlowState {
    next: Option<u32>,
    pending: Vec<Pending>,
    pending_bytes: usize,
    last_seen: f64,
}

pub struct Reassembler {
    flows: HashMap<FlowKey, FlowState>,
    /// Give up waiting for a missing segment after this many seconds.
    pub gap_timeout: f64,
    pub max_pending_bytes: usize,
    pub idle_timeout: f64,
    last_sweep: f64,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

fn seq_diff(a: u32, b: u32) -> i64 {
    a.wrapping_sub(b) as i32 as i64
}

impl Reassembler {
    pub fn new() -> Self {
        Reassembler {
            flows: HashMap::new(),
            gap_timeout: 1.5,
            max_pending_bytes: 4 << 20,
            idle_timeout: 120.0,
            last_sweep: 0.0,
        }
    }

    pub fn flow_count(&self) -> usize {
        self.flows.len()
    }

    pub fn push(&mut self, seg: &TcpSegment<'_>, ts: f64, out: &mut Vec<StreamEvent>) {
        let st = self.flows.entry(seg.flow).or_insert(FlowState {
            next: None,
            pending: Vec::new(),
            pending_bytes: 0,
            last_seen: ts,
        });
        st.last_seen = ts;

        let mut seq = seg.seq;
        if seg.flags & TCP_SYN != 0 {
            seq = seq.wrapping_add(1);
            st.next = Some(seq);
            st.pending.clear();
            st.pending_bytes = 0;
        }
        if !seg.payload.is_empty() {
            match st.next {
                None => {
                    // Joined mid-stream: start here and flag a gap so the
                    // framer knows it may be mid-message.
                    st.next = Some(seq.wrapping_add(seg.payload.len() as u32));
                    out.push(StreamEvent::Data { flow: seg.flow, bytes: seg.payload.to_vec(), gap: true, ts });
                }
                Some(next) => {
                    let d = seq_diff(seq, next);
                    if d <= 0 {
                        let skip = (-d) as usize;
                        if skip < seg.payload.len() {
                            let bytes = seg.payload[skip..].to_vec();
                            st.next = Some(next.wrapping_add(bytes.len() as u32));
                            out.push(StreamEvent::Data { flow: seg.flow, bytes, gap: false, ts });
                        }
                    } else if !st.pending.iter().any(|p| p.seq == seq) {
                        st.pending_bytes += seg.payload.len();
                        st.pending.push(Pending { seq, data: seg.payload.to_vec(), since: ts });
                    }
                    Self::drain(seg.flow, st, ts, false, out);
                }
            }
        }
        // Force progress if we have waited too long or buffered too much.
        let stale = st.pending.iter().any(|p| ts - p.since > self.gap_timeout);
        if stale || st.pending_bytes > self.max_pending_bytes {
            Self::drain(seg.flow, st, ts, true, out);
        }
        if seg.flags & (TCP_FIN | TCP_RST) != 0 {
            Self::drain(seg.flow, st, ts, true, out);
            self.flows.remove(&seg.flow);
            out.push(StreamEvent::Closed { flow: seg.flow });
        }
        if ts - self.last_sweep > 10.0 {
            self.last_sweep = ts;
            let idle = self.idle_timeout;
            self.flows.retain(|_, s| ts - s.last_seen < idle);
        }
    }

    fn drain(flow: FlowKey, st: &mut FlowState, ts: f64, force: bool, out: &mut Vec<StreamEvent>) {
        loop {
            let Some(next) = st.next else { return };
            // Find the earliest pending segment relative to `next`.
            let mut best: Option<(usize, i64)> = None;
            for (i, p) in st.pending.iter().enumerate() {
                let d = seq_diff(p.seq, next);
                if best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            let Some((idx, d)) = best else { return };
            if d > 0 && !force {
                return;
            }
            let p = st.pending.swap_remove(idx);
            st.pending_bytes -= p.data.len();
            if d <= 0 {
                let skip = (-d) as usize;
                if skip < p.data.len() {
                    let bytes = p.data[skip..].to_vec();
                    st.next = Some(next.wrapping_add(bytes.len() as u32));
                    out.push(StreamEvent::Data { flow, bytes, gap: false, ts });
                }
            } else {
                // Lost bytes: jump forward.
                st.next = Some(p.seq.wrapping_add(p.data.len() as u32));
                out.push(StreamEvent::Data { flow, bytes: p.data, gap: true, ts });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Endpoint, TCP_ACK};
    use std::net::Ipv4Addr;

    fn flow() -> FlowKey {
        FlowKey {
            src: Endpoint { ip: Ipv4Addr::new(1, 1, 1, 1), port: 1 },
            dst: Endpoint { ip: Ipv4Addr::new(2, 2, 2, 2), port: 2 },
        }
    }
    fn seg(seq: u32, flags: u8, p: &[u8]) -> TcpSegment<'_> {
        TcpSegment { flow: flow(), seq, flags, payload: p }
    }
    fn collect(ev: &[StreamEvent]) -> (Vec<u8>, usize) {
        let mut b = Vec::new();
        let mut gaps = 0;
        for e in ev {
            if let StreamEvent::Data { bytes, gap, .. } = e {
                b.extend_from_slice(bytes);
                if *gap {
                    gaps += 1;
                }
            }
        }
        (b, gaps)
    }

    #[test]
    fn reorder_and_retransmit() {
        let mut r = Reassembler::new();
        let mut out = Vec::new();
        r.push(&seg(u32::MAX - 1, TCP_SYN, b""), 0.0, &mut out); // wraps
        let base = u32::MAX;
        r.push(&seg(base.wrapping_add(4), TCP_ACK, b"EFGH"), 0.1, &mut out);
        r.push(&seg(base, TCP_ACK, b"ABCD"), 0.2, &mut out);
        r.push(&seg(base.wrapping_add(2), TCP_ACK, b"CDEF"), 0.3, &mut out); // retransmit overlap
        r.push(&seg(base.wrapping_add(8), TCP_ACK, b"IJ"), 0.4, &mut out);
        let (b, gaps) = collect(&out);
        assert_eq!(b, b"ABCDEFGHIJ");
        assert_eq!(gaps, 0);
    }

    #[test]
    fn gap_timeout() {
        let mut r = Reassembler::new();
        let mut out = Vec::new();
        r.push(&seg(99, TCP_SYN, b""), 0.0, &mut out);
        r.push(&seg(100, TCP_ACK, b"AB"), 0.1, &mut out);
        r.push(&seg(110, TCP_ACK, b"XY"), 0.2, &mut out);
        r.push(&seg(112, TCP_ACK, b"Z"), 5.0, &mut out);
        let (b, gaps) = collect(&out);
        assert_eq!(b, b"ABXYZ");
        assert_eq!(gaps, 1);
    }
}
