//! What a coarse solver tier loses, put back.
//!
//! The pipe solver's cells are centimetres long and its scheme smooths the steep fronts of the blow-down pulses a
//! little in every cell they cross, the more the coarser the grid: a tier with 7 cm cells radiates a good deal less
//! than the 2 cm one, more the higher the frequency (measured with `examples/tierlab.rs`, mean over engines and
//! speeds, dB re the finest tier):
//!
//! ```text
//!   Hz         63   125   250   500   1k   1.6k   2.5k
//!   28 mm     -0.1  -0.6  -0.6  -1.2  -2.4  -4.2  -5.6
//!   40 mm     -0.4  -0.5  -0.8  -1.8  -4.7  -10.9 -12.4
//!   55 mm     -3.0  -3.8  -4.3  -8.0  -13.5 -25.6 -26.0
//!   70 mm     -2.8  -3.7  -4.4  -8.6  -14.7 -28.6 -24.6
//! ```
//!
//! That is a different engine sound on every tier - the heavy engines, which the auto policy puts on the coarse
//! ones, lost their low end and the body of their note - and a step in the sound at every change of tier. The
//! correction here is a small bank of peaking filters, one set of gains per tier, fitted to those means (what is
//! left is the spread between engines, 1-3 dB below a kilohertz). The two coarsest tiers carry about 1 dB more at 320-1300 Hz
//! than the tierlab means ask for: in the finished mix (every source, a drive through the rev range) a W24 and a V8 on the 70 mm
//! tier were still 2-5 dB under the 20 mm one at 200-1500 Hz, evenly over all speeds. Gains above a few kHz are not restored: what the
//! grid has smeared out there it cannot be asked for, and the flow-noise layer supplies the top end anyway.

use std::f32::consts::TAU;

/// Centre frequencies of the peaking sections, Hz.
const CENTRES: [f32; 6] = [80.0, 160.0, 320.0, 640.0, 1280.0, 2400.0];
const Q: f32 = 1.0;

/// Gains (dB) of the six sections per cell size (m), finest first. Fitted to the measured differences above.
const TABLE: [(f32, [f32; 6]); 5] = [
    (0.020, [0.0; 6]),
    (0.028, [0.1, 0.4, 0.2, 1.0, 0.9, 5.3]),
    (0.040, [0.4, 0.1, 0.5, 0.9, 3.5, 9.4]),
    (0.055, [2.9, 1.9, 2.6, 7.7, 9.3, 12.0]),
    (0.070, [2.7, 1.8, 2.4, 9.8, 9.1, 12.0]),
];

/// The gains for a cell size: linear in the cell size between the tiers, held beyond them.
pub fn gains_for(dx: f32) -> [f32; 6] {
    if dx <= TABLE[0].0 {
        return TABLE[0].1;
    }
    for w in TABLE.windows(2) {
        let ((d0, g0), (d1, g1)) = (w[0], w[1]);
        if dx <= d1 {
            let t = (dx - d0) / (d1 - d0);
            let mut g = [0.0; 6];
            for i in 0..6 {
                g[i] = g0[i] + (g1[i] - g0[i]) * t;
            }
            return g;
        }
    }
    TABLE[TABLE.len() - 1].1
}

#[derive(Clone, Copy, Debug, Default)]
struct Section {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Section {
    /// RBJ peaking filter.
    fn peaking(f0: f32, gain_db: f32, q: f32, fs: f32) -> Section {
        let a = 10f32.powf(gain_db / 40.0);
        let w = TAU * f0 / fs;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        Section { b: [(1.0 + alpha * a) / a0, -2.0 * c / a0, (1.0 - alpha * a) / a0], a: [-2.0 * c / a0, (1.0 - alpha / a) / a0], z: [0.0; 2] }
    }

    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The correction of one tier, applied to a radiated pressure.
#[derive(Clone, Debug)]
pub struct TierEq {
    sections: Vec<Section>,
}

impl TierEq {
    pub fn new(dx: f32, fs: f32) -> TierEq {
        let g = gains_for(dx);
        TierEq { sections: CENTRES.iter().zip(g.iter()).filter(|(_, g)| g.abs() > 0.05).map(|(f, g)| Section::peaking(*f, *g, Q, fs)).collect() }
    }

