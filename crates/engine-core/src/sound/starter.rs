//! The starter motor, its pinion and the ring gear: what a key turn sounds like before the engine lights.
//!
//! Heard from outside a starter is a rough, buzzing whirr that chugs: low and mid-range (a few hundred Hz to two kHz),
//! not hiss. The first version was three clean tones that wobbled; the second matched the *flat* octave bands of two
//! recordings with a wide noise floor up to 6 kHz, and that read as gas rushing - an airy hiss with nothing to
//! hold on to. What makes the sound:
//!  * the *armature*: the motor turns about thirteen times for every turn of the crank and each of its turns is a
//!    pulse (commutator, brushes, pole pieces), so there is a buzz at the armature's speed - 50 to 100 pulses a
//!    second - whose pulses are all a little different and ring the housing's low modes. This is the "rrrrr";
//!  * the *ring gear*: every tooth that passes the pinion is a small impact, at (teeth x crank speed) with a large
//!    random scatter in spacing and strength, ringing damped modes of the housing at 1-3 kHz;
//!  * a light *hash* of brushes and fan turbulence under it, rolled off above 2-3 kHz;
//!  * the *solenoid and pinion*: a snap when the pinion is thrown in and a clunk when it is let go.
//!
//! Its pace and its strength are the crank's: it surges and sags with every compression stroke and the motor works
//! hardest (loudest, lowest) against each one, which is the chug of a cranking engine.

use super::mechanical::{Noise, Res};

/// Teeth on the ring gear: bigger engines have bigger flywheels.
fn ring_teeth(displacement_l: f32) -> f32 {
    (108.0 + 9.0 * displacement_l).clamp(110.0, 168.0)
}

/// Armature turns per crank turn (ring teeth over pinion teeth).
fn gear_ratio(teeth: f32) -> f32 {
    teeth / 9.5
}

/// What the starter needs to know each sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct StarterCtx {
    /// The pinion is thrown in and the motor driven.
    pub on: bool,
    /// Instantaneous crank speed (including the sag and surge of the compression strokes), rpm.
    pub rpm: f32,
    /// 0..1: how hard the motor is working against a compression stroke right now.
    pub strain: f32,
}

#[derive(Clone, Debug)]
pub struct Starter {
    fs: f32,
    rng: Noise,
    teeth: f32,
    ratio: f32,
    /// Solenoid and pinion engagement, 0..1.
    env: f32,
    was_on: bool,
    /// Armature speed, turns per second.
    motor_rps: f32,
    tooth_phase: f32,
    /// Phase at which the next tooth lands (1 plus the scatter of the tooth spacing).
    tooth_next: f32,
    whine_phase: f32,
    /// Housing modes that a tooth impact excites: heavily damped, so each click is a short tick.
    mesh: [Res; 3],
    /// Housing and gearbox modes the armature's pulses ring: the body of the whirr.
    armature: [Res; 3],
    arm_phase: f32,
    /// Strength of the pulse of the current armature turn.
    arm_hit: f32,
    /// Solenoid plunger and pinion throw.
    clack: [Res; 2],
    /// Impact and grind envelopes: the snap of the pinion, the grind as the teeth find each other.
    snap: f32,
    grind: f32,
    /// Pinking filter state (Kellet) and the high-pass and low-pass poles that shape the motor's hash.
    pink: [f32; 3],
    hp1: f32,
    hash_lp: f32,
    air_lp: f32,
    tone_lp: f32,
    out_lp: f32,
    /// Slow drift of the speed of the teeth against the pinion.
    wander: f32,
}

impl Starter {
    pub fn new(fs: f32, displacement_l: f32) -> Starter {
        let teeth = ring_teeth(displacement_l);
        Starter {
            fs,
            rng: Noise::new(0x7A3F_19C5),
            teeth,
            ratio: gear_ratio(teeth),
            env: 0.0,
            was_on: false,
            motor_rps: 0.0,
            tooth_phase: 0.0,
            tooth_next: 1.0,
            whine_phase: 0.0,
            mesh: [Res::new(1250.0, 0.20, fs), Res::new(2700.0, 0.26, fs), Res::new(4600.0, 0.30, fs)],
            armature: [Res::new(310.0, 0.16, fs), Res::new(760.0, 0.14, fs), Res::new(1500.0, 0.18, fs)],
            arm_phase: 0.0,
            arm_hit: 1.0,
            clack: [Res::new(170.0, 0.09, fs), Res::new(1500.0, 0.12, fs)],
            snap: 0.0,
            grind: 0.0,
            pink: [0.0; 3],
            hp1: 0.0,
            hash_lp: 0.0,
            air_lp: 0.0,
            tone_lp: 0.0,
            out_lp: 0.0,
            wander: 0.0,
        }
    }

