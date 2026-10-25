//! The broadband flow noise the one-dimensional gas solver cannot make: the turbulent jet that leaves each
//! valve, travels down the pipes and comes out of the tailpipe (exhaust) or the airbox (intake) as hiss and rasp.
//!
//! The solver carries plane waves; its cells are centimetres long, so what it radiates above 2-3 kHz is the
//! steepened front of every blow-down pulse resolved on a few cells - a train of sharp spikes, not the stationary
//! noise real engines have there (recordings: the envelope of a 2-8 kHz band at idle has an rms/mean of ~0.55, the
//! value of Gaussian noise, with only a few percent of modulation at the firing rate). So the monopole radiation is
//! cut at `BAND_HZ` and this layer supplies the top end from the physics that makes it:
//!
//! 1. each valve is a nozzle; its jet speed follows from the pressure ratio across it (`jet_power`), and a jet's
//!    acoustic power is a small fraction of its kinetic power that grows with Mach number to the fifth,
//! 2. that power is generated at the valve, so it reaches the mouth `path / c` later, smeared by the pipe's
//!    reverberation (`Envelope`),
//! 3. cat, muffler and airbox take it down by what they are (`exhaust_loss_db`, `intake_loss_db`) and the louder
//!    the losses the darker what is left,
//! 4. the pressure it makes at the listener is `sqrt(W rho c / (2 pi r^2))` times unit-variance noise with the
//!    spectrum of a turbulent jet (`Shaper`).
//!
//! The efficiency and the part losses are estimates (the efficiency of a subsonic jet is 1e-4 M^5 in the
//! literature; the losses are typical insertion losses), calibrated once against recordings of real cars; the
//! dependence on load, rpm, valve size and the fitted parts is what the physics gives.

use super::radiation::{C0, RHO0};
use crate::tuning::{AirFilter, Cat, Muffler, Tuning};

/// Upper edge of what the pipe solver radiates, Hz. Above this the fronts of the pulses are a handful of cells
/// thick and the content is spikes made by the grid; this layer takes over.
pub const BAND_HZ: f32 = 2_800.0;

/// Acoustic efficiency of a jet at Mach 1: `W_acoustic = ETA M^5 (1/2 mdot u^2)`. The textbook figure for a subsonic
/// jet is 1e-4; the blow-down jet is choked and keeps expanding behind shocks, which radiates several times more.
pub const ETA: f32 = 5.0e-4;

/// Acoustic power (W) of the jet through an orifice with upstream state `p_up`, `t_up` and downstream pressure
/// `p_dn` (Pa), carrying `mdot` kg/s. `gamma` and `r_gas` belong to the upstream gas.
#[inline]
pub fn jet_power(p_up: f32, p_dn: f32, t_up: f32, mdot: f32, gamma: f32, r_gas: f32) -> f32 {
    let pr = p_up / p_dn.max(1.0);
    if pr <= 1.0 {
        return 0.0;
    }
    let m2 = 2.0 / (gamma - 1.0) * (pr.powf((gamma - 1.0) / gamma) - 1.0);
    // a choked orifice's jet keeps expanding after the throat: the fully expanded Mach number, which radiates
    let m = m2.sqrt().min(1.7);
    let c_up = (gamma * r_gas * t_up).sqrt();
    let u = m * c_up / (1.0 + 0.5 * (gamma - 1.0) * m2).sqrt();
    ETA * m.powi(5) * 0.5 * mdot.abs() * u * u
}

/// Insertion loss of what sits between the valves and the tailpipe, dB (broadband, 1-5 kHz): a ceramic monolith
/// 12, a metallic one 6; an absorptive stock can 26, a chambered sport can 14, a straight-through pack 8; the rest
/// of the pipe (bends, junctions, wall) 4. Deleting both leaves 4.
pub fn exhaust_loss_db(t: &Tuning) -> f32 {
    let cat = match t.cat {
        Cat::Stock => 12.0,
        Cat::HighFlow => 6.0,
        Cat::Delete => 0.0,
    };
    let muffler = match t.muffler {
        Muffler::Stock => 26.0,
        Muffler::Sport => 14.0,
        Muffler::Straight => 8.0,
        Muffler::Delete => 0.0,
    };
    4.0 + cat + muffler
}

