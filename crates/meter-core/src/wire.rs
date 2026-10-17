//! Little helpers for reading binary game messages.

#[derive(Debug, Clone, PartialEq)]
pub enum WireErr {
    Eof,
    BadVarint,
    BadString,
}

pub struct Reader<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos.min(self.buf.len())..]
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], WireErr> {
        if self.remaining() < n {
            return Err(WireErr::Eof);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn skip(&mut self, n: usize) -> Result<(), WireErr> {
        self.take(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Result<u8, WireErr> {
        Ok(self.take(1)?[0])
    }
    pub fn u16le(&mut self) -> Result<u16, WireErr> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u16be(&mut self) -> Result<u16, WireErr> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn u32le(&mut self) -> Result<u32, WireErr> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u32be(&mut self) -> Result<u32, WireErr> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64le(&mut self) -> Result<u64, WireErr> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    pub fn f32le(&mut self) -> Result<f32, WireErr> {
        Ok(f32::from_bits(self.u32le()?))
    }
    /// Unsigned LEB128 / protobuf-style varint.
    pub fn varint(&mut self) -> Result<u64, WireErr> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(WireErr::BadVarint)
    }
    pub fn svarint(&mut self) -> Result<i64, WireErr> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    /// UTF-8 string with a varint length prefix.
    pub fn str_varint(&mut self) -> Result<String, WireErr> {
        let n = self.varint()? as usize;
        if n > 4096 {
            return Err(WireErr::BadString);
        }
        let b = self.take(n)?;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
    /// UTF-16LE string with a u16 character-count prefix.
    pub fn str_u16_utf16(&mut self) -> Result<String, WireErr> {
        let n = self.u16le()? as usize;
        if n > 2048 {
            return Err(WireErr::BadString);
        }
        let b = self.take(n * 2)?;
        Ok(utf16le(b))
    }
    /// Null-terminated UTF-16LE string.
    pub fn str_utf16z(&mut self) -> Result<String, WireErr> {
        let start = self.pos;
        loop {
            let c = self.u16le()?;
            if c == 0 {
                break;
            }
            if self.pos - start > 4096 {
                return Err(WireErr::BadString);
            }
        }
        Ok(utf16le(&self.buf[start..self.pos - 2]))
    }
}

pub fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

pub fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 3);
    for (i, x) in b.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02x}", x));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn varints() {
        for v in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64, u64::MAX] {
            let mut b = Vec::new();
            encode_varint(v, &mut b);
            assert_eq!(Reader::new(&b).varint().unwrap(), v);
        }
        let mut b = Vec::new();
        encode_varint(3, &mut b); // zigzag 3 = -2
        assert_eq!(Reader::new(&b).svarint().unwrap(), -2);
    }
}
