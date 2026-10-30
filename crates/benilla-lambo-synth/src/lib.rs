//! The Lamborghini's engine sound: `engine-core`'s thermodynamic synth behind one render call.
//!
//! The pipe-flow solver costs a third of a core, so a browser cannot run it on the game's thread:
//! there the render call runs in a worker (`web` below) and the game reads finished samples.
//! Natively kira's audio thread calls it directly.

use engine_core::config::engine_by_id;
use engine_core::sound::{default_banks, EngineSound, Quality, SoundInput, SAMPLE_RATE};
use engine_core::tuning::{AirFilter, Cat, Header, Muffler, Tuning};

#[cfg(target_arch = "wasm32")]
mod web;

/// What the game tells the synth each time it asks for samples.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub rpm: f32,
    pub throttle: f32,
    /// Master volume, 0 to 1.
    pub volume: f32,
    /// Vehicle speed, m/s.
    pub speed: f32,
    /// True once for a gear change: a backfire pop.
    pub shifting: bool,
}

/// The engine, its tuning and the resampler to the output device's rate.
pub struct Synth {
    sound: EngineSound,
    sample_rate: u32,
    burble: f32,
    serial: u64,
    resample_phase: f32,
    s0: f32,
    s1: f32,
}

/// The quality tier a config names; an unknown name is `balanced`.
fn quality_named(name: &str) -> Quality {
    match name.to_lowercase().as_str() {
        "fast" => Quality::FAST,
        "eco" => Quality::ECO,
        "ultra" => Quality::ULTRA,
        "high" => Quality::HIGH,
        _ => Quality::BALANCED,
    }
}

impl Synth {
    /// `engine_id`, `muffler` and `quality` are the names in `lambo_v10.json`; `burble` is its
    /// overrun burble, 0 to 1.
    pub fn new(
        engine_id: &str,
        muffler: &str,
        quality: &str,
        burble: f32,
        sample_rate: u32,
    ) -> Self {
        let cfg = engine_by_id(engine_id).unwrap_or_else(|| engine_by_id("v8").expect("stock v8"));
        let tuning = Tuning {
            header: Header::LongTube,
            cat: Cat::Delete,
            muffler: match muffler.to_lowercase().as_str() {
                "straight" => Muffler::Straight,
                "delete" => Muffler::Delete,
                "stock" => Muffler::Stock,
                _ => Muffler::Sport,
            },
            air_filter: AirFilter::ColdAir,
            ..Tuning::default()
        };
        let banks = default_banks(&cfg);
        let mut sound = EngineSound::new(&cfg, &tuning, &banks, quality_named(quality));
        sound.full_scale_pa = 9.0;
        Self {
            sound,
            sample_rate,
            burble,
            serial: 1,
            resample_phase: 0.0,
            s0: 0.0,
            s1: 0.0,
        }
    }

    /// The output device's rate, which can differ from the one the synth started with.
    pub fn set_sample_rate(&mut self, rate: u32) {
        self.sample_rate = rate.max(8000);
    }

    /// Cylinder count, for logging.
    pub fn cylinders(&self) -> usize {
        self.sound.cfg.cylinder_count
    }

    /// Fill `out` with mono samples at the output rate.
    pub fn render(&mut self, p: &Params, out: &mut [f32]) {
        self.serial += 1;
        let throttle = p.throttle;
        let input = SoundInput {
            rpm: p.rpm,
            throttle,
            load: (throttle * 1.15).clamp(0.04, 1.2),
            map_pa: 32_000.0 + throttle * 69_000.0,
            atm_pa: 101_325.0,
            air_k: 293.0,
            charge_k: 300.0,
            afr: 14.7 - throttle * 2.2,
            egt_k: 850.0 + throttle * 400.0,
            boost: 0.0,
            spool: 0.0,
            running: p.rpm > 50.0,
            lit: p.rpm > 150.0,
            cranking: false,
            knock: 0.0,
            vehicle_speed: p.speed,
            gear_whine: (p.speed / 35.0).clamp(0.0, 1.0),
            burn: vec![1.0; self.sound.cfg.cylinder_count],
            master: p.volume,
            serial: self.serial,
            turbo_speed: 0.0,
            burble: self.burble,
            backfire: if p.shifting { 1.0 } else { 0.0 },
        };
        self.sound.set_input(&input);

        // engine-core runs at 44.1 kHz; the device takes 48 kHz or whatever it has.
        let fs_ratio = SAMPLE_RATE as f32 / self.sample_rate as f32;
        for sample in out.iter_mut() {
            while self.resample_phase >= 1.0 {
                self.s0 = self.s1;
                self.s1 = self.sound.next_sample();
                self.resample_phase -= 1.0;
            }
            let s = self.s0 + (self.s1 - self.s0) * self.resample_phase;
            self.resample_phase += fs_ratio;
            *sample = (s * 1.2).tanh() * 0.95;
        }
    }
}