/// Same for the induction path: a sealed airbox with a paper element 24 dB, an open cone on a tube 12, a bare stack 3.
pub fn intake_loss_db(t: &Tuning) -> f32 {
    match t.air_filter {
        AirFilter::Stock => 24.0,
        AirFilter::ColdAir => 12.0,
        AirFilter::OpenStack => 3.0,
    }
}

/// A short ring buffer: the power the jets make now is heard at the mouth a path-length later.
#[derive(Clone, Debug)]
pub struct Delay {
    buf: Vec<f32>,
    w: usize,
}

impl Delay {
    pub fn new(max_samples: usize) -> Delay {
        Delay { buf: vec![0.0; max_samples.max(4)], w: 0 }
    }
    /// Pushes this sample's value and returns the one pushed `d` samples ago.
    #[inline]
    pub fn step(&mut self, x: f32, d: usize) -> f32 {
        let n = self.buf.len();
        self.buf[self.w] = x;
        let d = d.min(n - 1);
        let r = (self.w + n - d) % n;
        self.w = (self.w + 1) % n;
        self.buf[r]
    }
}

/// Unit-variance noise with the spectrum of a turbulent jet: nothing below `f_lo`, then falling with frequency
/// (the jet's own spectrum peaks near St = 0.2-1 and falls ~ -6 dB/octave in amplitude beyond), and at the top
/// the 10 kHz that a listener metres away receives.
#[derive(Clone, Debug)]
pub struct Shaper {
    rng: u32,
    hp: [f32; 2],
    lp1: f32,
    lp2: f32,
    lp3: f32,
    a_hp: f32,
    a_lp1: f32,
    a_lp2: f32,
    a_lp3: f32,
    gain: f32,
}

impl Shaper {
    pub fn new(seed: u32, fs: f32, f_lo: f32, f_knee: f32, f_hi: f32) -> Shaper {
        let a = |f: f32| 1.0 - (-std::f32::consts::TAU * f / fs).exp();
        Shaper { rng: seed | 1, hp: [0.0; 2], lp1: 0.0, lp2: 0.0, lp3: 0.0, a_hp: a(f_lo), a_lp1: a(f_knee), a_lp2: a(f_hi), a_lp3: a(f_hi * 1.4), gain: 1.0 }
    }

    /// Takes over the generator and filter state of `other` (same seed and shape, possibly other cut-offs): both
    /// then draw the same noise.
    pub fn copy_state_from(&mut self, other: &Shaper) {
        self.rng = other.rng;
        self.hp = other.hp;
        self.lp1 = other.lp1;
        self.lp2 = other.lp2;
        self.lp3 = other.lp3;
    }

    /// Sets the gain that makes the output unit variance by measuring it on a burst of the generator itself.
    pub fn normalise(&mut self) {
        let mut probe = self.clone();
        probe.gain = 1.0;
        let n = 1 << 16;
        let mut acc = 0.0f64;
        for i in 0..n {
            let y = probe.step(1.0);
            if i > 4096 {
                acc += (y as f64) * (y as f64);
            }
        }
        let rms = (acc / (n - 4097) as f64).sqrt() as f32;
        self.gain = 1.0 / rms.max(1.0e-9);
    }

