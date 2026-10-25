//! The sounds that are not gas dynamics: starter motor, valve train, combustion knock transmitted through
//! the block, detonation ping, turbo compressor whistle and blow-off, gear whine.

use super::flow_noise::Shaper;
use super::starter::{Starter, StarterCtx};
use super::turbo::{Turbo, TurboCtx};
use crate::tuning::BlowOff;

/// Pressure (rms, Pa, at the listener) of the broadband engine noise at 1000 rpm and full load: calibrated against
/// recordings of idling and revving cars (`README`, Audio).
const ENGINE_NOISE_PA: f32 = 0.011;

/// Structural modes of the block that a combustion thud rings: (Hz, damping ratio, weight). A block has hundreds of
/// them, closely spaced and well damped, so what reaches the ear is a smooth, dull thud; three sharp modes (the first
/// version) made a nasal tone at 640 Hz that sat in every engine at every speed.
const BLOCK_MODES: [(f32, f32, f32); 6] = [(520.0, 0.09, 0.9), (740.0, 0.09, 1.0), (1050.0, 0.10, 0.8), (1500.0, 0.11, 0.6), (2100.0, 0.12, 0.45), (2900.0, 0.13, 0.3)];

/// Scale of the pressure-rise input of the block modes (calibrated so the rattle keeps the level it had at idle).
const BLOCK_RISE_GAIN: f32 = 7.5;

/// A diesel's pressure rise is quicker than a petrol engine's (corner of the smoothing of its pulse, Hz) and its clatter
/// is louder against the block modes (gain of the clatter bank, with the 7x impulse of `step`), and it leaves the modes
/// above 1.8 kHz mostly unexcited (their weight).
const DIESEL_RISE_HZ: f32 = 1_000.0;
const DIESEL_UPPER_MODES: f32 = 0.2;
const DIESEL_CLATTER_LP_HZ: f32 = 1_600.0;
const PETROL_RATTLE_LP_HZ: f32 = 3_200.0;
const DIESEL_CLATTER: f32 = 0.026;

/// Share of the engine noise that is left when nothing burns (valve train, bearings, gears, belts only).
const MOTORING: f32 = 0.10;

/// 0 below `a`, 1 above `b`, smooth between.
#[inline]
pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[derive(Clone, Debug)]
pub struct Noise(u32);

