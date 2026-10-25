//! From pipe flow to sound pressure at a listener: tailpipe monopoles, jet noise, shell radiation.

use crate::acoustics::exhaust::Body;
use crate::acoustics::material::MaterialProps;

pub const RHO0: f32 = 1.2;
pub const C0: f32 = 343.0;

/// A volume source radiates `p = rho0 / (4 pi r) dQ/dt` in free field and twice that against a hard
/// plane; a tailpipe a third of a metre above the road is in between (ground reflection adds ~3 dB).
/// Only the *change* of the volume flow is heard, which is why an exhaust has no DC and a spectral tilt
/// of +6 dB/octave up to where the pipe's own response takes over.
#[inline]
pub fn monopole_pressure(dq_dt: f32, r: f32) -> f32 {
    1.4 * RHO0 / (4.0 * std::f32::consts::PI * r) * dq_dt
}

/// A fourth-order Butterworth low-pass (two biquads): what the pipe solver cannot resolve - its cells are
/// centimetres long - and what a listener metres away does not receive, cut out of the monopole radiation.
#[derive(Clone, Copy, Debug)]
pub struct Lowpass4 {
    b: [[f32; 3]; 2],
    a: [[f32; 2]; 2],
    z: [[f32; 2]; 2],
}

impl Lowpass4 {
    pub fn new(fc: f32, fs: f32) -> Lowpass4 {
        let mut b = [[0.0; 3]; 2];
        let mut a = [[0.0; 2]; 2];
        // Q of the two sections of a fourth-order Butterworth
        for (i, q) in [0.541_196_1f32, 1.306_563].iter().enumerate() {
            let w = std::f32::consts::TAU * fc / fs;
            let (s, c) = w.sin_cos();
            let alpha = s / (2.0 * q);
            let a0 = 1.0 + alpha;
            b[i] = [(1.0 - c) * 0.5 / a0, (1.0 - c) / a0, (1.0 - c) * 0.5 / a0];
            a[i] = [-2.0 * c / a0, (1.0 - alpha) / a0];
        }
        Lowpass4 { b, a, z: [[0.0; 2]; 2] }
    }

    #[inline]
    pub fn step(&mut self, x: f32) -> f32 {
        let mut v = x;
        for i in 0..2 {
            // transposed direct form II
            let y = self.b[i][0] * v + self.z[i][0];
            self.z[i][0] = self.b[i][1] * v - self.a[i][0] * y + self.z[i][1];
            self.z[i][1] = self.b[i][2] * v - self.a[i][1] * y;
            v = y;
        }
        v
    }
}

/// The volume flow out of a mouth, one value per solver step, kept as a cubic-Hermite curve so the rate of
/// change that radiates is *continuous* from audio sample to audio sample. Differentiating the straight
/// line between solver outputs instead gives a staircase whose steps repeat at the solver's rate: a hard
/// narrow-band tone at the top of the audio band (and, on the coarse solvers, in the middle of it).
///
/// The curve lags the solver by one step (the tangent at a point needs the point after it).
#[derive(Clone, Copy, Debug, Default)]
pub struct FlowHist {
    q: [f32; 4],
    /// `h[i]` is the time from `q[i]` to `q[i + 1]`.
    h: [f32; 3],
    primed: bool,
}

impl FlowHist {
    /// Appends the flow at the end of a solver step of length `h`.
    #[inline]
    pub fn push(&mut self, q: f32, h: f32) {
        let h = h.max(1.0e-7);
        if !self.primed {
            // the first value stands for the whole history, so it does not read as a jump from zero
            *self = FlowHist { q: [q; 4], h: [h; 3], primed: true };
        }
        self.q = [self.q[1], self.q[2], self.q[3], q];
        self.h = [self.h[1], self.h[2], h];
    }

    /// Three-point derivative at the middle of a non-uniform triple.
    #[inline]
    fn slope(f0: f32, f1: f32, f2: f32, h0: f32, h1: f32) -> f32 {
        let s = h0 + h1;
        -h1 / (h0 * s) * f0 + (h1 - h0) / (h0 * h1) * f1 + h0 / (h1 * s) * f2
    }

