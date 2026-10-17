//! Alert chimes synthesised in code (no sound files to ship): a soft
//! two-note bell for "starting soon", a brighter rising triad for "started".

const RATE: u32 = 44_100;

/// A 16-bit mono WAV file for the alert kind ("soon" or "start") at
/// `volume` 0.0..=1.0.
pub fn chime_wav(kind: &str, volume: f32) -> Vec<u8> {
    // (start seconds, frequency Hz, length seconds)
    let notes: &[(f32, f32, f32)] = if kind == "start" {
        &[(0.00, 1046.50, 0.9), (0.11, 1318.51, 0.9), (0.22, 1567.98, 1.1)] // C6 E6 G6
    } else {
        &[(0.00, 1318.51, 0.9), (0.16, 1760.00, 1.1)] // E6 A6
    };
    let total = notes.iter().map(|(s, _, l)| s + l).fold(0.0f32, f32::max);
    let n = (total * RATE as f32) as usize;
    let mut buf = vec![0f32; n];
    for &(start, f, len) in notes {
        let s0 = (start * RATE as f32) as usize;
        let count = (len * RATE as f32) as usize;
        for i in 0..count {
            let Some(slot) = buf.get_mut(s0 + i) else { break };
            let t = i as f32 / RATE as f32;
            // Bell: fast attack, exponential decay, a touch of the octave.
            let attack = (t / 0.006).min(1.0);
            let env = attack * (-t * 4.2).exp();
            let w = std::f32::consts::TAU * f * t;
            *slot += env * (w.sin() + 0.28 * (2.0 * w).sin() * (-t * 6.0).exp());
        }
    }
    let peak = buf.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-6);
    let gain = volume.clamp(0.0, 1.0) * 0.6 / peak;
    let mut pcm = Vec::with_capacity(n * 2);
    for x in buf {
        let v = (x * gain * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(&pcm);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn valid_wav() {
        let w = super::chime_wav("start", 0.7);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        let data = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
        assert_eq!(w.len(), 44 + data);
        assert!(super::chime_wav("soon", 0.0)[44..].iter().all(|b| *b == 0));
    }
}