    #[inline]
    fn white(&mut self) -> f32 {
        // four uniform draws: a good-enough Gaussian (variance 4/3 before the scale below)
        let mut s = 0.0;
        for _ in 0..4 {
            let mut x = self.rng;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.rng = x;
            s += (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
        }
        s * 0.866
    }

    /// One sample of unit-variance shaped noise times `amp`.
    #[inline]
    pub fn step(&mut self, amp: f32) -> f32 {
        let x = self.white();
        // two one-pole high-passes (12 dB/octave below f_lo)
        self.hp[0] += (x - self.hp[0]) * self.a_hp;
        let h1 = x - self.hp[0];
        self.hp[1] += (h1 - self.hp[1]) * self.a_hp;
        let h2 = h1 - self.hp[1];
        // the jet's spectral fall, then the top of the band
        self.lp1 += (h2 - self.lp1) * self.a_lp1;
        self.lp2 += (self.lp1 - self.lp2) * self.a_lp2;
        self.lp3 += (self.lp2 - self.lp3) * self.a_lp3;
        self.lp3 * self.gain * amp
    }
}

/// How the acoustic power at the valves becomes the noise level at the mouth: delayed by the path, smeared by the
/// pipe's reverberation, and a mix of a stationary part (what the engine puts out on average) and a firing-locked part.
#[derive(Clone, Debug)]
pub struct Envelope {
    delay: Delay,
    fast: f32,
    slow: f32,
    k_fast: f32,
    k_slow: f32,
    /// Share of the firing-locked power in the level.
    pub locked: f32,
}

impl Envelope {
    pub fn new(fs: f32, fast_s: f32, slow_s: f32, locked: f32) -> Envelope {
        Envelope {
            delay: Delay::new((0.05 * fs) as usize),
            fast: 0.0,
            slow: 0.0,
            k_fast: 1.0 - (-1.0 / (fast_s * fs)).exp(),
            k_slow: 1.0 - (-1.0 / (slow_s * fs)).exp(),
            locked,
        }
    }

    /// Feeds the jet power of this sample; returns the power level heard at the mouth, W.
    #[inline]
    pub fn step(&mut self, w: f32, delay_samples: usize) -> f32 {
        let d = self.delay.step(w, delay_samples);
        self.fast += (d - self.fast) * self.k_fast;
        self.slow += (d - self.slow) * self.k_slow;
        // a stopped engine leaves these decaying toward the denormal floats, which some CPUs are slow on
        if self.fast < 1.0e-24 {
            self.fast = 0.0;
        }
        if self.slow < 1.0e-24 {
            self.slow = 0.0;
        }
        self.locked * self.fast + (1.0 - self.locked) * self.slow
    }
}

/// Calibration of the jets' acoustic power against recordings (see the module notes): the literature efficiency
/// is only good to a factor of a few.
const JET_GAIN: f32 = 0.12;

/// The exhaust and the intake flow-noise sources of one engine.
#[derive(Clone, Debug)]
pub struct FlowLayer {
    env_x: Envelope,
    env_i: Envelope,
    shp_x: Shaper,
    shp_i: Shaper,
    att_x: f32,
    att_i: f32,
    delay_x: usize,
    delay_i: usize,
    w: (f32, f32),
    /// Scale on the jets' power (see `set_hash_scale`).
    scale: f32,
}

impl FlowLayer {
    /// `path_m` is the exhaust path from valve to tailpipe, `gas_c` the speed of sound in it, `turbine` whether a
    /// turbocharger sits in it.
    pub fn new(fs: f32, tuning: &Tuning, path_m: f32, gas_c: f32, turbine: bool) -> FlowLayer {
        // a turbine wheel in the path is a good absorber of everything above the pulses: about 6 dB more
        let (loss_x, loss_i) = (exhaust_loss_db(tuning) + if turbine { 6.0 } else { 0.0 }, intake_loss_db(tuning));
        // what is left after a lot of insertion loss is darker: the knee of the jet spectrum slides down
        let knee = |loss: f32| (1_500.0 - 14.0 * loss).clamp(700.0, 1_500.0);
        let mut shp_x = Shaper::new(0x6D2B_79F5, fs, 700.0, knee(loss_x), 4_500.0);
        let mut shp_i = Shaper::new(0x1B87_3593, fs, 600.0, knee(loss_i), 4_500.0);
        shp_x.normalise();
        shp_i.normalise();
        FlowLayer {
            env_x: Envelope::new(fs, 0.003, 0.06, 0.3),
            env_i: Envelope::new(fs, 0.003, 0.06, 0.3),
            shp_x,
            shp_i,
            att_x: 10f32.powf(-loss_x / 20.0),
            att_i: 10f32.powf(-loss_i / 20.0),
            delay_x: (path_m / gas_c.max(250.0) * fs) as usize,
            delay_i: (0.004 * fs) as usize,
            w: (0.0, 0.0),
            scale: 1.0,
        }
    }

