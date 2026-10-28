//! The turbocharger's sound: the compressor wheel's whistle that climbs as the shaft spools up and hangs on as it coasts
//! down, the rush of the air it moves, and what happens to the charge pressure when the throttle snaps shut: a soft hiss
//! from a recirculating diverter valve, the loud "pssh" of a vented blow-off valve, or, with no valve at all, the
//! compressor surging against the closed throttle: the "fut-fut-fut" flutter.
//!
//! What the recordings show (the rally Celicas and the Renault 5 Turbo in `reference/batch2`): the flutter is a train of
//! chirps, each a tone sweeping up by a factor of two or three and down again over about a tenth of a second, at 3 to 12
//! a second, dying as the pressure in the charge pipe leaks away; the whistle is a narrow tone that is audible only under
//! boost.
//!
//! The shaft is a heavy little rotor: it takes a few tenths of a second to spool up under the exhaust gas (the engine
//! model's own spool lag comes on top) and a second or two to run down, because it keeps its momentum on a lift. The
//! whistle's pitch follows that shaft speed, not the throttle, so on a lift it glides down slowly instead of vanishing.

use super::mechanical::{Noise, Res, Wander};
use crate::tuning::BlowOff;

/// What the turbo needs to know each sample.
#[derive(Clone, Copy, Debug)]
pub struct TurboCtx {
    /// How far the engine model's turbo has spooled, 0..1.
    pub spool: f32,
    pub throttle: f32,
    /// Charge pressure above ambient, bar (smoothed).
    pub boost: f32,
    /// Engine speed over redline.
    pub rpm_frac: f32,
    /// Where in its firing period the engine is, 0..1 (the compressor's flow pulses at that rate).
    pub fire_phase: f32,
    pub combusting: bool,
    pub diesel: bool,
    pub blow_off: BlowOff,
}

#[derive(Clone, Debug)]
pub struct Turbo {
    fs: f32,
    rng: Noise,
    rng2: Noise,
    /// Shaft speed, 0..1.
    ns: f32,
    on: f32,
    thr: f32,
    thr_slow: f32,
    /// The charge pipe's pressure (bar): it follows the boost up and leaks away slowly.
    hold: f32,
    phase1: f32,
    phase2: f32,
    fm: Wander,
    am: Wander,
    // the rush of air and the turbine's rumble
    hp: f32,
    lp1: f32,
    lp2: f32,
    rumble: f32,
    // blow-off valve
    armed: bool,
    bov_t: f32,
    bov_amp: f32,
    bov_decay: f32,
    bov_hp: f32,
    bov_lp: f32,
    bov_phase: f32,
    // flutter
    flutter_on: bool,
    next_pulse: f32,
    pulse_t: f32,
    pulse_amp: f32,
    pulse_f0: f32,
    pulse_phase: f32,
    pulse_hp: f32,
    pulse_lp: f32,
    thump: Res,
}

impl Turbo {
    pub fn new(fs: f32) -> Turbo {
        Turbo {
            fs,
            rng: Noise::new(0x3C6E_F372),
            rng2: Noise::new(0x7A31_C5E3),
            ns: 0.0,
            on: 0.0,
            thr: 0.0,
            thr_slow: 0.0,
            hold: 0.0,
            phase1: 0.0,
            phase2: 0.0,
            fm: Wander::new(5.0, fs),
            am: Wander::new(14.0, fs),
            hp: 0.0,
            lp1: 0.0,
            lp2: 0.0,
            rumble: 0.0,
            armed: true,
            bov_t: -1.0,
            bov_amp: 0.0,
            bov_decay: 0.17,
            bov_hp: 0.0,
            bov_lp: 0.0,
            bov_phase: 0.0,
            flutter_on: false,
            next_pulse: 0.0,
            pulse_t: -1.0,
            pulse_amp: 0.0,
            pulse_f0: 800.0,
            pulse_phase: 0.0,
            pulse_hp: 0.0,
            pulse_lp: 0.0,
            thump: Res::new(85.0, 0.12, fs),
        }
    }

    /// Takes over the noise generators of `other`, so both produce the same noise from here on.
    pub fn copy_noise_from(&mut self, other: &Turbo) {
        self.rng = other.rng.clone();
        self.rng2 = other.rng2.clone();
        self.ns = other.ns;
        self.hold = other.hold;
        self.thr = other.thr;
        self.thr_slow = other.thr_slow;
    }

