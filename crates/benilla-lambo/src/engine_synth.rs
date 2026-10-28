//! Engine acoustic simulation bridge using `engine-core`.
//!
//! Replaces toy physical oscillators with a full 0-D thermodynamic gas dynamics solver:
//! tailpipe monopole radiation, pipe shell acoustic emission, exhaust jet noise,
//! mechanical machinery clatter, transmission gear whine, and overrun burble/backfire.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use kira::{
    info::Info,
    sound::{Sound, SoundData},
    Frame,
};
use serde::{Deserialize, Serialize};

use engine_core::config::engine_by_id;
use engine_core::sound::{default_banks, EngineSound, Quality, SoundInput, SAMPLE_RATE};
use engine_core::tuning::{AirFilter, Cat, Header, Muffler, Tuning};

/// Configuration loaded from `assets/lambo_v10.json`.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LamboConfig {
    #[serde(default = "default_engine_id")]
    pub engine_id: String,
    #[serde(default = "default_muffler")]
    pub muffler: String,
    #[serde(default = "default_quality")]
    pub quality: String,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default = "default_burble")]
    pub burble: f32,
}

fn default_engine_id() -> String {
    "v8".to_string()
}
fn default_muffler() -> String {
    "sport".to_string()
}
fn default_quality() -> String {
    "balanced".to_string()
}
fn default_volume() -> f32 {
    0.85
}
fn default_burble() -> f32 {
    0.85
}

impl Default for LamboConfig {
    fn default() -> Self {
        Self {
            engine_id: default_engine_id(),
            muffler: default_muffler(),
            quality: default_quality(),
            volume: default_volume(),
            burble: default_burble(),
        }
    }
}

/// Loads `assets/lambo_v10.json` or defaults to 6.2L V8 sport configuration.
pub fn load_lambo_config() -> LamboConfig {
    // A browser has no disk to look on: the shipped profile is compiled in.
    #[cfg(target_arch = "wasm32")]
    if let Ok(cfg) = serde_json::from_str(include_str!("../assets/lambo_v10.json")) {
        return cfg;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("assets").join("lambo_v10.json"),
        PathBuf::from("assets/lambo_v10.json"),
        PathBuf::from("crates/benilla-lambo/assets/lambo_v10.json"),
    ];

    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(cfg) = serde_json::from_str::<LamboConfig>(&content) {
                eprintln!(
                    "benilla-lambo audio: loaded engine-core config from {:?}",
                    path
                );
                return cfg;
            }
        }
    }

    eprintln!("benilla-lambo audio: using built-in default engine-core V8 profile");
    LamboConfig::default()
}

/// Kira `SoundData` for streaming `engine-core`'s physical thermodynamic sound simulation.
pub struct EngineSoundData {
    pub sample_rate: u32,
    pub config: LamboConfig,
}

impl EngineSoundData {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            config: load_lambo_config(),
        }
    }
}

/// Control handle for updating engine state from car physics in real time.
#[derive(Clone)]
pub struct EngineSoundHandle {
    rpm: Arc<AtomicU32>,
    throttle: Arc<AtomicU32>,
    volume: Arc<AtomicU32>,
    speed: Arc<AtomicU32>,
    gear: Arc<AtomicU32>,
    shift_pop: Arc<AtomicBool>,
    #[allow(dead_code)]
    stopped: Arc<AtomicBool>,
}

impl EngineSoundHandle {
    pub fn set_rpm(&self, rpm: f32) {
        self.rpm
            .store((rpm.max(100.0).round()) as u32, Ordering::Relaxed);
    }

    pub fn set_throttle(&self, throttle: f32) {
        self.throttle.store(
            ((throttle.clamp(0.0, 1.0)) * 1000.0).round() as u32,
            Ordering::Relaxed,
        );
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume.store(
            ((volume.max(0.0)) * 1000.0).round() as u32,
            Ordering::Relaxed,
        );
    }

    pub fn set_drivetrain(&self, speed: f32, gear: usize, is_shifting: bool) {
        self.speed
            .store((speed.max(0.0) * 100.0).round() as u32, Ordering::Relaxed);
        self.gear.store(gear.clamp(1, 7) as u32, Ordering::Relaxed);
        if is_shifting {
            self.shift_pop.store(true, Ordering::Relaxed);
        }
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

impl SoundData for EngineSoundData {
    type Error = std::convert::Infallible;
    type Handle = EngineSoundHandle;

    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        let rpm = Arc::new(AtomicU32::new(850));
        let throttle = Arc::new(AtomicU32::new(0));
        let volume = Arc::new(AtomicU32::new((self.config.volume * 1000.0) as u32));
        let speed = Arc::new(AtomicU32::new(0));
        let gear = Arc::new(AtomicU32::new(1));
        let shift_pop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));

        let handle = EngineSoundHandle {
            rpm: rpm.clone(),
            throttle: throttle.clone(),
            volume: volume.clone(),
            speed: speed.clone(),
            gear: gear.clone(),
            shift_pop: shift_pop.clone(),
            stopped: stopped.clone(),
        };

        let cfg = engine_by_id(&self.config.engine_id)
            .unwrap_or_else(|| engine_by_id("v8").expect("stock v8"));
        eprintln!(
            "benilla-lambo audio: running engine-core with {} ({} cylinders, {:.1}L)",
            cfg.display_name,
            cfg.cylinder_count,
            cfg.displacement_l()
        );