    /// Scales the jets' acoustic power (an amplitude ratio squared is applied): the engines with many cylinders have the
    /// quietest note (their balanced pulses cancel the low orders) under the same hash, which then stands out of it.
    pub fn set_hash_scale(&mut self, amplitude_ratio: f32) {
        self.scale = amplitude_ratio * amplitude_ratio;
    }

    /// Delay of the exhaust path from valve to mouth, in audio samples, and what the fitted cat and muffler let through of a
    /// broadband sound that crosses them (an amplitude ratio).
    pub fn exhaust_delay_samples(&self) -> usize {
        self.delay_x
    }
    pub fn exhaust_transmission(&self) -> f32 {
        self.att_x
    }

    /// Takes over the noise generators of `other` (state included), keeping this layer's own shaping filters' gains.
    pub fn copy_noise_from(&mut self, other: &FlowLayer) {
        self.shp_x.copy_state_from(&other.shp_x);
        self.shp_i.copy_state_from(&other.shp_i);
    }

    /// The jets' acoustic power now (W), from [`crate::acoustics::GasPath::jet_powers`]; held until the next solver step.
    #[inline]
    pub fn set_jets(&mut self, exhaust: f32, intake: f32) {
        self.w = (exhaust, intake);
    }

    /// One audio sample: pressure at the listener from the exhaust mouth (distance `r`) and the intake mouth (`ri`), Pa.
    /// `throttle_open` (0..1) is how open the intake is to the sound travelling back up it from the valves: a choked
    /// throttle at idle is a wall to it, wide open it is not there (the manifold pressure ratio, squared, stands for it).
    #[inline]
    pub fn step(&mut self, r: f32, ri: f32, throttle_open: f32, rpm: f32) -> (f32, f32) {
        // The jets of a fast engine are the loud end of what the recordings (revving in neutral) showed, and a long
        // steady pull in a high gear keeps them there: the hiss would stand out of the note it sits on. Past
        // 2500 rpm the share they take falls by 6 dB (exhaust) and 3 dB (intake) toward 5500.
        let hi = super::mechanical::smoothstep(2_500.0, 5_500.0, rpm);
        let wx = self.env_x.step(self.w.0 * JET_GAIN * self.scale * (1.0 - 0.75 * hi), self.delay_x);
        let wi = self.env_i.step(self.w.1 * JET_GAIN * self.scale * (1.0 - 0.5 * hi), self.delay_i);
        let open = throttle_open.clamp(0.0, 1.0);
        (self.shp_x.step(pressure_at(wx, r) * self.att_x), self.shp_i.step(pressure_at(wi, ri) * self.att_i * open * open))
    }
}

/// Pressure rms at distance `r` (half space) from a source of acoustic power `w`, Pa.
#[inline]
pub fn pressure_at(w: f32, r: f32) -> f32 {
    (w.max(0.0) * RHO0 * C0 / (2.0 * std::f32::consts::PI * r * r)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jet_is_silent_without_a_pressure_ratio_and_grows_steeply_with_it() {
        let w = |pr: f32| jet_power(pr * 1.0e5, 1.0e5, 1100.0, 0.05, 1.33, 288.7);
        assert_eq!(w(1.0), 0.0);
        assert_eq!(w(0.8), 0.0);
        // doubling the pressure ratio from 1.2 to 2.4 at a fixed mass flow takes the jet from subsonic to choked
        assert!(w(2.4) > 30.0 * w(1.2), "{} vs {}", w(2.4), w(1.2));
        assert!(w(2.4).is_finite() && w(40.0).is_finite());
    }

    #[test]
    fn shaped_noise_is_unit_variance_gaussian_like_and_has_the_right_slope() {
        let fs = 44_100.0;
        let mut s = Shaper::new(7, fs, 700.0, 1_500.0, 6_000.0);
        s.normalise();
        let n = 1 << 18;
        let y: Vec<f32> = (0..n).map(|_| s.step(1.0)).collect();
        let var = y[8192..].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (n - 8192) as f64;
        assert!((var - 1.0).abs() < 0.1, "variance {var}");
        // envelope statistics of Gaussian noise: kurtosis 3
        let k = y[8192..].iter().map(|v| (*v as f64).powi(4)).sum::<f64>() / (n - 8192) as f64 / var.powi(2);
        assert!((k - 3.0).abs() < 0.4, "kurtosis {k}");
        // more of it above 1 kHz than at 200 Hz is not allowed: it is a high-passed, falling spectrum
        let band = |f0: f32| -> f64 {
            // Goertzel-ish: power near f0 by correlation with a sine, over 0.5 s
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, v) in y[8192..8192 + 22_050].iter().enumerate() {
                let ph = std::f32::consts::TAU * f0 * i as f32 / fs;
                re += *v as f64 * ph.cos() as f64;
                im += *v as f64 * ph.sin() as f64;
            }
            re * re + im * im
        };
        assert!(band(200.0) < 0.2 * band(1_000.0), "no low end");
        assert!(band(5_000.0) < band(1_000.0), "falls with frequency");
    }

    #[test]
    fn the_envelope_delays_and_smooths() {
        let mut e = Envelope::new(44_100.0, 0.003, 0.05, 0.5);
        let mut out = Vec::new();
        for i in 0..2000 {
            out.push(e.step(if i == 100 { 1.0 } else { 0.0 }, 300));
        }
        assert!(out[..399].iter().all(|v| *v == 0.0), "nothing before the path delay");
        assert!(out[420] > 0.0 && out[420] < 0.01);
    }

    /// V8 at an operating point, listener pressure of all sources (as the sound lab sums them), 2 s after a settle.
    fn render_v8(rpm: f32, throttle: f32, tuning: &Tuning) -> Vec<f32> {
        use crate::acoustics::analysis::*;
        let _ = (spectrum as fn(&[f32], usize) -> Vec<f64>, band_power as fn(&[f64], f64, f64, f64) -> f64);
        let cfg = crate::config::engine_by_id("v8").unwrap();
        let wot = throttle > 0.9;
        let inp = crate::sound::SoundInput {
            rpm,
            throttle,
            load: if wot { 1.0 } else { 0.05 },
            map_pa: if wot { 95_000.0 } else { 25_000.0 },
            atm_pa: 101_325.0,
            air_k: 293.0,
            charge_k: 300.0,
            afr: if wot { 12.8 } else { 14.6 },
            egt_k: if wot { 950.0 } else { 700.0 },
            running: true,
            lit: true,
            burn: vec![1.0; cfg.cylinder_count],
            master: 1.0,
            serial: 1,
            ..Default::default()
        };
        let mut snd = crate::sound::EngineSound::new(&cfg, tuning, &crate::sound::default_banks(&cfg), crate::sound::Quality::BALANCED);
        snd.preheat(inp.egt_k * 0.9);
        snd.set_input(&inp);
        snd.soak(1.0, 60.0);
        let mut sink = vec![0.0f32; 22_050];
        snd.render(&mut sink);
        (0..88_200)
            .map(|_| {
                snd.next_sample();
                snd.p_tail + snd.p_jet + snd.p_shell + snd.p_mech + snd.p_intake
            })
            .collect()
    }

    /// The 1 ms-window rms of the 3-9 kHz part of `x`: its largest over its mean is ~1.3 for noise and ~10 or more for a
    /// train of sharp spikes.
    fn hf_crest(x: &[f32]) -> f32 {
        let mut hi = Lowpass4::new(9_000.0, 44_100.0);
        let mut lo = Lowpass4::new(3_000.0, 44_100.0);
        let band: Vec<f32> = x.iter().map(|v| hi.step(*v) - lo.step(*v)).collect();
        let w: Vec<f32> = band[4096..].chunks(44).map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt()).collect();
        w.iter().cloned().fold(0.0f32, f32::max) / (w.iter().sum::<f32>() / w.len() as f32)
    }