    /// Whistle frequency at shaft speed `ns`, Hz.
    pub fn whistle_hz(ns: f32) -> f32 {
        900.0 + 5800.0 * ns.clamp(0.0, 1.0).powf(1.15)
    }

    /// Shaft speed now, 0..1 (analysis).
    pub fn shaft(&self) -> f32 {
        self.ns
    }

    /// One sample, in pascals at the listener.
    pub fn step(&mut self, c: &TurboCtx) -> f32 {
        let dt = 1.0 / self.fs;
        let tau = std::f32::consts::TAU;
        let n = self.rng.next();
        let n2 = self.rng2.next();

        // ---- the shaft: spools up in a few tenths of a second, runs down over a second or two. With the throttle shut the
        // exhaust gas still turns it at a fraction of its speed under load.
        // (the shaft speed is the spool the engine model reports and what is left of it as it runs down: an engine without a turbo
        // reports none, and there is nothing here to whistle. A floor of "coasting" speed made every naturally aspirated engine whistle.)
        let target = if c.combusting {
            c.spool.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let t_shaft = if target > self.ns { 0.10 } else { 1.6 };
        self.ns += (target - self.ns) * (1.0 - (-dt / t_shaft).exp());
        self.on += ((c.combusting as u8 as f32) - self.on) * (1.0 - (-dt / 0.05).exp());
        self.thr += (c.throttle - self.thr) * (1.0 - (-dt / 0.08).exp());
        self.thr_slow += (self.thr - self.thr_slow) * (1.0 - (-dt / 0.4).exp());
        // the charge pipe: with a valve it empties at once on a lift, without one it is trapped and bleeds off slowly
        let leak = if c.blow_off == BlowOff::None {
            1.1
        } else {
            0.5
        };
        self.hold = (self.hold * (-dt / leak).exp()).max(c.boost);

        // how hard the compressor works: wide open and on boost
        let air =
            (0.25 + 0.75 * self.thr_slow.min(1.0)) * (0.3 + 0.7 * (c.boost / 1.2).clamp(0.0, 1.0));
        let sp = self.ns;
        let mut out = 0.0;
        let mut kick = 0.0;

        // ---- a lift with boost on: the blow-off valve opens, or the compressor surges
        if c.throttle < 0.4
            && self.armed
            && self.thr_slow > 0.55
            && self.hold > 0.3
            && c.spool > 0.2
            && !c.diesel
        {
            self.armed = false;
            match c.blow_off {
                BlowOff::Recirculating => {
                    self.bov_t = 0.0;
                    self.bov_amp = self.hold.clamp(0.0, 1.2) * 0.2;
                    self.bov_decay = 0.07;
                }
                BlowOff::Vented => {
                    self.bov_t = 0.0;
                    self.bov_amp = self.hold.clamp(0.0, 1.2);
                    self.bov_decay = 0.17;
                }
                BlowOff::None => {
                    self.flutter_on = true;
                    self.next_pulse = 0.0;
                }
            }
        }
        if c.throttle > 0.25 {
            self.armed = true;
            self.flutter_on = false;
        }

        // ---- the whistle of the compressor wheel, with the roughness of the pulsing flow it passes
        let mut flutter_am = 1.0;
        if self.pulse_t >= 0.0 {
            let x = (self.pulse_t / PULSE_S).min(1.0);
            flutter_am = 1.0 - 0.75 * (std::f32::consts::PI * x).sin().powi(2);
        }
        if sp > 0.04 && self.on > 0.01 {
            let _ = c.rpm_frac;
            let f1 = Self::whistle_hz(sp) * (1.0 + 0.010 * self.fm.step(n2));
            let flutter = (1.0 + 0.3 * self.am.step(self.rng2.next())).max(0.25);
            self.phase1 = (self.phase1 + f1 * dt).fract();
            self.phase2 = (self.phase2 + f1 * 0.617 * dt).fract();
            let tone = (self.phase1 * tau).sin()
                + 0.4 * (self.phase2 * tau).sin()
                + 0.2 * (self.phase1 * 2.0 * tau).sin();
            let pulse = 1.0 + 0.22 * (c.fire_phase * tau).sin();
            out += tone * flutter * pulse * flutter_am * 0.18 * sp.powf(1.5) * air * self.on;
            // the air the wheel moves: a rush from 1.2 to 6 kHz, and the turbine's rumble under it
            self.hp += (n - self.hp) * (1.0 - (-tau * 1200.0 * dt).exp());
            let x = n - self.hp;
            self.lp1 += (x - self.lp1) * (1.0 - (-tau * 6000.0 * dt).exp());
            self.lp2 += (self.lp1 - self.lp2) * (1.0 - (-tau * 6000.0 * dt).exp());
            out += self.lp2 * 0.08 * sp.powf(1.8) * air.powf(1.2) * self.on;
            self.rumble += (n - self.rumble) * (0.04 + 0.08 * sp);
            out += self.rumble * 0.008 * sp * (0.3 + c.boost.min(1.2)) * self.on;
        }

        // ---- blow-off: a jet of the charge pipe's air through a valve, loudest at the opening
        if self.bov_t >= 0.0 {
            let t = self.bov_t;
            let env = (1.0 - (-t / 0.006).exp()) * (-t / self.bov_decay).exp();
            self.bov_hp += (n2 - self.bov_hp) * (1.0 - (-tau * 1500.0 * dt).exp());
            let x = n2 - self.bov_hp;
            self.bov_lp += (x - self.bov_lp) * (1.0 - (-tau * 6500.0 * dt).exp());
            // the jet's own whistle drops as the pressure behind it falls
            self.bov_phase = (self.bov_phase + (1800.0 + 2200.0 * env) * dt).fract();
            out += (self.bov_lp * 10.0 + (self.bov_phase * tau).sin() * 1.0) * env * self.bov_amp;
            self.bov_t += dt;
            if env < 1.0e-3 && t > 0.05 {
                self.bov_t = -1.0;
            }
        }

        // ---- compressor surge: a train of chirps while the pipe still holds pressure
        if self.flutter_on {
            if self.hold < 0.1 || sp < 0.15 {
                self.flutter_on = false;
            } else {
                self.next_pulse -= dt;
                if self.next_pulse <= 0.0 && self.pulse_t < 0.0 {
                    let rate = 3.0 + 6.0 * self.hold.clamp(0.0, 1.2);
                    let j = 0.5 + 0.5 * n2;
                    self.next_pulse = (1.0 / rate) * (0.8 + 0.4 * j);
                    self.pulse_t = 0.0;
                    self.pulse_amp = self.hold.clamp(0.0, 1.2) * (0.6 + 0.8 * (0.5 + 0.5 * n));
                    self.pulse_f0 = (0.45 * Self::whistle_hz(sp)).clamp(450.0, 1100.0);
                    self.pulse_phase = 0.0;
                    // the thump of the wave hitting the closed throttle
                    kick = self.pulse_amp * 24.0;
                }
            }
        }
        if self.pulse_t >= 0.0 {
            let x = self.pulse_t / PULSE_S;
            if x >= 1.0 {
                self.pulse_t = -1.0;
            } else {
                let env = (std::f32::consts::PI * x).sin().powi(2);
                let f = self.pulse_f0 * (1.0 + 1.5 * (std::f32::consts::PI * x).sin());
                self.pulse_phase = (self.pulse_phase + f * dt).fract();
                let tone =
                    (self.pulse_phase * tau).sin() + 0.3 * (self.pulse_phase * 2.0 * tau).sin();
                self.pulse_hp += (n - self.pulse_hp) * (1.0 - (-tau * 600.0 * dt).exp());
                let x2 = n - self.pulse_hp;
                self.pulse_lp += (x2 - self.pulse_lp) * (1.0 - (-tau * 4000.0 * dt).exp());
                out += self.pulse_amp * env * (tone * 0.9 + self.pulse_lp * 1.8);
                self.pulse_t += dt;
            }
        }
        out += self.thump.step(kick) * 0.0006;
        out
    }
}

/// Length of one surge chirp, s.
const PULSE_S: f32 = 0.11;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(spool: f32, throttle: f32, boost: f32, blow_off: BlowOff) -> TurboCtx {
        TurboCtx {
            spool,
            throttle,
            boost,
            rpm_frac: 0.5,
            fire_phase: 0.0,
            combusting: true,
            diesel: false,
            blow_off,
        }
    }

