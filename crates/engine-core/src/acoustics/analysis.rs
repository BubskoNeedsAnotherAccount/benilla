//! Signal analysis for validating the acoustic model: FFT, averaged spectra, resonance finding,
//! 1/3-octave levels, WAV and PNG (spectrogram) output. No dependencies.

use std::f64::consts::PI;

/// In-place radix-2 FFT. `re.len()` must be a power of two.
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

/// Welch-averaged power spectrum (Hann window, 50 % overlap): `nfft/2 + 1` bins, power per bin.
pub fn spectrum(x: &[f32], nfft: usize) -> Vec<f64> {
    assert!(nfft.is_power_of_two());
    let win: Vec<f64> = (0..nfft).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / nfft as f64).cos()).collect();
    let wsum: f64 = win.iter().map(|w| w * w).sum();
    let mut acc = vec![0.0f64; nfft / 2 + 1];
    let mut count = 0.0;
    let hop = nfft / 2;
    let mut start = 0;
    while start + nfft <= x.len() {
        let mut re: Vec<f64> = (0..nfft).map(|i| x[start + i] as f64 * win[i]).collect();
        let mut im = vec![0.0f64; nfft];
        fft(&mut re, &mut im);
        for k in 0..=nfft / 2 {
            acc[k] += (re[k] * re[k] + im[k] * im[k]) / wsum;
        }
        count += 1.0;
        start += hop;
    }
    if count == 0.0 {
        return acc;
    }
    acc.iter().map(|v| v / count).collect()
}