    #[inline]
    pub fn step(&mut self, x: f32) -> f32 {
        let mut v = x;
        for s in self.sections.iter_mut() {
            v = s.step(v);
        }
        v
    }

    /// Gain of the whole bank at `f` Hz, dB (tests).
    pub fn response_db(dx: f32, f: f32, fs: f32) -> f32 {
        let g = gains_for(dx);
        let w = TAU * f / fs;
        let z = (w.cos(), -w.sin());
        let mul = |a: (f32, f32), b: (f32, f32)| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
        let mut db = 0.0;
        for (f0, gain) in CENTRES.iter().zip(g.iter()) {
            let s = Section::peaking(*f0, *gain, Q, fs);
            let z2 = mul(z, z);
            let num = (s.b[0] + s.b[1] * z.0 + s.b[2] * z2.0, s.b[1] * z.1 + s.b[2] * z2.1);
            let den = (1.0 + s.a[0] * z.0 + s.a[1] * z2.0, s.a[0] * z.1 + s.a[1] * z2.1);
            db += 10.0 * ((num.0 * num.0 + num.1 * num.1) / (den.0 * den.0 + den.1 * den.1)).log10();
        }
        db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_finest_tier_is_untouched_and_coarser_ones_are_lifted_more() {
        assert_eq!(TierEq::new(0.020, 44_100.0).sections.len(), 0);
        let at = |dx: f32, f: f32| TierEq::response_db(dx, f, 44_100.0);
        for f in [100.0, 400.0, 1000.0, 2000.0] {
            assert!(at(0.020, f).abs() < 1e-3);
            assert!(at(0.028, f) <= at(0.040, f) + 0.2 && at(0.040, f) <= at(0.055, f) + 0.2, "{f} Hz: {} {} {}", at(0.028, f), at(0.040, f), at(0.055, f));
        }
        // the fit: the 40 mm tier lost about 4.7 dB at 1 kHz and 12 at 2.5 kHz
        assert!((at(0.040, 1008.0) - 5.0).abs() < 2.0, "{}", at(0.040, 1008.0));
        assert!(at(0.040, 2400.0) > 8.0);
        // and a low end is put back on the 55 mm one: it lost 3-4 dB at 100-250 Hz
        assert!(at(0.055, 160.0) > 3.0, "{}", at(0.055, 160.0));
    }

    #[test]
    fn the_filter_is_stable_and_matches_its_own_response() {
        let fs = 44_100.0;
        let mut eq = TierEq::new(0.055, fs);
        // a sine at 500 Hz comes out louder by the response at 500 Hz
        let f = 500.0;
        let n = 20_000;
        let (mut ein, mut eout) = (0.0f64, 0.0f64);
        for i in 0..n {
            let x = (TAU * f * i as f32 / fs).sin();
            let y = eq.step(x);
            if i > n / 2 {
                ein += (x * x) as f64;
                eout += (y * y) as f64;
            }
        }
        let measured = 10.0 * (eout / ein).log10() as f32;
        let predicted = TierEq::response_db(0.055, f, fs);
        assert!((measured - predicted).abs() < 0.3, "{measured} vs {predicted}");
        // nothing blows up on an impulse
        let mut eq = TierEq::new(0.07, fs);
        let mut peak = 0.0f32;
        for i in 0..10_000 {
            peak = peak.max(eq.step(if i == 0 { 1.0 } else { 0.0 }).abs());
        }
        assert!(peak < 10.0 && eq.step(0.0).abs() < 1e-6);
    }
}