    /// Armature speed, turns per second (analysis).
    pub fn motor_rps(&self) -> f32 {
        self.motor_rps
    }

    /// One sample, in pascals at the listener.
    pub fn step(&mut self, c: &StarterCtx) -> f32 {
        let dt = 1.0 / self.fs;
        let tau = std::f32::consts::TAU;
        let n = self.rng.next();
        let n2 = self.rng.next();
        let n3 = self.rng.next();

        // ---- solenoid: pulls in fast, drops out slower
        let k_env = 1.0 - (-dt / if c.on { 0.006 } else { 0.05 }).exp();
        self.env += ((c.on as u8 as f32) - self.env) * k_env;
        let mut kick = 0.0;
        if c.on && !self.was_on {
            kick = 1.0;
            self.snap = 1.0;
            self.grind = 1.0;
        } else if !c.on && self.was_on {
            kick = -0.45;
            self.snap = 0.4;
        }
        self.was_on = c.on;
        // the pinion hits the ring gear: a wide, short snap, the plunger's thunk and a ring of the housing
        let mut out = 0.02 * self.clack[0].step(kick) + 0.028 * self.clack[1].step(kick) + self.snap * n3 * 0.07;
        self.snap *= (-dt / 0.0035).exp();

        // ---- armature speed: locked to the crank through the gears while the pinion is in (an overrunning
        // clutch lets the engine run ahead of a starter that tops out), then free to spin down
        let crank_rps = (c.rpm / 60.0).max(0.0);
        let driven = (crank_rps.min(7.4)) * self.ratio;
        let tau_motor = if c.on { 0.015 } else { 0.35 };
        let target = if c.on { driven } else { 0.0 };
        self.motor_rps += (target - self.motor_rps) * (1.0 - (-dt / tau_motor).exp());
        if self.motor_rps < 0.5 && self.env < 0.01 {
            self.motor_rps = 0.0;
            return out;
        }
        let speed = (self.motor_rps / (7.0 * self.ratio)).clamp(0.0, 1.4);
        let engaged = self.env.clamp(0.0, 1.0);
        let strain = c.strain.clamp(0.0, 1.0);
        // the motor lugs against a compression stroke: everything it makes is louder then (and quicker to fall back:
        // the chug of a cranking engine is this swing, about 10 dB from the weakest part of the turn to the loudest)
        let effort = 0.4 + 0.9 * strain;

        // ---- armature: a pulse for every turn of the motor, each a little different, ringing the housing
        let mut arm = 0.0;
        if c.on || engaged > 0.2 {
            self.arm_phase += self.motor_rps * (1.0 + 0.02 * self.wander) * dt;
            if self.arm_phase >= 1.0 {
                self.arm_phase -= 1.0;
                self.arm_hit = 0.55 + 0.9 * (0.5 + 0.5 * n2);
                arm = self.arm_hit;
            }
        }
        let body = self.armature[0].step(arm) * 0.016 + self.armature[1].step(arm) * 0.023 + self.armature[2].step(arm) * 0.011;
        out += body * effort * effort * speed.sqrt() * engaged.max(0.3);

        // ---- ring gear: a tooth impact every time a tooth passes the pinion. Every tooth is a little different
        // (machining, wear, run-out, the angle it meets the pinion at), so both the spacing and the strength
        // scatter: the pulse train is a coarse rasp and not a comb of lines
        let mut hit = 0.0;
        if c.on || engaged > 0.2 {
            self.wander += (n2 - self.wander) * 0.002;
            self.tooth_phase += self.teeth * crank_rps * (1.0 + 0.5 * self.wander) * dt;
            if self.tooth_phase >= self.tooth_next {
                self.tooth_phase -= self.tooth_next;
                self.tooth_next = 1.0 + 0.45 * n;
                let strength = 0.35 + 0.65 * n3.abs();
                hit = 0.10 * strength * effort * engaged;
            }
        }
        out += self.mesh[0].step(hit) * 0.28 + self.mesh[1].step(hit) * 0.5 + self.mesh[2].step(hit) * 0.3;

        // ---- the teeth grinding together as the pinion is thrown in
        if self.grind > 0.001 {
            out += n2 * self.grind * 0.012;
            self.grind *= (-dt / 0.045).exp();
        }

        // ---- brush and fan hash: pink noise (equal energy per octave) without the rumble below ~160 Hz and rolled off
        // above ~2.4 kHz (two poles), under the buzz and not above it
        let w = n3;
        self.pink[0] = 0.99765 * self.pink[0] + w * 0.099_046;
        self.pink[1] = 0.963 * self.pink[1] + w * 0.296_516_4;
        self.pink[2] = 0.57 * self.pink[2] + w * 1.052_691_3;
        let pink = self.pink[0] + self.pink[1] + self.pink[2] + w * 0.1848;
        let k_hp = 1.0 - (-tau * 160.0 * dt).exp();
        self.hp1 += (pink - self.hp1) * k_hp;
        let a = pink - self.hp1;
        let k_lp = 1.0 - (-tau * 2400.0 * dt).exp();
        self.hash_lp += (a - self.hash_lp) * k_lp;
        self.air_lp += (self.hash_lp - self.air_lp) * k_lp;
        out += self.air_lp * speed.powf(1.2) * effort * effort * HASH_GAIN * engaged.max(0.3);

        // ---- the armature's own whine: weak, with the phase wandering so it never settles into a pure tone
        let wobble = 1.0 + 0.02 * n;
        self.whine_phase = (self.whine_phase + 6.0 * self.motor_rps * wobble * dt).fract();
        let whine = (self.whine_phase * tau).sin() + 0.5 * (self.whine_phase * 2.0 * tau).sin();
        self.tone_lp += (whine - self.tone_lp) * 0.5;
        out += self.tone_lp * speed * 0.0008;

        // the body of the car and the distance take the top off
        self.out_lp += (out - self.out_lp) * (1.0 - (-tau * 7_000.0 * dt).exp());
        self.out_lp
    }
}

