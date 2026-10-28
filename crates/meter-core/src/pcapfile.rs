//! Reading and writing capture files (classic pcap and pcapng).

use std::io::{self, Read, Write};

#[derive(Debug, Clone)]
pub struct Packet {
    pub ts: f64,
    pub linktype: u32,
    pub data: Vec<u8>,
}

/// Reads a whole capture file. Supports classic pcap (both byte orders,
/// micro/nanosecond) and pcapng (EPB/SPB/OPB blocks).
pub fn read_capture(bytes: &[u8]) -> io::Result<Vec<Packet>> {
    if bytes.len() < 24 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file too short"));
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    match magic {
        0xa1b2c3d4 | 0xa1b23c4d => read_classic(bytes, false, magic == 0xa1b23c4d),
        0xd4c3b2a1 | 0x4d3cb2a1 => read_classic(bytes, true, magic == 0x4d3cb2a1),
        0x0a0d0d0a => read_pcapng(bytes),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "not a pcap/pcapng file")),
    }
}

fn rd32(b: &[u8], off: usize, be: bool) -> u32 {
    let a = [b[off], b[off + 1], b[off + 2], b[off + 3]];
    if be {
        u32::from_be_bytes(a)
    } else {
        u32::from_le_bytes(a)
    }
}

fn read_classic(b: &[u8], be: bool, nanos: bool) -> io::Result<Vec<Packet>> {
    let linktype = rd32(b, 20, be) & 0x0fff_ffff;
    let mut out = Vec::new();
    let mut p = 24;
    while p + 16 <= b.len() {
        let sec = rd32(b, p, be) as f64;
        let frac = rd32(b, p + 4, be) as f64;
        let incl = rd32(b, p + 8, be) as usize;
        p += 16;
        if p + incl > b.len() {
            break;
        }
        let ts = sec + if nanos { frac / 1e9 } else { frac / 1e6 };
        out.push(Packet { ts, linktype, data: b[p..p + incl].to_vec() });
        p += incl;
    }
    Ok(out)
}

fn read_pcapng(b: &[u8]) -> io::Result<Vec<Packet>> {
    let mut out = Vec::new();
    let mut p = 0;
    let mut be = false;
    // per-interface (linktype, ts resolution divisor)
    let mut ifaces: Vec<(u32, f64)> = Vec::new();
    while p + 12 <= b.len() {
        let btype_le = rd32(b, p, false);
        if btype_le == 0x0a0d0d0a {
            let bom = rd32(b, p + 8, false);
            be = bom == 0x4d3c2b1a;
            ifaces.clear();
        }
        let btype = rd32(b, p, be);
        let blen = rd32(b, p + 4, be) as usize;
        if blen < 12 || p + blen > b.len() {
            break;
        }
        let body = &b[p + 8..p + blen - 4];
        match btype {
            // Interface Description Block (a damaged one too short for its
            // link type is skipped instead of crashing the reader)
            1 if body.len() >= 2 => {
                let lt = if be { u16::from_be_bytes([body[0], body[1]]) } else { u16::from_le_bytes([body[0], body[1]]) } as u32;
                let mut res = 1e6;
                // parse options for if_tsresol (code 9)
                let mut o = 8;
                while o + 4 <= body.len() {
                    let code = if be { u16::from_be_bytes([body[o], body[o + 1]]) } else { u16::from_le_bytes([body[o], body[o + 1]]) };
                    let len = if be { u16::from_be_bytes([body[o + 2], body[o + 3]]) } else { u16::from_le_bytes([body[o + 2], body[o + 3]]) } as usize;
                    if code == 0 {
                        break;
                    }
                    if code == 9 && len >= 1 && o + 4 < body.len() {
                        let v = body[o + 4];
                        res = if v & 0x80 != 0 { 2f64.powi((v & 0x7f) as i32) } else { 10f64.powi(v as i32) };
                    }
                    o += 4 + ((len + 3) & !3);
                }
                ifaces.push((lt, res));
            }
            6 => {
                // Enhanced Packet Block
                if body.len() >= 20 {
                    let ifid = rd32(body, 0, be) as usize;
                    let hi = rd32(body, 4, be) as u64;
                    let lo = rd32(body, 8, be) as u64;
                    let cap = rd32(body, 12, be) as usize;
                    let (lt, res) = ifaces.get(ifid).copied().unwrap_or((1, 1e6));
                    if 20 + cap <= body.len() {
                        out.push(Packet { ts: ((hi << 32) | lo) as f64 / res, linktype: lt, data: body[20..20 + cap].to_vec() });
                    }
                }
            }
            3 => {
                // Simple Packet Block
                if body.len() >= 4 {
                    let (lt, _) = ifaces.first().copied().unwrap_or((1, 1e6));
                    out.push(Packet { ts: 0.0, linktype: lt, data: body[4..].to_vec() });
                }
            }
            _ => {}
        }
        p += blen;
    }
    Ok(out)
}

pub fn read_capture_file(path: &std::path::Path) -> io::Result<Vec<Packet>> {
    let mut f = std::fs::File::open(path)?;
    let mut b = Vec::new();
    f.read_to_end(&mut b)?;
    read_capture(&b)
}

/// Streaming classic-pcap writer (microsecond resolution).
pub struct PcapWriter<W: Write> {
    w: W,
}

impl<W: Write> PcapWriter<W> {
    pub fn new(mut w: W, linktype: u32) -> io::Result<Self> {
        w.write_all(&0xa1b2c3d4u32.to_le_bytes())?;
        w.write_all(&2u16.to_le_bytes())?;
        w.write_all(&4u16.to_le_bytes())?;
        w.write_all(&0i32.to_le_bytes())?;
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(&262144u32.to_le_bytes())?;
        w.write_all(&linktype.to_le_bytes())?;
        Ok(PcapWriter { w })
    }
    pub fn write(&mut self, ts: f64, data: &[u8]) -> io::Result<()> {
        let sec = ts.floor();
        let usec = ((ts - sec) * 1e6) as u32;
        self.w.write_all(&(sec as u32).to_le_bytes())?;
        self.w.write_all(&usec.to_le_bytes())?;
        self.w.write_all(&(data.len() as u32).to_le_bytes())?;
        self.w.write_all(&(data.len() as u32).to_le_bytes())?;
        self.w.write_all(data)
    }
    pub fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn write_read() {
        let mut buf = Vec::new();
        {
            let mut w = PcapWriter::new(&mut buf, 1).unwrap();
            w.write(100.25, b"abc").unwrap();
            w.write(101.5, b"defg").unwrap();
        }
        let p = read_capture(&buf).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].data, b"defg");
        assert!((p[0].ts - 100.25).abs() < 1e-5);
    }

    #[test]
    fn damaged_pcapng_does_not_crash() {
        // Section header, then an interface block with an empty body.
        let mut b = Vec::new();
        for w in [0x0a0d0d0au32, 28, 0x1a2b3c4d] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.extend_from_slice(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        b.extend_from_slice(&28u32.to_le_bytes());
        for w in [1u32, 12, 12] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        assert_eq!(read_capture(&b).unwrap().len(), 0);
    }
}