    use crate::sound::radiation::Lowpass4;

    #[test]
    fn the_top_end_is_stationary_noise_that_follows_the_load() {
        use crate::acoustics::analysis::*;
        let idle = render_v8(850.0, 0.12, &Tuning::default());
        let wot = render_v8(3500.0, 1.0, &Tuning::default());
        let (c_idle, c_wot) = (hf_crest(&idle), hf_crest(&wot));
        println!("3-9 kHz 1 ms crest: idle {c_idle:.2}, WOT {c_wot:.2} (noise ~1.3, solver spikes 10+)");
        // the grid spike train (crest 10-30 at the steepened fronts) is gone: what is up there is noise (the per-tier correction lifts the 2-3 kHz edge of the band a little at idle)
        assert!(c_idle < 3.5 && c_wot < 3.5, "the top end is spiky: crest {c_idle} / {c_wot}");
        let p_idle = spectrum(&idle, 8192);
        let p_wot = spectrum(&wot, 8192);
        let band = |p: &[f64], lo: f64, hi: f64| 10.0 * band_power(p, 44_100.0, lo, hi).log10();
        let (hf_i, hf_w) = (band(&p_idle, 2_000.0, 5_000.0), band(&p_wot, 2_000.0, 5_000.0));
        let (lf_i, lf_w) = (band(&p_idle, 150.0, 700.0), band(&p_wot, 150.0, 700.0));
        println!("2-5 kHz re 150-700 Hz: idle {:.1} dB, WOT {:.1} dB; 2-5 kHz WOT - idle {:.1} dB", hf_i - lf_i, hf_w - lf_w, hf_w - hf_i);
        // recordings of a V8 at idle and revving: the 2-5 kHz band sits 25 to 30 dB under the 150-700 Hz band at idle
        // and 15 to 28 under it with the throttle open
        assert!((-37.0..-20.0).contains(&(hf_i - lf_i)), "idle top end {} dB re the body", hf_i - lf_i);
        assert!((-34.0..-12.0).contains(&(hf_w - lf_w)), "WOT top end {} dB re the body", hf_w - lf_w);
        assert!(hf_w > hf_i + 12.0, "the top end must grow with load and rpm: {} -> {}", hf_i, hf_w);
    }