        let mut tuning = Tuning::default();
        tuning.header = Header::LongTube;
        tuning.cat = Cat::Delete;
        tuning.muffler = match self.config.muffler.to_lowercase().as_str() {
            "straight" => Muffler::Straight,
            "delete" => Muffler::Delete,
            "stock" => Muffler::Stock,
            _ => Muffler::Sport,
        };
        tuning.air_filter = AirFilter::ColdAir;

        let quality = match self.config.quality.to_lowercase().as_str() {
            "fast" => Quality::FAST,
            "ultra" => Quality::ULTRA,
            "high" => Quality::HIGH,
            _ => Quality::BALANCED,
        };

        let banks = default_banks(&cfg);
        let mut engine_sound = EngineSound::new(&cfg, &tuning, &banks, quality);
        engine_sound.full_scale_pa = 9.0;

        let sound = EngineSoundStream {
            engine_sound,
            sample_rate: self.sample_rate,
            rpm,
            throttle,
            volume,
            speed,
            gear,
            shift_pop,
            stopped,
            serial: 1,
            burble_setting: self.config.burble,
            resample_phase: 0.0,
            s0: 0.0,
            s1: 0.0,
        };

        Ok((Box::new(sound), handle))
    }
}

/// An actively streaming audio sound running `engine-core` on Kira's audio thread.
pub struct EngineSoundStream {
    engine_sound: EngineSound,
    sample_rate: u32,
    rpm: Arc<AtomicU32>,
    throttle: Arc<AtomicU32>,
    volume: Arc<AtomicU32>,
    speed: Arc<AtomicU32>,
    #[allow(dead_code)]
    gear: Arc<AtomicU32>,
    shift_pop: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    serial: u64,
    burble_setting: f32,
    resample_phase: f32,
    s0: f32,
    s1: f32,
}

impl Sound for EngineSoundStream {
    fn process(&mut self, out: &mut [Frame], _dt: f64, _info: &Info) {
        if self.stopped.load(Ordering::Relaxed) {
            for frame in out.iter_mut() {
                *frame = Frame::ZERO;
            }
            return;
        }

        let rpm = self.rpm.load(Ordering::Relaxed) as f32;
        let throttle = (self.throttle.load(Ordering::Relaxed) as f32) / 1000.0;
        let speed = (self.speed.load(Ordering::Relaxed) as f32) / 100.0;
        let master_vol = (self.volume.load(Ordering::Relaxed) as f32) / 1000.0;
        let is_shifting = self.shift_pop.swap(false, Ordering::Relaxed);
        self.serial += 1;

        let running = rpm > 50.0;
        let lit = rpm > 150.0;
        let load = (throttle * 1.15).clamp(0.04, 1.2);
        let map_pa = 32_000.0 + throttle * 69_000.0;
        let afr = 14.7 - throttle * 2.2;
        let egt_k = 850.0 + throttle * 400.0;

        let input = SoundInput {
            rpm,
            throttle,
            load,
            map_pa,
            atm_pa: 101_325.0,
            air_k: 293.0,
            charge_k: 300.0,
            afr,
            egt_k,
            boost: 0.0,
            spool: 0.0,
            running,
            lit,
            cranking: false,
            knock: 0.0,
            vehicle_speed: speed,
            gear_whine: (speed / 35.0).clamp(0.0, 1.0),
            burn: vec![1.0; self.engine_sound.cfg.cylinder_count],
            master: master_vol,
            serial: self.serial,
            turbo_speed: 0.0,
            burble: self.burble_setting,
            backfire: if is_shifting { 1.0 } else { 0.0 },
        };
        self.engine_sound.set_input(&input);

        // Resample from engine-core (44,100 Hz) to Kira's sample rate (e.g. 48,000 Hz)
        let fs_ratio = SAMPLE_RATE as f32 / self.sample_rate as f32;
        for frame in out.iter_mut() {
            while self.resample_phase >= 1.0 {
                self.s0 = self.s1;
                self.s1 = self.engine_sound.next_sample();
                self.resample_phase -= 1.0;
            }
            let sample = self.s0 + (self.s1 - self.s0) * self.resample_phase;
            self.resample_phase += fs_ratio;

            let clipped = (sample * 1.2).tanh() * 0.95;
            *frame = Frame::from_mono(clipped);
        }
    }

    fn finished(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_core_creation() {
        let sound_data = EngineSoundData::new(48000);
        let (mut sound, handle) = sound_data.into_sound().expect("into_sound");

        handle.set_rpm(4500.0);
        handle.set_throttle(0.8);
        handle.set_drivetrain(25.0, 2, false);

        let mut frames = vec![Frame::ZERO; 512];
        let dt = 1.0 / 48000.0;
        let info = kira::info::MockInfoBuilder::new().build();

        sound.process(&mut frames, dt, &info);

        let mut has_nonzero = false;
        for frame in &frames {
            assert!(!frame.left.is_nan());
            assert!(!frame.right.is_nan());
            if frame.left.abs() > 0.0001 {
                has_nonzero = true;
            }
        }
        assert!(has_nonzero, "engine-core should produce audio samples");
        assert!(!sound.finished());

        handle.stop();
        assert!(sound.finished());
    }
}