/// Level of the brush and fan hash (pink noise through the filters above, Pa per unit).
const HASH_GAIN: f32 = 0.30;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acoustics::analysis::{peaks, spectrum};

    fn run(rpm: impl Fn(f32) -> f32, secs: f32) -> Vec<f32> {
        let fs = 44_100.0;
        let mut s = Starter::new(fs, 2.0);
        let n = (secs * fs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                s.step(&StarterCtx { on: t > 0.05, rpm: rpm(t), strain: 0.3 })
            })
            .collect()
    }

    /// Spectral flatness (geometric over arithmetic mean of the power, dB) between two bins: 0 dB for white noise,
    /// far below for a tone.
    fn flatness_db(psd: &[f64], lo: usize, hi: usize) -> f64 {
        let n = (hi - lo) as f64;
        let geo = psd[lo..hi].iter().map(|p| p.max(1e-30).ln()).sum::<f64>() / n;
        let ari = psd[lo..hi].iter().sum::<f64>() / n;
        10.0 * (geo.exp() / ari).log10()
    }

    /// Octave-band levels (dB re 1 Pa) of the starter alone cranking at `rpm` with `strain`.
    fn octave_levels(rpm: f32, strain: f32) -> Vec<(f64, f64)> {
        let fs = 44_100.0;
        let mut s = Starter::new(fs, 2.0);
        let n = (4.0 * fs) as usize;
        let x: Vec<f32> = (0..n).map(|i| s.step(&StarterCtx { on: i > 2000, rpm, strain })).collect();
        let psd = spectrum(&x[44_100..], 16384);
        let nfft = (psd.len() - 1) * 2;
        let df = 44_100.0 / nfft as f64;
        let mut out = Vec::new();
        let mut lo = 50.0f64;
        while lo < 12_000.0 {
            let ms: f64 = 2.0 / nfft as f64 * (1..psd.len()).filter(|k| (*k as f64 * df) >= lo && (*k as f64 * df) < lo * 2.0).map(|k| psd[k]).sum::<f64>();
            out.push((lo, 10.0 * (ms.max(1e-30) / (20e-6f64).powi(2)).log10()));
            lo *= 2.0;
        }
        out
    }

    #[test]
    fn the_starter_is_a_low_mid_whirr_and_not_hiss() {
        // The second version had flat octave bands from 800 Hz to 6 kHz (the shape of two recordings) and sounded like
        // gas rushing out: hiss. A starter's weight is in the 100 Hz - 2 kHz octaves; the top end is a rolled-off
        // fringe.
        for (rpm, strain) in [(230.0, 0.5), (140.0, 0.9), (350.0, 0.2)] {
            let l = octave_levels(rpm, strain);
            let at = |f: f64| l.iter().find(|(lo, _)| (*lo - f).abs() < 1.0).unwrap().1;
            let top = [100.0, 200.0, 400.0, 800.0, 1600.0].iter().map(|&f| at(f)).fold(f64::MIN, f64::max);
            println!("starter {rpm} rpm strain {strain}: {}", l.iter().map(|(f, v)| format!("{f:.0}:{:.1}", v - top)).collect::<Vec<_>>().join(" "));
            for f in [100.0, 200.0, 400.0, 800.0] {
                assert!(top - at(f) < 5.0, "{rpm} rpm: the {f} Hz octave is {:.1} dB under the loudest", top - at(f));
            }
            assert!(top - at(3200.0) > 6.0, "{rpm} rpm: 3.2-6.4 kHz octave only {:.1} dB under: hiss", top - at(3200.0));
            assert!(top - at(6400.0) > 14.0, "{rpm} rpm: 6.4-12.8 kHz octave only {:.1} dB under: hiss", top - at(6400.0));
        }
    }

    #[test]
    fn the_starter_chugs_with_the_compression_strokes() {
        // loud against a stroke, quiet between: the level swings by several dB with the strain
        let fs = 44_100.0;
        let mut s = Starter::new(fs, 2.0);
        let rms = |s: &mut Starter, strain: f32| {
            let n = 4410;
            let mut acc = 0.0;
            for _ in 0..n {
                let v = s.step(&StarterCtx { on: true, rpm: 230.0, strain });
                acc += v * v;
            }
            (acc / n as f32).sqrt()
        };
        let _ = rms(&mut s, 0.0);
        let (easy, hard) = (rms(&mut s, 0.0), rms(&mut s, 1.0));
        println!("starter rms between strokes {easy:.4}, against one {hard:.4}");
        assert!(hard > 2.0 * easy, "no chug: {easy:.4} vs {hard:.4}");
    }

    #[test]
    fn a_cranking_starter_is_a_rough_buzz_and_not_a_pure_tone() {
        // the crank surges and sags about 10 % with every compression stroke
        let wobble = |t: f32| 230.0 * (1.0 + 0.10 * (std::f32::consts::TAU * 7.0 * t).sin());
        let x = run(wobble, 3.0);
        assert!(x.iter().all(|v| v.is_finite()));
        let psd = spectrum(&x[22_050..], 16384);
        // The old starter was one sine and its second harmonic (a theremin): the band 300 Hz - 4 kHz of such a
        // signal is almost empty, spectral flatness -45 dB and below. Tooth impacts and brush hiss fill it.
        let flat = flatness_db(&psd, 112, 1490);
        let pk = peaks(&psd, 44_100.0, 200.0, 6000.0, 6.0);
        let strongest = pk.first().map(|p| p.1).unwrap_or(0.0);
        let near = pk.iter().filter(|p| p.1 > strongest * 1.0e-2).count();
        println!("starter: flatness 300 Hz - 4 kHz {flat:.1} dB, {near} lines within 20 dB of the strongest");
        assert!(flat > -22.0, "starter reads as a tone (flatness {flat:.1} dB)");
        assert!(near >= 6, "only {near} spectral lines: that is a whistle, not a gear train");
        // and it has real energy in the kHz range
        let hi: f64 = psd[93..700].iter().sum();
        let lo: f64 = psd[10..93].iter().sum();
        assert!(hi > 0.05 * lo, "starter is all rumble: hi {hi:.2e} lo {lo:.2e}");
    }

    #[test]
    fn the_old_starter_would_have_failed_the_tone_test() {
        // the test's own yardstick: a swept sine plus a second harmonic scores far below the threshold
        let fs = 44_100.0f32;
        let n = (3.0 * fs) as usize;
        let mut ph = 0.0f32;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                let rpm = 230.0 * (1.0 + 0.10 * (std::f32::consts::TAU * 7.0 * t).sin());
                ph = (ph + (380.0 + 2.2 * rpm) / fs).fract();
                ((ph * std::f32::consts::TAU).sin() + 0.4 * (ph * 2.0 * std::f32::consts::TAU).sin()) * 0.01
            })
            .collect();
        let psd = spectrum(&x[22_050..], 16384);
        let flat = flatness_db(&psd, 112, 1490);
        println!("sine starter: flatness {flat:.1} dB");
        assert!(flat < -30.0, "the yardstick cannot tell a sine from a starter ({flat:.1} dB)");
    }

    #[test]
    fn the_starter_follows_the_crank_and_goes_quiet_when_released() {
        let fs = 44_100.0;
        let mut s = Starter::new(fs, 2.0);
        let mut e = |on: bool, rpm: f32, secs: f32| -> f32 {
            let n = (secs * fs) as usize;
            let mut acc = 0.0f32;
            for _ in 0..n {
                let v = s.step(&StarterCtx { on, rpm, strain: 0.2 });
                acc += v * v;
            }
            (acc / n as f32).sqrt()
        };
        let _ = e(true, 230.0, 0.3);
        let running = e(true, 230.0, 0.5);
        let _ = e(false, 0.0, 1.5);
        let after = e(false, 0.0, 0.5);
        println!("starter rms: cranking {running:.4}, 1.5 s after release {after:.2e}");
        assert!(running > 0.002);
        assert!(after < running * 0.01, "the armature must have spun down");
    }
}