    /// `dq/dt` at fraction `s` (0..1) of the interval between the two middle points.
    #[inline]
    pub fn dq_dt(&self, s: f32) -> f32 {
        let (q, h) = (&self.q, &self.h);
        let m1 = Self::slope(q[0], q[1], q[2], h[0], h[1]);
        let m2 = Self::slope(q[1], q[2], q[3], h[1], h[2]);
        let s = s.clamp(0.0, 1.0);
        let h1 = h[1];
        (6.0 * s * s - 6.0 * s) * (q[1] - q[2]) / h1 + (3.0 * s * s - 4.0 * s + 1.0) * m1 + (3.0 * s * s - 2.0 * s) * m2
    }
}

/// Broadband jet noise from a mouth: Lighthill's `u^8` acoustic power, shaped as a band-pass that
/// follows the jet's Strouhal frequency `0.2 u / D`. Unsteady flow modulates it at the firing rate,
/// which is the rasp on top of the pulses.
#[derive(Clone, Debug)]
pub struct JetNoise {
    rng: u32,
    lp1: f32,
    lp2: f32,
    hp: f32,
    diameter: f32,
}

impl JetNoise {
    pub fn new(diameter: f32, seed: u32) -> Self {
        JetNoise { rng: seed | 1, lp1: 0.0, lp2: 0.0, hp: 0.0, diameter }
    }

    fn noise(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// `u` is the exit velocity, m/s (any sign); returns pressure at distance `r`.
    pub fn step(&mut self, u: f32, r: f32, fs: f32) -> f32 {
        let au = u.abs();
        // p_rms = sqrt(K/4pi) rho0 u^4 D / (c^2 r) with K ~ 3e-5, and a safety taper for the very
        // low speeds where the correlation does not apply
        let amp = 1.5e-3 * RHO0 * au.powi(4) * self.diameter / (C0 * C0 * r) * (au / (au + 4.0));
        let fc = (0.2 * au / self.diameter).clamp(150.0, 7000.0);
        let k = 1.0 - (-std::f32::consts::TAU * fc / fs).exp();
        let n = self.noise() * 1.7;
        self.lp1 += (n - self.lp1) * k;
        self.lp2 += (self.lp1 - self.lp2) * k;
        // band-pass: remove the content below ~fc/3
        let kh = 1.0 - (-std::f32::consts::TAU * (fc / 3.0) / fs).exp();
        self.hp += (self.lp2 - self.hp) * kh;
        (self.lp2 - self.hp) * amp
    }
}

#[derive(Clone, Debug)]
struct Mode {
    w: f32,
    zeta: f32,
    /// excitation per pascal (m/s^2 per Pa) and radiation per unit acceleration (Pa s^2/m at the listener)
    drive: f32,
    rad: f32,
    x: f32,
    v: f32,
}

/// The vibrating shell of one pipe / can: a handful of ovalling modes whose frequencies follow the
/// material (stiffness over density), thickness and radius, driven by the pressure inside.
#[derive(Clone, Debug)]
pub struct ShellBody {
    modes: Vec<Mode>,
    p_lp: f32,
    primed: bool,
    pub name: String,
}

/// Calibration of the shell excitation: the share of the plane-wave pressure that reaches the
/// non-axisymmetric modes through bends, junctions and chamber cross-modes.
const SHELL_COUPLING: f32 = 0.0035;

/// Damping a mounted exhaust always has on top of its material's (hangers, joints, flanges, gas coupling).
const ASSEMBLY_LOSS: f32 = 0.006;

impl ShellBody {
    pub fn new(body: &Body, props: &MaterialProps, listener_r: f32, fs: f32) -> ShellBody {
        let radius = 0.5 * body.diameter;
        let t = body.wall.thickness_m;
        let cl = props.plate_wave_speed();
        let ms = body.wall.mass_area.max(0.5);
        let eta = body.wall.loss_factor + ASSEMBLY_LOSS;
        // radiating area is shared by the modes; higher modes couple less
        let mut modes = Vec::new();
        for n in 2..=7usize {
            let nf = n as f32;
            let f = cl * t / (std::f32::consts::TAU * radius * radius * 12f32.sqrt()) * nf * (nf * nf - 1.0) / (nf * nf + 1.0).sqrt();
            if f < 40.0 || f > 0.12 * fs {
                continue;
            }
            // radiation efficiency of a low-order shell mode climbs toward 1 with frequency
            let sigma = (f / (f + 700.0)).min(1.0);
            let w = std::f32::consts::TAU * f;
            let kappa = SHELL_COUPLING / nf;
            modes.push(Mode {
                w,
                zeta: 0.5 * eta,
                drive: kappa / ms,
                rad: RHO0 * sigma * body.surface / (4.0 * std::f32::consts::PI * listener_r) / (nf * nf).sqrt(),
                x: 0.0,
                v: 0.0,
            });
        }
        ShellBody { modes, p_lp: 0.0, primed: false, name: body.name.clone() }
    }