    fn run(t: &mut Turbo, c: TurboCtx, secs: f32) -> Vec<f32> {
        (0..(secs * 44_100.0) as usize)
            .map(|_| t.step(&c))
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn the_shaft_spools_up_fast_and_runs_down_slowly() {
        let mut t = Turbo::new(44_100.0);
        run(&mut t, ctx(1.0, 1.0, 1.0, BlowOff::Recirculating), 0.15);
        let early = t.shaft();
        run(&mut t, ctx(1.0, 1.0, 1.0, BlowOff::Recirculating), 1.0);
        let full = t.shaft();
        // lift: the engine model's spool falls away fast, the shaft must not
        run(&mut t, ctx(0.0, 0.0, 0.0, BlowOff::Recirculating), 0.5);
        let half_s = t.shaft();
        run(&mut t, ctx(0.0, 0.0, 0.0, BlowOff::Recirculating), 3.0);
        let late = t.shaft();
        println!("shaft: {early:.2} after 0.15 s, {full:.2} spooled, {half_s:.2} 0.5 s after the lift, {late:.2} 3.5 s after");
        assert!(early > 0.5 * full, "spool-up too slow");
        assert!(
            half_s > 0.65 * full,
            "the shaft must coast through the first half second of a lift"
        );
        assert!(late < 0.35 * full, "and run down eventually");
    }

    #[test]
    fn the_whistle_follows_the_shaft_and_is_there_only_under_boost() {
        let mut t = Turbo::new(44_100.0);
        let on = run(&mut t, ctx(1.0, 1.0, 1.2, BlowOff::Recirculating), 2.0);
        let mut off = Turbo::new(44_100.0);
        let idle = run(&mut off, ctx(0.0, 0.0, 0.0, BlowOff::Recirculating), 2.0);
        println!(
            "rms on boost {:.4}, off {:.5}",
            rms(&on[44_100..]),
            rms(&idle[44_100..])
        );
        assert!(rms(&on[44_100..]) > 10.0 * rms(&idle[44_100..]));
        assert!(
            Turbo::whistle_hz(1.0) > Turbo::whistle_hz(0.5)
                && Turbo::whistle_hz(0.5) > Turbo::whistle_hz(0.1)
        );
    }

    #[test]
    fn an_engine_without_a_turbo_never_whistles() {
        // spool 0 and boost 0 at every throttle and speed: silence (a floor of coasting shaft speed once made every NA engine whistle)
        let mut t = Turbo::new(44_100.0);
        for (thr, rpm) in [(0.0, 0.15), (0.5, 0.4), (1.0, 0.9)] {
            let c = TurboCtx {
                rpm_frac: rpm,
                ..ctx(0.0, thr, 0.0, BlowOff::Recirculating)
            };
            let x: Vec<f32> = (0..88_200).map(|_| t.step(&c)).collect();
            assert!(
                rms(&x[44_100..]) < 1.0e-4,
                "throttle {thr}: a naturally aspirated engine whistles ({:.5})",
                rms(&x[44_100..])
            );
        }
    }

    #[test]
    fn a_vented_valve_is_louder_than_a_diverter_and_flutter_has_a_rhythm() {
        let burst = |bo: BlowOff| {
            let mut t = Turbo::new(44_100.0);
            run(&mut t, ctx(1.0, 1.0, 1.0, bo), 1.5);
            run(&mut t, ctx(0.8, 0.0, 0.8, bo), 1.5)
        };
        let (vent, div, none) = (
            burst(BlowOff::Vented),
            burst(BlowOff::Recirculating),
            burst(BlowOff::None),
        );
        let peak = |x: &[f32]| x.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        println!(
            "peaks after the lift: vent {:.3}, diverter {:.3}, none {:.3}",
            peak(&vent),
            peak(&div),
            peak(&none)
        );
        assert!(peak(&vent) > 1.8 * peak(&div));
        // the flutter: the envelope of the 1.5 s after a lift has several separate bursts
        let env: Vec<f32> = none.chunks(441).map(|c| rms(c)).collect();
        let hi = env.iter().cloned().fold(0.0f32, f32::max);
        let mut bursts = 0;
        let mut was = false;
        for &e in &env {
            let up = e > 0.35 * hi;
            if up && !was {
                bursts += 1;
            }
            was = up;
        }
        println!("flutter bursts in 1.5 s: {bursts}");
        assert!(bursts >= 4, "only {bursts} flutter bursts");
        let div_env: Vec<f32> = div.chunks(441).map(|c| rms(c)).collect();
        let hd = div_env.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            div_env
                .iter()
                .skip(40)
                .all(|&e| e < 0.3 * hd.max(1e-9) || e < 0.01),
            "a diverter valve must not flutter"
        );
    }
}