    #[test]
    fn deleting_cat_and_muffler_brightens_the_top_end() {
        use crate::acoustics::analysis::*;
        let open = Tuning { cat: Cat::Delete, muffler: Muffler::Delete, ..Tuning::default() };
        let stock = render_v8(3500.0, 1.0, &Tuning::default());
        let free = render_v8(3500.0, 1.0, &open);
        let hf = |x: &[f32]| 10.0 * band_power(&spectrum(x, 8192), 44_100.0, 2_000.0, 8_000.0).log10();
        println!("2-8 kHz at 3500 rpm WOT: stock {:.1} dB, cat and muffler deleted {:.1} dB", hf(&stock), hf(&free));
        assert!(hf(&free) > hf(&stock) + 15.0, "no rasp from a straight pipe");
    }

    #[test]
    fn fitted_parts_set_the_loss() {
        let stock = Tuning::default();
        let open = Tuning { cat: Cat::Delete, muffler: Muffler::Delete, ..Tuning::default() };
        assert!(exhaust_loss_db(&stock) > exhaust_loss_db(&open) + 30.0);
        let stack = Tuning { air_filter: AirFilter::OpenStack, ..Tuning::default() };
        assert!(intake_loss_db(&stock) > intake_loss_db(&stack) + 15.0);
    }
}
