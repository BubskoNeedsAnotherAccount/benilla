//! The low orders of the crank: the rumble under an engine's note.
//!
//! A real engine is never the perfectly matched set of cylinders the pipe model makes of it. Every cylinder breathes,
//! burns and blows down a little differently from its neighbours, the exhaust runners and the intake runners are not
//! all the same length, the crank flexes, the reciprocating masses and the gas torque shake the block on its mounts:
//! all of it shows up as content at the *low orders of the crank* - once, one and a half, twice per revolution -
//! well under the firing frequency. An engine whose firing frequency is high (a six or a twelve at speed, where the
//! fundamental is 250 Hz and more) is made only of its firing harmonics without them, and sounds thin: the spectrogram
//! is one bright line and its overtones, and nothing under it.
//!
//! This layer puts those orders in where the model left them out. It does not add a fixed amount: it measures the
//! band under 150 Hz and the band above it (260-1500 Hz) in what the engine already makes, and adds just what is
//! missing to bring the low band up to a floor that falls with engine speed (a smooth idle has plenty down there;
//! a fast engine less, as in the recordings). An engine that already has its low end (the cross-plane V8's half
//! orders, a four's firing fundamental, a diesel) gets nothing. The partials are phase-locked to the crank angle
//! like any engine order, each wandering in level and phase so that they are a rumble and not a chord.

use super::mechanical::{smoothstep, Wander};

/// Q of the two sections of a fourth-order Butterworth filter.
const Q4: [f32; 2] = [0.541_196_1, 1.306_563];

/// Orders of the crank (cycles per crank revolution) the rumble is made of; half orders are once per engine cycle.
const ORDERS: [f32; 10] = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0];

/// Share of the low band (30-150 Hz) in the power of the body band (260-1500 Hz) the layer holds the engine to, dB:
/// at idle, at 2000 rpm and from 3000 rpm up. The cross-plane V8 and the four, which sound right, make about -1 to -3,
/// -4 to -8 and -6 to -10 on their own.
const FLOOR_DB: [f32; 3] = [-3.0, -5.0, -6.5];

/// Extra floor (dB) of an engine with many cylinders: a W16 or a V16 is the furthest from the matched-cylinder
/// idealisation to start with (sixteen near-identical firings leave only the firing harmonics, 250-400 Hz and up) and
/// is the one that sounded thin and buzzy with a rumble no deeper than a six's. It grows from eight cylinders to sixteen,
/// and from 40 % of it at idle to all of it from 2000 rpm (an idling W16 already has a lump to it).
const MANY_CYLINDERS_DB: f32 = 7.5;

/// Lowest and highest frequency a partial is heard at, Hz (it fades in over the first and out over the last).
const BAND: (f32, f32, f32, f32) = (28.0, 48.0, 140.0, 210.0);