    /// Frequencies of the modes, Hz (for reports and tests).
    pub fn frequencies(&self) -> Vec<f32> {
        self.modes.iter().map(|m| m.w / std::f32::consts::TAU).collect()
    }

    /// `p_body` is the mean gas pressure in the body; returns the radiated pressure at the listener.
    pub fn step(&mut self, p_body: f32, dt: f32) -> f32 {
        // only the fluctuating part drives the shell
        if !self.primed {
            self.p_lp = p_body;
            self.primed = true;
        }
        self.p_lp += (p_body - self.p_lp) * (1.0 - (-dt / 0.05).exp());
        let p = p_body - self.p_lp;
        let mut out = 0.0;
        for m in self.modes.iter_mut() {
            let a = m.drive * p - 2.0 * m.zeta * m.w * m.v - m.w * m.w * m.x;
            m.v += a * dt;
            m.x += m.v * dt;
            out += m.rad * a;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acoustics::material::{PipeMaterial, Wall};

    fn body(mat: PipeMaterial, t_mm: f32, dia: f32) -> Body {
        let wall = Wall::pipe(mat, t_mm * 1e-3, false, dia);
        Body { name: "b".into(), ducts: vec![], surface: std::f32::consts::PI * dia * 0.5, diameter: dia, wall }
    }

    /// A sine of `f` Hz sampled by solver steps of slowly varying length (about 1.8 samples, as the CFL limit gives), differentiated per audio sample.
    fn hermite_derivative(f: f32, linear: bool) -> (f32, f32, f32) {
        let fs = 44_100.0f32;
        let w = std::f32::consts::TAU * f;
        let q = |t: f32| (w * t).sin();
        let mut hist = FlowHist::default();
        let (mut t_solver, mut prev_q, mut prev_dq, mut h) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let (mut step_clock, mut step_len) = (0.0f32, 0.0f32);
        let (mut worst, mut jump, mut last) = (0.0f32, 0.0f32, None::<f32>);
        let mut k = 0u32;
        for i in 0..4000 {
            step_clock += 1.0 / fs;
            while step_clock > step_len {
                step_clock -= step_len;
                k += 1;
                h = (1.8 + 0.15 * (0.03 * k as f32).sin()) / fs;
                t_solver += h;
                let qn = q(t_solver);
                if linear {
                    prev_dq = (qn - prev_q) / h;
                    prev_q = qn;
                }
                hist.push(qn, h);
                step_len = h;
            }
            let frac = (step_clock / step_len).clamp(0.0, 1.0);
            let d = if linear { prev_dq } else { hist.dq_dt(frac) };
            // the interpolant is delayed by one step; compare against the derivative at the time it represents
            let t_rep = t_solver - h + frac * h - h;
            let exact = w * (w * t_rep).cos();
            if i > 200 {
                worst = worst.max((d - exact).abs() / w);
                if let Some(l) = last {
                    jump = jump.max((d - l).abs() / w);
                }
            }
            last = Some(d);
        }
        (worst, jump, w)
    }

    #[test]
    fn the_flow_derivative_is_smooth_between_solver_steps() {
        // (the interpolant lags the solver by one step, which the comparison accounts for)
        // a 1 kHz flow sampled by solver steps of about 1.8 samples: the Hermite derivative follows the true one closely and, unlike the
        // slope of the straight line between the steps, has no jump at each step boundary
        let (err_h, jump_h, w) = hermite_derivative(1000.0, false);
        let (err_l, jump_l, _) = hermite_derivative(1000.0, true);
        // the derivative of a sine itself changes by w / fs of its amplitude from one sample to the next
        let natural = w / 44_100.0;
        println!("derivative error / step-boundary jump (relative to the amplitude): hermite {err_h:.3} / {jump_h:.3}, linear {err_l:.3} / {jump_l:.3}");
        assert!(err_h < 0.06, "hermite derivative error {err_h}");
        assert!(jump_h < 1.15 * natural, "no jumps beyond the signal's own slope: {jump_h} vs {natural}");
        assert!(jump_l > 1.5 * natural, "the straight-line derivative is a staircase: {jump_l} vs {natural}");
    }

    #[test]
    fn the_radiation_low_pass_cuts_the_top_and_leaves_the_body() {
        let fs = 44_100.0f32;
        let gain = |f: f32| {
            let mut lp = Lowpass4::new(7_500.0, fs);
            let n = 8192;
            let (mut acc, mut cnt) = (0.0f64, 0usize);
            for i in 0..n {
                let y = lp.step((std::f32::consts::TAU * f * i as f32 / fs).sin());
                if i > 2048 {
                    acc += (y as f64).powi(2);
                    cnt += 1;
                }
            }
            20.0 * ((acc / cnt as f64 * 2.0).sqrt()).log10()
        };
        let (g1, g5, g7, g18) = (gain(1_000.0), gain(5_000.0), gain(7_500.0), gain(18_000.0));
        println!("low-pass gain: 1 kHz {g1:.2} dB, 5 kHz {g5:.2}, 7.5 kHz {g7:.2}, 18 kHz {g18:.1}");
        assert!(g1.abs() < 0.1 && g5.abs() < 1.0, "the passband must be flat");
        assert!((g7 + 3.0).abs() < 0.6, "-3 dB at the corner");
        assert!(g18 < -35.0, "the top of the band must be gone ({g18} dB)");
    }

    #[test]
    fn shell_modes_follow_thickness_and_radius() {
        let thin = ShellBody::new(&body(PipeMaterial::MildSteel, 1.0, 0.15), &PipeMaterial::MildSteel.props(), 1.5, 44100.0);
        let thick = ShellBody::new(&body(PipeMaterial::MildSteel, 2.0, 0.15), &PipeMaterial::MildSteel.props(), 1.5, 44100.0);
        let small = ShellBody::new(&body(PipeMaterial::MildSteel, 1.0, 0.06), &PipeMaterial::MildSteel.props(), 1.5, 44100.0);
        let (ft, fk, fs) = (thin.frequencies(), thick.frequencies(), small.frequencies());
        println!("can modes: thin {:?}", ft);
        assert!((fk[0] / ft[0] - 2.0).abs() < 0.05, "mode frequency scales with thickness");
        assert!(fs[0] > 3.0 * ft[0], "a narrower shell is much stiffer");
    }

    #[test]
    fn titanium_shell_rings_longer_and_louder_than_steel() {
        let run = |mat: PipeMaterial, t_mm: f32| {
            let mut sb = ShellBody::new(&body(mat, t_mm, 0.15), &mat.props(), 1.5, 44100.0);
            let dt = 1.0 / 44100.0;
            // a 3 ms pressure knock, then listen to the ring-down
            let mut early = 0.0f32;
            let mut late = 0.0f32;
            for i in 0..22_050 {
                let p = if i < 130 { 8000.0 * (std::f32::consts::PI * i as f32 / 130.0).sin() } else { 0.0 };
                let o = sb.step(101_325.0 + p, dt);
                if (130..2000).contains(&i) {
                    early = early.max(o.abs());
                }
                if (11_000..22_050).contains(&i) {
                    late = late.max(o.abs());
                }
            }
            (early, late)
        };
        let (se, sl) = run(PipeMaterial::MildSteel, 1.1);
        let (te, tl) = run(PipeMaterial::Titanium, 0.8);
        let (ce, cl) = run(PipeMaterial::CastIron, 4.0);
        println!("shell response: steel {se:.2e}/{sl:.2e}, titanium {te:.2e}/{tl:.2e}, cast iron {ce:.2e}/{cl:.2e}");
        assert!(te > se, "thin titanium radiates more than steel");
        assert!(tl / te > sl / se, "titanium rings longer relative to its onset");
        assert!(ce < se, "a thick cast-iron shell is much quieter");
    }
}
