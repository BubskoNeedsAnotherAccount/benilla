//! Engine acoustic simulation bridge using `engine-core`.
//!
//! Replaces toy physical oscillators with a full 0-D thermodynamic gas dynamics solver:
//! tailpipe monopole radiation, pipe shell acoustic emission, exhaust jet noise,
//! mechanical machinery clatter, transmission gear whine, and overrun burble/backfire.
//!
//! The synth itself is `benilla-lambo-synth`. Natively kira's audio thread renders it. In a
//! browser the audio callback shares the game's thread, which the solver would take most of, so a
//! worker renders it and the callback only copies finished samples (`web.rs`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use benilla_lambo_synth::Params;
#[cfg(not(target_arch = "wasm32"))]
use benilla_lambo_synth::Synth;
use kira::{
    info::Info,
    sound::{Sound, SoundData},
    Frame,
};
use serde::{Deserialize, Serialize};

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

    /// The state the game last set, as the synth takes it; a pending gear-shift pop is consumed.
    fn params(&self) -> Params {
        Params {
            rpm: self.rpm.load(Ordering::Relaxed) as f32,
            throttle: self.throttle.load(Ordering::Relaxed) as f32 / 1000.0,
            volume: self.volume.load(Ordering::Relaxed) as f32 / 1000.0,
            speed: self.speed.load(Ordering::Relaxed) as f32 / 100.0,
            shifting: self.shift_pop.swap(false, Ordering::Relaxed),
        }
    }
}

impl SoundData for EngineSoundData {
    type Error = std::convert::Infallible;
    type Handle = EngineSoundHandle;

    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        let handle = EngineSoundHandle {
            rpm: Arc::new(AtomicU32::new(850)),
            throttle: Arc::new(AtomicU32::new(0)),
            volume: Arc::new(AtomicU32::new((self.config.volume * 1000.0) as u32)),
            speed: Arc::new(AtomicU32::new(0)),
            gear: Arc::new(AtomicU32::new(1)),
            shift_pop: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
        };

        #[cfg(not(target_arch = "wasm32"))]
        let sound = {
            let synth = Synth::new(
                &self.config.engine_id,
                &self.config.muffler,
                &self.config.quality,
                self.config.burble,
                self.sample_rate,
            );
            eprintln!(
                "benilla-lambo audio: running engine-core with {} ({} cylinders)",
                self.config.engine_id,
                synth.cylinders()
            );
            EngineSoundStream {
                synth,
                ctl: handle.clone(),
                scratch: Vec::new(),
            }
        };
        // The worker makes its own synth from the page's copy of the config.
        #[cfg(target_arch = "wasm32")]
        let sound = EngineSoundStream {
            ctl: handle.clone(),
            scratch: Vec::new(),
        };

        Ok((Box::new(sound), handle))
    }
}

/// An actively streaming audio sound running `engine-core` on Kira's audio thread.
pub struct EngineSoundStream {
    #[cfg(not(target_arch = "wasm32"))]
    synth: Synth,
    ctl: EngineSoundHandle,
    scratch: Vec<f32>,
}

impl Sound for EngineSoundStream {
    fn process(&mut self, out: &mut [Frame], _dt: f64, _info: &Info) {
        if self.ctl.stopped.load(Ordering::Relaxed) {
            out.fill(Frame::ZERO);
            return;
        }
        let params = self.ctl.params();
        self.scratch.resize(out.len(), 0.0);

        #[cfg(not(target_arch = "wasm32"))]
        self.synth.render(&params, &mut self.scratch);
        // The output device's rate is the frame spacing kira reports.
        #[cfg(target_arch = "wasm32")]
        let got = crate::web::pull_synth(&params, (1.0 / _dt).round() as u32, &mut self.scratch);
        #[cfg(target_arch = "wasm32")]
        self.scratch[got..].fill(0.0);

        for (frame, sample) in out.iter_mut().zip(&self.scratch) {
            *frame = Frame::from_mono(*sample);
        }
    }

    fn finished(&self) -> bool {
        self.ctl.stopped.load(Ordering::Relaxed)
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