/// Spectral peaks above `min_db_over_median` of the median level, between `f_lo` and `f_hi`,
/// refined by parabolic interpolation. Returns `(frequency, power)` strongest first.
pub fn peaks(psd: &[f64], fs: f64, f_lo: f64, f_hi: f64, min_db_over_median: f64) -> Vec<(f64, f64)> {
    let nfft = (psd.len() - 1) * 2;
    let df = fs / nfft as f64;
    let lo = ((f_lo / df).ceil() as usize).max(2);
    let hi = ((f_hi / df).floor() as usize).min(psd.len() - 2);
    if hi <= lo {
        return vec![];
    }
    let mut sorted: Vec<f64> = psd[lo..hi].to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = sorted[sorted.len() / 2].max(1e-30);
    let thresh = med * 10f64.powf(min_db_over_median / 10.0);
    let mut out = Vec::new();
    for k in lo..hi {
        if psd[k] > psd[k - 1] && psd[k] >= psd[k + 1] && psd[k] > thresh {
            let (a, b, c) = (psd[k - 1].max(1e-30).ln(), psd[k].max(1e-30).ln(), psd[k + 1].max(1e-30).ln());
            let denom = a - 2.0 * b + c;
            let off = if denom.abs() > 1e-12 { 0.5 * (a - c) / denom } else { 0.0 };
            out.push(((k as f64 + off.clamp(-0.5, 0.5)) * df, psd[k]));
        }
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    out
}

/// The frequency of the strongest peak in `[f_lo, f_hi]`.
pub fn strongest_peak(psd: &[f64], fs: f64, f_lo: f64, f_hi: f64) -> Option<f64> {
    peaks(psd, fs, f_lo, f_hi, 3.0).first().map(|p| p.0)
}

/// Power of the spectrum between two frequencies (sum of bins).
pub fn band_power(psd: &[f64], fs: f64, f_lo: f64, f_hi: f64) -> f64 {
    let nfft = (psd.len() - 1) * 2;
    let df = fs / nfft as f64;
    let lo = (f_lo / df).floor() as usize;
    let hi = ((f_hi / df).ceil() as usize).min(psd.len() - 1);
    psd[lo.min(hi)..=hi].iter().sum()
}

/// Power at the bin nearest `hz` plus its two neighbours (a tone's leakage).
pub fn tone_power(psd: &[f64], fs: f64, hz: f64) -> f64 {
    let nfft = (psd.len() - 1) * 2;
    let k = (hz / (fs / nfft as f64)).round() as usize;
    let k = k.clamp(1, psd.len() - 2);
    psd[k - 1] + psd[k] + psd[k + 1]
}

pub fn db(p: f64) -> f64 {
    10.0 * p.max(1e-30).log10()
}

pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// A-weighting gain (linear power) at `f` Hz (IEC 61672).
pub fn a_weight_power(f: f64) -> f64 {
    let f2 = f * f;
    let ra = 12194.0f64.powi(2) * f2 * f2 / ((f2 + 20.6f64.powi(2)) * ((f2 + 107.7f64.powi(2)) * (f2 + 737.9f64.powi(2))).sqrt() * (f2 + 12194.0f64.powi(2)));
    let a_db = 20.0 * ra.log10() + 2.0;
    10f64.powf(a_db / 10.0)
}

/// A-weighted sound pressure level (dB re 20 uPa) of a pressure signal in pascals.
pub fn a_weighted_spl(x: &[f32], fs: f64) -> f64 {
    let psd = spectrum(x, 16384);
    let nfft = (psd.len() - 1) * 2;
    let df = fs / nfft as f64;
    // one-sided Welch power per bin: the mean square is 2 * sum / nfft
    let ms: f64 = 2.0 / nfft as f64 * (1..psd.len()).map(|k| psd[k] * a_weight_power(k as f64 * df)).sum::<f64>();
    10.0 * (ms.max(1e-30) / (20e-6f64).powi(2)).log10()
}

/// Spectral centroid (Hz) between `f_lo` and `f_hi`.
pub fn centroid(psd: &[f64], fs: f64, f_lo: f64, f_hi: f64) -> f64 {
    let nfft = (psd.len() - 1) * 2;
    let df = fs / nfft as f64;
    let (mut s, mut w) = (0.0, 0.0);
    for k in 1..psd.len() {
        let f = k as f64 * df;
        if f >= f_lo && f <= f_hi {
            s += psd[k] * f;
            w += psd[k];
        }
    }
    s / w.max(1e-30)
}

/// 1/3-octave band powers (dB re arbitrary) with centre frequencies from `f_lo` up to `f_hi`.
pub fn third_octaves(psd: &[f64], fs: f64, f_lo: f64, f_hi: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut fc = f_lo;
    while fc <= f_hi {
        let (a, b) = (fc / 2f64.powf(1.0 / 6.0), fc * 2f64.powf(1.0 / 6.0));
        out.push((fc, db(band_power(psd, fs, a, b))));
        fc *= 2f64.powf(1.0 / 3.0);
    }
    out
}

// ------------------------------------------------------------------ files

pub fn write_wav(path: &str, x: &[f32], fs: u32) -> std::io::Result<()> {
    let n = x.len() as u32;
    let mut b: Vec<u8> = Vec::with_capacity(44 + 2 * x.len());
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + n * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&fs.to_le_bytes());
    b.extend_from_slice(&(fs * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(n * 2).to_le_bytes());
    for v in x {
        b.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, b)
}

/// Reads the first channel of a PCM16 / PCM24 / float32 WAV file. Returns samples and sample rate.
pub fn read_wav(path: &str) -> Result<(Vec<f32>, u32), String> {
    let b = std::fs::read(path).map_err(|e| e.to_string())?;
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let (mut fmt, mut channels, mut rate, mut bits) = (0u16, 1u16, 44_100u32, 16u16);
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            fmt = u16::from_le_bytes(b[body..body + 2].try_into().unwrap());
            channels = u16::from_le_bytes(b[body + 2..body + 4].try_into().unwrap());
            rate = u32::from_le_bytes(b[body + 4..body + 8].try_into().unwrap());
            bits = u16::from_le_bytes(b[body + 14..body + 16].try_into().unwrap());
        } else if id == b"data" {
            let end = (body + len).min(b.len());
            let data = &b[body..end];
            let bytes = (bits / 8) as usize;
            let frame = bytes * channels as usize;
            let mut out = Vec::with_capacity(data.len() / frame.max(1));
            for f in data.chunks_exact(frame.max(1)) {
                let s = &f[..bytes];
                let v = match (fmt, bits) {
                    (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
                    (1, 24) => (((s[2] as i32) << 24 | (s[1] as i32) << 16 | (s[0] as i32) << 8) >> 8) as f32 / 8_388_608.0,
                    (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    _ => return Err(format!("unsupported WAV format {fmt}/{bits}")),
                };
                out.push(v);
            }
            return Ok((out, rate));
        }
        pos = body + len + (len & 1);
    }
    Err("no data chunk".into())
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// Writes an RGB image as an (uncompressed) PNG.
pub fn write_png(path: &str, w: usize, h: usize, rgb: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0u8);
        raw.extend_from_slice(&rgb[y * w * 3..(y + 1) * w * 3]);
    }
    let mut z = vec![0x78u8, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b2) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65_521;
        b2 = (b2 + a) % 65_521;
    }
    z.extend_from_slice(&((b2 << 16) | a).to_be_bytes());
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        png.extend_from_slice(&body);
        png.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, png)
}

fn colormap(v: f32) -> [u8; 3] {
    // black -> indigo -> magenta -> orange -> pale yellow
    let stops: [(f32, [f32; 3]); 5] = [
        (0.0, [0.0, 0.0, 0.02]),
        (0.25, [0.20, 0.05, 0.45]),
        (0.5, [0.75, 0.15, 0.40]),
        (0.75, [0.98, 0.55, 0.10]),
        (1.0, [1.0, 0.97, 0.7]),
    ];
    let v = v.clamp(0.0, 1.0);
    for w in stops.windows(2) {
        if v <= w[1].0 {
            let t = (v - w[0].0) / (w[1].0 - w[0].0);
            let c = |i: usize| ((w[0].1[i] + (w[1].1[i] - w[0].1[i]) * t) * 255.0) as u8;
            return [c(0), c(1), c(2)];
        }
    }
    [255, 247, 178]
}