impl Noise {
    pub fn new(seed: u32) -> Self {
        Noise(seed | 1)
    }
    #[inline]
    pub fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Slowly wandering unit-variance noise (one-pole low-passed white noise, rescaled): the random drift of a tone's
/// frequency or the flutter of its level.
#[derive(Clone, Copy, Debug)]
pub struct Wander {
    y: f32,
    k: f32,
    g: f32,
}

impl Wander {
    pub fn new(hz: f32, fs: f32) -> Wander {
        let k = 1.0 - (-std::f32::consts::TAU * hz / fs).exp();
        // white noise in [-1, 1] has variance 1/3; a one-pole low-pass keeps k / (2 - k) of it
        Wander { y: 0.0, k, g: (3.0 * (2.0 - k) / k).sqrt() }
    }
    #[inline]
    pub fn step(&mut self, n: f32) -> f32 {
        self.y += (n - self.y) * self.k;
        self.y * self.g
    }
}

/// A two-pole resonator excited by impulses: one structural mode of the block or head.
#[derive(Clone, Copy, Debug, Default)]
pub struct Res {
    y1: f32,
    y2: f32,
    a1: f32,
    a2: f32,
}

impl Res {
    pub fn new(f: f32, zeta: f32, fs: f32) -> Res {
        let w = std::f32::consts::TAU * f / fs;
        let r = (-zeta * w).exp();
        Res { y1: 0.0, y2: 0.0, a1: 2.0 * r * w.cos(), a2: -r * r }
    }
    #[inline]
    pub fn step(&mut self, x: f32) -> f32 {
        let y = x + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Everything the mechanical layer needs to know each sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct MechCtx {
    pub rpm: f32,
    pub throttle: f32,
    pub load: f32,
    pub spool: f32,
    pub boost: f32,
    /// The starter is driving the engine: the pinion is in and the motor is turning.
    pub starter_on: bool,
    /// Instantaneous crank speed while cranking (with the surge and sag of the compression strokes), rpm.
    pub crank_rpm: f32,
    /// 0..1: how hard the starter works against a compression stroke right now.
    pub starter_strain: f32,
    pub combusting: bool,
    pub knock: f32,
    pub gear_whine: f32,
    pub vehicle_speed: f32,
    /// Combustion impulses this sample (sum over cylinders, 0..), valve events this sample (count).
    pub combustion: f32,
    pub valve_events: f32,
    pub diesel: bool,
    /// How hard the combustion hits the block relative to a petrol engine at idle (from the rate of pressure rise).
    pub rattle: f32,
    /// Engine speed over redline, where the engine is in its firing period (0..1), and the valve on the charge pipe.
    pub rpm_frac: f32,
    pub fire_phase: f32,
    pub blow_off: BlowOff,
    pub lift_off_pop: f32,
}

#[derive(Clone, Debug)]
pub struct Mechanical {
    fs: f32,
    rng: Noise,
    block: [Res; BLOCK_MODES.len()],
    /// Two poles that take the edge off the diesel's clatter.
    diesel_lp: [f32; 2],
    head: Res,
    ping: Res,
    starter: Starter,
    /// The combustion pressure rise is not a spike: it reaches the block as a pulse about a third of a
    /// millisecond long, which is what keeps the rattle a thud rather than a click.
    comb_lp: f32,
    comb_prev: f32,
    valve_lp: f32,
    turbo: Turbo,
    gear_phase: f32,
    gear_shaft: f32,
    knock_env: f32,
    tick_env: f32,
    tick_gain: f32,
    /// Broadband engine noise (combustion, bearings, gears, drive, injectors): stationary, as recordings of real engines show.
    engine_noise: Shaper,
    /// Scale on the broadband engine noise (many-cylinder engines, see `set_hash_scale`).
    noise_scale: f32,
    /// 1 while the engine fires, falling away when it stops.
    fire: f32,
    // the gear mesh is not perfectly steady: drift and flutter
    gear_fm: Wander,
    gear_am: Wander,
    rng2: Noise,
}

impl Mechanical {
    pub fn new(fs: f32, displacement_l: f32) -> Self {
        Mechanical {
            fs,
            rng: Noise::new(0x51ED_270B),
            block: BLOCK_MODES.map(|(f, z, _)| Res::new(f, z, fs)),
            diesel_lp: [0.0; 2],
            head: Res::new(2400.0, 0.06, fs),
            ping: Res::new(5800.0, 0.02, fs),
            starter: Starter::new(fs, displacement_l),
            comb_lp: 0.0,
            comb_prev: 0.0,
            valve_lp: 0.0,
            turbo: Turbo::new(fs),
            gear_phase: 0.0,
            gear_shaft: 0.0,
            knock_env: 0.0,
            tick_env: 0.0,
            tick_gain: 1.0,
            fire: 0.0,
            noise_scale: 1.0,
            engine_noise: {
                let mut s = Shaper::new(0x2F6B_91A7, fs, 450.0, 1_800.0, 4_500.0);
                s.normalise();
                s
            },
            gear_fm: Wander::new(3.0, fs),
            gear_am: Wander::new(9.0, fs),
            rng2: Noise::new(0x7A31_C5E3),
        }
    }

    /// Scale on the broadband engine noise.
    pub fn set_hash_scale(&mut self, amplitude_ratio: f32) {
        self.noise_scale = amplitude_ratio;
    }

    /// Shaft speed of the turbo, 0..1 (analysis).
    pub fn turbo_shaft(&self) -> f32 {
        self.turbo.shaft()
    }

    /// Takes over the noise generators of `other` (state included), so both produce the same noise from here on.
    pub fn copy_noise_from(&mut self, other: &Mechanical) {
        self.engine_noise = other.engine_noise.clone();
        self.rng = other.rng.clone();
        self.rng2 = other.rng2.clone();
        self.fire = other.fire;
        self.turbo.copy_noise_from(&other.turbo);
    }

    /// One sample of mechanical sound, in pascals at the listener.
    pub fn step(&mut self, c: &MechCtx) -> f32 {
        let dt = 1.0 / self.fs;
        let tau = std::f32::consts::TAU;
        let n = self.rng.next();

        // ---- combustion pressure rise rattles the block (diesel clatter is the same, much sharper)
        let strength = c.combustion * if c.diesel { 7.0 } else { 1.0 };
        let imp = strength * c.rattle;
        // the pulse is smoothed (a diesel's much less: its pressure rise is nearly instant, but not instant: the
        // first version fed three sharp modes with a step and made a regular 640 Hz buzz at the firing rate)
        let k_comb = 1.0 - (-tau * if c.diesel { DIESEL_RISE_HZ } else { 700.0 } * dt).exp();
        self.comb_lp += (imp - self.comb_lp) * k_comb;
        let mut s = 0.0;
        // the block answers the rate of rise of the pressure, not its level: no rumble below the first modes
        let rise = (self.comb_lp - self.comb_prev) * BLOCK_RISE_GAIN;
        self.comb_prev = self.comb_lp;
        for (r, m) in self.block.iter_mut().zip(BLOCK_MODES.iter()) {
            // (the clatter's body is the low modes: its upper ones, which a petrol engine's thud only touches, are
            // what made it a tick train hissing out to 5 kHz)
            let w = if c.diesel && m.0 > 1_800.0 { DIESEL_UPPER_MODES } else { 1.0 };
            s += r.step(rise * m.2 * w);
        }
        {
            // a two-pole resonator passes its input straight through above its own frequency, so the sharp rise
            // would click through unchanged (a diesel's clatter to 5 kHz; a V-twin's sparse thuds a tick to 10 kHz, which
            // read as high-frequency clicks): the rattle is the modes' ring, not that edge
            let hz = if c.diesel { DIESEL_CLATTER_LP_HZ } else { PETROL_RATTLE_LP_HZ };
            for lp in self.diesel_lp.iter_mut() {
                *lp += (s - *lp) * (1.0 - (-tau * hz * dt).exp());
                s = *lp;
            }
        }
        // a petrol engine's combustion thud stands out at idle, where nothing masks it, and is lost in the
        // exhaust and the wind of a hard drive: it is a little quieter at low revs
        let revs = 0.55 + 0.45 * (c.rpm / 3000.0).clamp(0.0, 1.0);
        let mut out = s * if c.diesel { DIESEL_CLATTER } else { 0.030 * revs };

        // ---- valve train: a soft tick per valve event through the head's resonance, every one a little
        // different (lash, the lifter, which valve), and rolled off - the ear picks out sparse sharp ticks
        // at idle, where they are not masked by anything
        if c.valve_events > 0.0 {
            self.tick_gain = 0.45 + 0.55 * (0.5 + 0.5 * n);
        }
        self.tick_env = self.tick_env.max(c.valve_events.min(1.0) * self.tick_gain * (0.2 + 0.8 * (c.rpm / 6000.0).min(1.0)));
        self.tick_env *= (-dt / 0.0008).exp();
        let tick = self.head.step(n * self.tick_env * 0.05);
        self.valve_lp += (tick - self.valve_lp) * (1.0 - (-tau * 2600.0 * dt).exp());
        out += self.valve_lp * 0.02;

        // ---- engine noise: combustion, bearings, gears, chain and belts, injectors and the rest add up to a broadband
        // noise (recordings: stationary, envelope rms/mean ~0.55, with only a few percent of modulation at the firing
        // rate) whose level climbs with engine speed (about rpm^1.1 in amplitude) and a little with load
        // Most of it is combustion: an engine that is only being turned (the key off in a car that rolls on, a
        // stalled engine on a hill, the starter before the fire) is some 20 dB quieter. The change follows the fire
        // with a short lag, so the hiss of a switched-off engine does not hang on while its crank runs down.
        let speed = (c.rpm / 1000.0).max(0.0);
        let k_fire = 1.0 - (-dt / if c.combusting { 0.02 } else { 0.10 }).exp();
        self.fire += (c.combusting as u8 as f32 - self.fire) * k_fire;
        // the noise of a very fast engine grows more slowly than rpm^1.1: a long steady pull in a high gear must
        // not hiss
        let hi_rpm = smoothstep(2_500.0, 5_500.0, c.rpm);
        let load = (0.7 + 0.3 * c.load.clamp(0.0, 1.2)) * (MOTORING + (1.0 - MOTORING) * self.fire) * (1.0 - 0.3 * hi_rpm);
        out += self.engine_noise.step(ENGINE_NOISE_PA * self.noise_scale * speed.powf(1.1) * load * if c.diesel { 1.6 } else { 1.0 });

        // ---- knock (detonation ping)
        if c.knock > 0.05 {
            self.knock_env = self.knock_env.max(0.25 * c.knock);
        }
        out += self.ping.step(n * self.knock_env) * 0.05;
        self.knock_env *= (-dt / 0.004).exp();

        // ---- starter motor, pinion and ring gear
        out += self.starter.step(&StarterCtx { on: c.starter_on, rpm: c.crank_rpm, strain: c.starter_strain });

        // ---- turbo: the compressor's whistle and rush, the blow-off valve or the flutter on a lift
        out += self.turbo.step(&TurboCtx {
            spool: c.spool,
            throttle: c.throttle,
            boost: c.boost,
            rpm_frac: c.rpm_frac,
            fire_phase: c.fire_phase,
            combusting: c.combusting,
            diesel: c.diesel,
            blow_off: c.blow_off,
        });

        // ---- gearbox whine
        if c.gear_whine > 0.01 {
            // the mesh tone wanders a little and its level is modulated at the shaft's turn (eccentricity): sidebands
            // either side of the line, as in a real gearbox, instead of one pure sine
            let f = (180.0 + 28.0 * c.vehicle_speed) * (1.0 + 0.004 * self.gear_fm.step(self.rng2.next()));
            self.gear_phase = (self.gear_phase + f * dt).fract();
            self.gear_shaft = (self.gear_shaft + f / 23.0 * dt).fract();
            let am = (1.0 + 0.35 * (self.gear_shaft * tau).sin() + 0.2 * self.gear_am.step(self.rng2.next())).max(0.2);
            out += ((self.gear_phase * tau).sin() + 0.35 * (self.gear_phase * 3.0 * tau).sin()) * am * c.gear_whine * 0.003;
        }
        out
    }
}
