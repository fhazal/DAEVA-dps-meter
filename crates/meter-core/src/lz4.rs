//! Minimal LZ4 block decompressor (std only) so offline tools can read
//! compressed game frames. The Windows app uses lz4_flex for speed.

/// Decompresses an LZ4 block whose decompressed size is given.
pub fn decompress_block(src: &[u8], size: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let mut i = 0;
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            loop {
                let b = *src.get(i)?;
                i += 1;
                lit += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        out.extend_from_slice(src.get(i..i + lit)?);
        i += lit;
        if i >= src.len() {
            break; // last sequence has literals only
        }
        let off = u16::from_le_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        i += 2;
        if off == 0 || off > out.len() {
            return None;
        }
        let mut ml = (token & 15) as usize;
        if ml == 15 {
            loop {
                let b = *src.get(i)?;
                i += 1;
                ml += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        ml += 4;
        let start = out.len() - off;
        for k in 0..ml {
            let b = out[start + k];
            out.push(b);
        }
        if out.len() > size.max(1) * 2 + 1024 {
            return None;
        }
    }
    Some(out)
}

/// Format used by lz4_flex::block::decompress_size_prepended: u32le size + block.
pub fn decompress_size_prepended(src: &[u8]) -> Option<Vec<u8>> {
    if src.len() < 4 {
        return None;
    }
    let size = u32::from_le_bytes([src[0], src[1], src[2], src[3]]) as usize;
    if size > 16 << 20 {
        return None;
    }
    let out = decompress_block(&src[4..], size)?;
    (out.len() == size).then_some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn literal_and_match() {
        // "abcabcabcabc": literals "abc" then match offset 3 len 9
        let block = [0x35, b'a', b'b', b'c', 3, 0, 0x00];
        let _ = block;
        let enc = [0x35u8, b'a', b'b', b'c', 3, 0];
        let out = super::decompress_block(&enc, 12).unwrap();
        assert_eq!(out, b"abcabcabcabc");
    }
}