/// Spectrogram to a PNG: time on x, frequency (0..`fmax`, linear) on y, `range_db` of dynamic range.
pub fn spectrogram_png(path: &str, x: &[f32], fs: f32, fmax: f32, range_db: f32) -> std::io::Result<()> {
    let nfft = 2048usize;
    let hop = 512usize;
    let bins = ((fmax / fs * nfft as f32) as usize).min(nfft / 2).max(8);
    let frames = (x.len().saturating_sub(nfft)) / hop;
    if frames == 0 {
        return Ok(());
    }
    let win: Vec<f64> = (0..nfft).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / nfft as f64).cos()).collect();
    let mut cols: Vec<Vec<f32>> = Vec::with_capacity(frames);
    let mut peak = 1e-12f32;
    for f in 0..frames {
        let mut re: Vec<f64> = (0..nfft).map(|i| x[f * hop + i] as f64 * win[i]).collect();
        let mut im = vec![0.0f64; nfft];
        fft(&mut re, &mut im);
        let col: Vec<f32> = (0..bins).map(|k| ((re[k] * re[k] + im[k] * im[k]) as f32).max(1e-20)).collect();
        peak = peak.max(col.iter().cloned().fold(0.0, f32::max));
        cols.push(col);
    }
    // upscale so the picture is easy to read, with a faint grid every 500 Hz
    let sx = (900 / frames).max(1);
    let sy = (420 / bins).max(1);
    let (w, h) = (frames * sx, bins * sy);
    let mut rgb = vec![0u8; w * h * 3];
    for (xi, col) in cols.iter().enumerate() {
        for (k, &p) in col.iter().enumerate() {
            let d = 10.0 * (p / peak).log10();
            let c = colormap(1.0 + d / range_db);
            for dy in 0..sy {
                let y = h - 1 - (k * sy + dy);
                for dx in 0..sx {
                    let x = xi * sx + dx;
                    rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
                }
            }
        }
    }
    let hz_per_row = fmax / h as f32;
    let mut f = 500.0;
    while f < fmax {
        let y = h - 1 - (f / hz_per_row) as usize;
        for x in 0..w {
            for c in 0..3 {
                let v = rgb[(y * w + x) * 3 + c] as u16 + 40;
                rgb[(y * w + x) * 3 + c] = v.min(255) as u8;
            }
        }
        f += 500.0;
    }
    write_png(path, w, h, &rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_finds_a_tone_and_peaks_interpolate() {
        let fs = 44_100.0f32;
        let x: Vec<f32> = (0..16384).map(|i| (std::f32::consts::TAU * 1234.5 * i as f32 / fs).sin()).collect();
        let psd = spectrum(&x, 4096);
        let f = strongest_peak(&psd, fs as f64, 100.0, 5000.0).unwrap();
        assert!((f - 1234.5).abs() < 1.5, "{f}");
    }

    #[test]
    fn a_weighting_matches_the_standard_curve() {
        // IEC 61672: -26.2 dB at 100 Hz, 0 dB at 1 kHz, +1.2 dB at 2 kHz, -9.3 dB at 10 kHz... roughly
        let db = |f: f64| 10.0 * a_weight_power(f).log10();
        assert!((db(1000.0)).abs() < 0.2, "{}", db(1000.0));
        assert!((db(100.0) + 19.1).abs() < 0.4, "{}", db(100.0));
        assert!((db(50.0) + 30.2).abs() < 0.5, "{}", db(50.0));
        assert!((db(4000.0) - 1.0).abs() < 0.3);
        // a 1 kHz tone of 1 Pa rms reads 94 dB(A)
        let x: Vec<f32> = (0..65536).map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / 44100.0).sin() * 2f32.sqrt()).collect();
        let l = a_weighted_spl(&x, 44100.0);
        assert!((l - 94.0).abs() < 0.8, "{l}");
    }

    #[test]
    fn wav_and_png_round_trip() {
        let dir = std::env::temp_dir();
        let wav = dir.join("engine_core_test.wav");
        let x: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        write_wav(wav.to_str().unwrap(), &x, 44_100).unwrap();
        let (y, fs) = read_wav(wav.to_str().unwrap()).unwrap();
        assert_eq!(fs, 44_100);
        assert_eq!(y.len(), x.len());
        assert!((y[777] - x[777]).abs() < 1e-3);
        let png = dir.join("engine_core_test.png");
        spectrogram_png(png.to_str().unwrap(), &vec![0.1; 30000], 44_100.0, 4000.0, 60.0).unwrap();
        assert!(std::fs::metadata(&png).unwrap().len() > 100);
    }
}