/// A second-order Butterworth section (RBJ cookbook), transposed direct form II.
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn new(fc: f32, q: f32, fs: f32, highpass: bool) -> Biquad {
        let w = std::f32::consts::TAU * fc / fs;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = if highpass {
            [(1.0 + c) * 0.5 / a0, -(1.0 + c) / a0, (1.0 + c) * 0.5 / a0]
        } else {
            [(1.0 - c) * 0.5 / a0, (1.0 - c) / a0, (1.0 - c) * 0.5 / a0]
        };
        Biquad {
            b,
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }

    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

#[derive(Clone, Debug)]
pub struct LowEnd {
    fs: f32,
    // the analysis bands: 30-150 Hz and 260-1500 Hz, sharp enough that the firing line of a fast six, a quarter of a
    // kHz, does not count as low end
    low_hp: Biquad,
    low_lp: [Biquad; 2],
    body_hp: [Biquad; 2],
    body_lp: Biquad,
    // band powers (smoothed), Pa^2
    p_low: f32,
    p_body: f32,
    k_env: f32,
    /// What the partials add, rms Pa (smoothed).
    amp: f32,
    phase: [f32; ORDERS.len()],
    /// Added to the floor, dB (`MANY_CYLINDERS_DB` scaled by the cylinder count).
    boost_db: f32,
    am: Vec<Wander>,
    pm: Vec<Wander>,
    rng: u32,
}

impl LowEnd {
    pub fn new(fs: f32) -> LowEnd {
        let mut s = LowEnd {
            fs,
            low_hp: Biquad::new(30.0, Q4[0], fs, true),
            low_lp: [
                Biquad::new(150.0, Q4[0], fs, false),
                Biquad::new(150.0, Q4[1], fs, false),
            ],
            body_hp: [
                Biquad::new(260.0, Q4[0], fs, true),
                Biquad::new(260.0, Q4[1], fs, true),
            ],
            body_lp: Biquad::new(1500.0, Q4[0], fs, false),
            p_low: 0.0,
            p_body: 0.0,
            k_env: 1.0 - (-1.0 / (0.35 * fs)).exp(),
            amp: 0.0,
            boost_db: 0.0,
            phase: [0.0; ORDERS.len()],
            am: ORDERS
                .iter()
                .enumerate()
                .map(|(i, _)| Wander::new(3.0 + 0.9 * i as f32, fs))
                .collect(),
            pm: ORDERS
                .iter()
                .enumerate()
                .map(|(i, _)| Wander::new(4.0 + 1.1 * i as f32, fs))
                .collect(),
            rng: 0xC0FF_EE11,
        };
        // fixed, unrelated phases: the partials of a real engine are not in step with each other
        for (i, p) in s.phase.iter_mut().enumerate() {
            *p = (i as f32 * 2.399_963).fract() * std::f32::consts::TAU;
        }
        s
    }

    /// A layer for an engine of `cylinders` cylinders: the many-cylinder engines get a deeper floor.
    pub fn for_cylinders(fs: f32, cylinders: usize) -> LowEnd {
        let mut l = LowEnd::new(fs);
        l.boost_db = MANY_CYLINDERS_DB * smoothstep(8.0, 16.0, cylinders as f32);
        l
    }

    fn noise(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Floor of the low band re the body band, as a power ratio, at this engine speed.
    pub fn floor_ratio(rpm: f32) -> f32 {
        Self::floor_ratio_boosted(rpm, 0.0)
    }

    fn floor_ratio_boosted(rpm: f32, boost_db: f32) -> f32 {
        let db = boost_db * (0.4 + 0.6 * smoothstep(900.0, 2_000.0, rpm))
            + if rpm < 2_000.0 {
                FLOOR_DB[0] + (FLOOR_DB[1] - FLOOR_DB[0]) * smoothstep(900.0, 2_000.0, rpm)
            } else {
                FLOOR_DB[1] + (FLOOR_DB[2] - FLOOR_DB[1]) * smoothstep(2_000.0, 3_000.0, rpm)
            };
        10f32.powf(db / 10.0)
    }

    /// Band powers of the signal so far (low, body), Pa^2 (analysis).
    pub fn powers(&self) -> (f32, f32) {
        (self.p_low, self.p_body)
    }

    /// Takes this sample's pressure `p` (what the engine makes without the rumble), the crank angle in degrees
    /// (0..720), the engine speed and how active the engine is (0..1); returns the rumble to add, Pa.
    #[inline]
    pub fn step(&mut self, p: f32, crank_deg: f32, rpm: f32, active: f32) -> f32 {
        // band powers of what is there
        let v = self.low_hp.step(p);
        let v = self.low_lp[0].step(v);
        let low = self.low_lp[1].step(v);
        let v = self.body_hp[0].step(p);
        let v = self.body_hp[1].step(v);
        let body = self.body_lp.step(v);
        self.p_low += (low * low - self.p_low) * self.k_env;
        self.p_body += (body * body - self.p_body) * self.k_env;
        if self.p_low < 1.0e-20 {
            self.p_low = 0.0;
        }
        if self.p_body < 1.0e-20 {
            self.p_body = 0.0;
        }

        // what is missing, rms, followed slowly (a rumble that pumps with every gust of the signal is not one)
        let missing =
            (Self::floor_ratio_boosted(rpm, self.boost_db) * self.p_body - self.p_low).max(0.0);
        let want = missing.sqrt() * active;
        let k = if want > self.amp {
            1.0 / (0.3 * self.fs)
        } else {
            1.0 / (0.6 * self.fs)
        };
        self.amp += (want - self.amp) * k;
        if self.amp < 1.0e-9 {
            return 0.0;
        }

        // the partials in range, phase-locked to the crank
        let crank = crank_deg.to_radians();
        let crank_hz = rpm / 60.0;
        let (mut sum, mut pow) = (0.0f32, 0.0f32);
        for (i, &o) in ORDERS.iter().enumerate() {
            let f = o * crank_hz;
            let w =
                smoothstep(BAND.0, BAND.1, f) * (1.0 - smoothstep(BAND.2, BAND.3, f)) / o.sqrt();
            // the wander generators must keep running whether or not the partial is audible
            let (na, np) = (self.noise(), self.noise());
            let a = (1.0 + 0.45 * self.am[i].step(na)).max(0.15);
            // a phase that wanders by most of a radian within a tenth of a second makes each order a line a few Hz wide
            // (a rough rumble) and not a hum
            let ph = 0.9 * self.pm[i].step(np);
            if w <= 0.0 {
                continue;
            }
            let wa = w * a;
            sum += wa * (o * crank + self.phase[i] + ph).sin();
            pow += 0.5 * wa * wa;
        }
        if pow < 1.0e-6 {
            return 0.0;
        }
        // the rumble stops with the engine, whatever the followers above still remember
        sum / pow.sqrt() * self.amp * active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(rpm: f32, low_amp: f32, body_amp: f32, secs: f32) -> (f32, f32, f32) {
        let fs = 44_100.0;
        let mut l = LowEnd::new(fs);
        let mut crank = 0.0f32;
        let (mut e_fill, mut e_in, mut e_all) = (0.0f64, 0.0f64, 0.0f64);
        let n = (secs * fs) as usize;
        for i in 0..n {
            crank = (crank + rpm * 6.0 / fs).rem_euclid(720.0);
            let t = i as f32 / fs;
            let p = low_amp * (std::f32::consts::TAU * 120.0 * t).sin()
                + body_amp * (std::f32::consts::TAU * 500.0 * t).sin();
            let f = l.step(p, crank, rpm, 1.0);
            if i > n / 2 {
                e_fill += (f as f64).powi(2);
                e_in += (p as f64).powi(2);
                e_all += ((p + f) as f64).powi(2);
            }
        }
        let m = (n - n / 2 - 1) as f64;
        (
            (e_fill / m).sqrt() as f32,
            (e_in / m).sqrt() as f32,
            (e_all / m).sqrt() as f32,
        )
    }

    #[test]
    fn an_engine_with_its_low_end_gets_nothing() {
        // the low band is already as strong as the body: no fill
        let (fill, _, _) = run(900.0, 2.0, 1.0, 3.0);
        assert!(fill < 0.02, "fill {fill}");
    }

    #[test]
    fn a_thin_engine_is_brought_up_to_the_floor_and_no_further() {
        // a 500 Hz tone and nothing under 200 Hz at 4000 rpm: the low band must end up near -9 dB of the body's power
        let (fill, input, _) = run(4_000.0, 0.0, 1.0, 6.0);
        let ratio_db = 20.0 * (fill / input).log10();
        println!("fill {fill:.3}, input {input:.3}, ratio {ratio_db:.1} dB");
        assert!(
            (ratio_db + 9.0).abs() < 2.0,
            "the rumble sits at {ratio_db} dB of the body"
        );
    }

    #[test]
    fn it_is_silent_when_the_engine_is_not() {
        let fs = 44_100.0;
        let mut l = LowEnd::new(fs);
        for i in 0..44_100 {
            let p = 0.5 * (i as f32 * 0.07).sin();
            l.step(p, (i % 720) as f32, 3_000.0, 1.0);
        }
        let mut peak = 0.0f32;
        for i in 0..88_200 {
            // the engine is off: no pressure, no activity
            peak = peak.max(l.step(0.0, (i % 720) as f32, 3_000.0, 0.0).abs());
        }
        assert!(peak < 1.0e-3, "rumble of an engine that is off: {peak}");
    }
}
