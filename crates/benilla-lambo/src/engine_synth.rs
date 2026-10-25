//! Procedural engine sound synthesizer based on physical acoustic waveguide simulation.
//!
//! Inspired by and adapted from `DasEtwas/enginesound`.
//! Simulates four-stroke combustion, piston motion, intake and exhaust acoustic
//! waveguides, and muffler chambers in real-time.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use kira::{
    info::Info,
    sound::{Sound, SoundData},
    Frame,
};
use serde::{Deserialize, Serialize};

pub const PI2F: f32 = 2.0 * std::f32::consts::PI;
pub const PI4F: f32 = 4.0 * std::f32::consts::PI;
pub const WAVEGUIDE_MAX_AMP: f32 = 24.0;

/// Fast pseudo-random noise generator using 32-bit XorShift.
#[derive(Clone, Debug)]
pub struct Noise {
    state: u32,
}

impl Default for Noise {
    fn default() -> Self {
        Self::new(0x85412963)
    }
}

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x85412963 } else { seed },
        }
    }

    pub fn step(&mut self) -> f32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        (self.state as f32 / (u32::MAX as f32 * 0.5)) - 1.0
    }
}

/// Circular sample buffer for acoustic delay lines.
#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct LoopBuffer {
    pub delay: f32,
    #[serde(skip)]
    pub data: Vec<f32>,
    #[serde(skip)]
    pub pos: usize,
}

impl LoopBuffer {
    pub fn new(len: usize, samples_per_second: u32) -> Self {
        let len = len.max(1);
        Self {
            delay: len as f32 / samples_per_second as f32,
            data: vec![0.0; len],
            pos: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f32) {
        let len = self.data.len();
        self.data[self.pos % len] = value;
    }

    #[inline]
    pub fn pop(&mut self) -> f32 {
        let len = self.data.len();
        self.data[(self.pos + 1) % len]
    }

    #[inline]
    pub fn advance(&mut self) {
        self.pos += 1;
    }
}

/// First-order low-pass filter.
#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct LowPassFilter {
    pub delay: f32,
    #[serde(skip)]
    pub alpha: f32,
    #[serde(skip)]
    pub last: f32,
}

impl LowPassFilter {
    pub fn new(freq: f32, samples_per_second: u32) -> Self {
        let freq = freq.max(1.0);
        let dt = 1.0 / samples_per_second as f32;
        let alpha = (PI2F * dt * freq) / (PI2F * dt * freq + 1.0);
        Self {
            delay: 1.0 / freq,
            alpha,
            last: 0.0,
        }
    }

    #[inline]
    pub fn filter(&mut self, sample: f32) -> f32 {
        let ret = (sample - self.last).mul_add(self.alpha, self.last);
        self.last = ret;
        ret
    }
}

/// Acoustic delay line wrapping a circular buffer.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct DelayLine {
    pub samples: LoopBuffer,
}

impl DelayLine {
    pub fn new(delay: usize, samples_per_second: u32) -> Self {
        Self {
            samples: LoopBuffer::new(delay, samples_per_second),
        }
    }

    #[inline]
    pub fn pop(&mut self) -> f32 {
        self.samples.pop()
    }

    #[inline]
    pub fn push(&mut self, sample: f32) {
        self.samples.push(sample);
    }
}

/// Bidirectional acoustic waveguide representing a resonant pipe.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct WaveGuide {
    pub chamber0: DelayLine,
    pub chamber1: DelayLine,
    pub alpha: f32,
    pub beta: f32,
    #[serde(skip)]
    c1_out: f32,
    #[serde(skip)]
    c0_out: f32,
}

impl WaveGuide {
    pub fn new(delay: usize, alpha: f32, beta: f32, samples_per_second: u32) -> Self {
        Self {
            chamber0: DelayLine::new(delay, samples_per_second),
            chamber1: DelayLine::new(delay, samples_per_second),
            alpha,
            beta,
            c1_out: 0.0,
            c0_out: 0.0,
        }
    }

    #[inline]
    pub fn pop(&mut self) -> (f32, f32, bool) {
        let (c1_out, dampened_c1) = Self::dampen(self.chamber1.pop());
        let (c0_out, dampened_c0) = Self::dampen(self.chamber0.pop());
        self.c1_out = c1_out;
        self.c0_out = c0_out;
        (
            self.c1_out * (1.0 - self.alpha.abs()),
            self.c0_out * (1.0 - self.beta.abs()),
            dampened_c1 | dampened_c0,
        )
    }

    #[inline]
    pub fn dampen(sample: f32) -> (f32, bool) {
        let sample_abs = sample.abs();
        if sample_abs > WAVEGUIDE_MAX_AMP {
            (
                sample.signum()
                    * (-1.0 / (sample_abs - WAVEGUIDE_MAX_AMP + 1.0) + 1.0 + WAVEGUIDE_MAX_AMP),
                true,
            )
        } else {
            (sample, false)
        }
    }

    #[inline]
    pub fn push(&mut self, x0_in: f32, x1_in: f32) {
        let c0_in = self.c1_out * self.alpha + x0_in;
        let c1_in = self.c0_out * self.beta + x1_in;
        self.chamber0.push(c0_in);
        self.chamber1.push(c1_in);
        self.chamber0.samples.advance();
        self.chamber1.samples.advance();
    }
}

/// A simulated engine cylinder with intake/exhaust waveguides and combustion timing.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Cylinder {
    pub crank_offset: f32,
    pub exhaust_waveguide: WaveGuide,
    pub intake_waveguide: WaveGuide,
    pub extractor_waveguide: WaveGuide,
    pub intake_open_refl: f32,
    pub intake_closed_refl: f32,
    pub exhaust_open_refl: f32,
    pub exhaust_closed_refl: f32,
    pub piston_motion_factor: f32,
    pub ignition_factor: f32,
    pub ignition_time: f32,
    #[serde(skip)]
    pub cyl_sound: f32,
    #[serde(skip)]
    pub extractor_exhaust: f32,
}

impl Cylinder {
    #[inline]
    pub fn pop(
        &mut self,
        crank_pos: f32,
        exhaust_collector: f32,
        intake_valve_shift: f32,
        exhaust_valve_shift: f32,
    ) -> (f32, f32, f32, bool) {
        let crank = (crank_pos + self.crank_offset).fract();
        self.cyl_sound = piston_motion(crank) * self.piston_motion_factor
            + fuel_ignition(crank, self.ignition_time) * self.ignition_factor;

        let ex_valve = exhaust_valve((crank + exhaust_valve_shift).fract());
        let in_valve = intake_valve((crank + intake_valve_shift).fract());

        self.exhaust_waveguide.alpha = self.exhaust_closed_refl
            + (self.exhaust_open_refl - self.exhaust_closed_refl) * ex_valve;
        self.intake_waveguide.alpha =
            self.intake_closed_refl + (self.intake_open_refl - self.intake_closed_refl) * in_valve;

        let ex_wg_ret = self.exhaust_waveguide.pop();
        let in_wg_ret = self.intake_waveguide.pop();

        let extractor_wg_ret = self.extractor_waveguide.pop();
        self.extractor_exhaust = extractor_wg_ret.0;
        self.extractor_waveguide.push(ex_wg_ret.1, exhaust_collector);

        (
            in_wg_ret.1,
            extractor_wg_ret.1,
            self.cyl_sound,
            ex_wg_ret.2 | in_wg_ret.2 | extractor_wg_ret.2,
        )
    }

    #[inline]
    pub fn push(&mut self, intake: f32) {
        let ex_in = (1.0 - self.exhaust_waveguide.alpha.abs()) * self.cyl_sound * 0.5;
        self.exhaust_waveguide.push(ex_in, self.extractor_exhaust);
        let in_in = (1.0 - self.intake_waveguide.alpha.abs()) * self.cyl_sound * 0.5;
        self.intake_waveguide.push(in_in, intake);
    }
}

/// Exhaust muffler system with a main straight pipe and expansion chambers.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Muffler {
    pub straight_pipe: WaveGuide,
    pub muffler_elements: Vec<WaveGuide>,
}

/// Complete physical engine specification.
#[derive(Clone, Serialize, Deserialize, Debug)]
#[allow(dead_code)]
pub struct Engine {
    pub rpm: f32,
    pub intake_volume: f32,
    pub exhaust_volume: f32,
    pub engine_vibrations_volume: f32,
    pub cylinders: Vec<Cylinder>,
    #[serde(skip)]
    pub intake_noise: Noise,
    pub intake_noise_factor: f32,
    pub intake_noise_lp: LowPassFilter,
    pub engine_vibration_filter: LowPassFilter,
    pub muffler: Muffler,
    pub intake_valve_shift: f32,
    pub exhaust_valve_shift: f32,
    pub crankshaft_fluctuation: f32,
    pub crankshaft_fluctuation_lp: LowPassFilter,
    #[serde(skip)]
    pub crankshaft_noise: Noise,
    #[serde(skip)]
    pub crankshaft_pos: f32,
    #[serde(skip)]
    pub exhaust_collector: f32,
    #[serde(skip)]
    pub intake_collector: f32,
    pub exhaust_filter_cutoff: f32,
}

#[inline]
fn exhaust_valve(crank_pos: f32) -> f32 {
    if (0.75..1.0).contains(&crank_pos) {
        -(crank_pos * PI4F).sin()
    } else {
        0.0
    }
}

#[inline]
fn intake_valve(crank_pos: f32) -> f32 {
    if (0.0..0.25).contains(&crank_pos) {
        (crank_pos * PI4F).sin()
    } else {
        0.0
    }
}

#[inline]
fn piston_motion(crank_pos: f32) -> f32 {
    (crank_pos * PI4F).cos()
}

///// Realistic asymmetric combustion pressure wave.
/// Smooth C1 continuous pressure pulse with zero derivative at boundaries.
/// Eliminates high-frequency clicking and crackle while delivering deep low-end visceral punch.
#[inline]
fn fuel_ignition(crank_pos: f32, ignition_time: f32) -> f32 {
    let duration = ignition_time.max(0.20);
    if crank_pos > 0.5 && crank_pos < 0.5 + duration {
        let phase = (crank_pos - 0.5) / duration;
        let pulse = (phase * std::f32::consts::PI).sin();
        let expansion = (1.0 - phase).powf(1.4);
        pulse * pulse * (0.35 + 0.65 * expansion) * 2.8
    } else {
        0.0
    }
}

/// Core audio sample generator managing the engine DSP state.
pub struct Generator {
    pub volume: f32,
    pub samples_per_second: u32,
    pub engine: Engine,
    pub dc_lp: LowPassFilter,
    pub exhaust_lowpass: LowPassFilter,
    pub sub_bass_filter: LowPassFilter,
    pub intake_lowpass: LowPassFilter,
    pub throttle_smoothed: f32,
}

impl Generator {
    pub fn new(samples_per_second: u32, engine: Engine, dc_lp: LowPassFilter) -> Self {
        let cutoff = engine.exhaust_filter_cutoff.min(850.0);
        Self {
            volume: 0.18,
            samples_per_second,
            engine,
            dc_lp,
            exhaust_lowpass: LowPassFilter::new(cutoff, samples_per_second),
            sub_bass_filter: LowPassFilter::new(140.0, samples_per_second),
            intake_lowpass: LowPassFilter::new(550.0, samples_per_second),
            throttle_smoothed: 0.0,
        }
    }

    #[inline]
    pub fn gen_sample(&mut self) -> f32 {
        let num_cyl = self.engine.cylinders.len() as f32;
        let last_exhaust_collector = self.engine.exhaust_collector / num_cyl;
        self.engine.exhaust_collector = 0.0;
        self.engine.intake_collector = 0.0;

        let mut engine_vibration = 0.0;

        for cylinder in self.engine.cylinders.iter_mut() {
            let (cyl_intake, cyl_exhaust, cyl_vib, _) = cylinder.pop(
                self.engine.crankshaft_pos,
                last_exhaust_collector,
                self.engine.intake_valve_shift,
                self.engine.exhaust_valve_shift,
            );

            self.engine.intake_collector += cyl_intake;
            self.engine.exhaust_collector += cyl_exhaust;
            engine_vibration += cyl_vib;
        }

        let straight_pipe_wg_ret = self.engine.muffler.straight_pipe.pop();
        let mut muffler_wg_ret = (0.0_f32, 0.0_f32, false);

        for muffler_line in self.engine.muffler.muffler_elements.iter_mut() {
            let ret = muffler_line.pop();
            muffler_wg_ret.0 += ret.0;
            muffler_wg_ret.1 += ret.1;
        }

        // Push inputs without random noise to keep signal completely pure and free of crackle
        for cylinder in self.engine.cylinders.iter_mut() {
            cylinder.push(self.engine.intake_collector / num_cyl);
        }

        self.engine
            .muffler
            .straight_pipe
            .push(self.engine.exhaust_collector, muffler_wg_ret.0);

        self.engine.exhaust_collector += straight_pipe_wg_ret.0;

        let muffler_elements = self.engine.muffler.muffler_elements.len() as f32;
        for muffler_delay_line in self.engine.muffler.muffler_elements.iter_mut() {
            muffler_delay_line.push(straight_pipe_wg_ret.1 / muffler_elements, 0.0);
        }

        // 1. Raw exhaust merge with straight-pipe core and resonant muffler tailpipes
        let raw_exhaust = straight_pipe_wg_ret.0 * 0.70 + muffler_wg_ret.1 * 1.5;

        // 2. Non-linear wave shaping (analog soft saturation for raw, throaty bark without harsh clicks)
        let x = raw_exhaust * 1.35;
        let raspy_exhaust = (x + 0.12 * x * x.abs()) / (1.0 + x.abs());

        // 3. Acoustic exhaust filter to eliminate high-frequency peep/whistling
        let deep_exhaust = self.exhaust_lowpass.filter(raspy_exhaust);

        // 4. Sub-bass visceral displacement punch from cylinder pressure & rotational throb
        let crank_rad = self.engine.crankshaft_pos * PI4F;
        let crank_sub = (crank_rad * 0.5).sin() * 0.35 + crank_rad.sin() * 0.65;
        let raw_bass = engine_vibration * 1.4 + crank_sub * (0.8 + 0.6 * self.throttle_smoothed);
        let sub_bass = self.sub_bass_filter.filter(raw_bass);

        // 5. Deep induction intake roar (filtered airbox acoustics)
        let intake_roar = self
            .intake_lowpass
            .filter(self.engine.intake_collector / num_cyl);

        let mixed = (deep_exhaust * self.engine.exhaust_volume
            + sub_bass * self.engine.engine_vibrations_volume
            + intake_roar * self.engine.intake_volume)
            * self.volume;

        // DC blocking filter subtraction
        mixed - self.dc_lp.filter(mixed)
    }
}

/// JSON deserializable configuration structure for engine parameters.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LamboConfig {
    #[serde(default = "default_engine_name")]
    pub engine_name: String,
    #[serde(default = "default_cylinders")]
    pub cylinders: usize,
    #[serde(default = "default_firing_order")]
    pub firing_order: Vec<f32>,
    #[serde(default = "default_idle_rpm")]
    pub idle_rpm: f32,
    #[serde(default = "default_redline_rpm")]
    pub redline_rpm: f32,
    #[serde(default = "default_intake_volume")]
    pub intake_volume: f32,
    #[serde(default = "default_exhaust_volume")]
    pub exhaust_volume: f32,
    #[serde(default = "default_vibrations_volume")]
    pub engine_vibrations_volume: f32,
    #[serde(default = "default_intake_noise")]
    pub intake_noise_factor: f32,
    #[serde(default = "default_exhaust_delay")]
    pub exhaust_waveguide_delay: usize,
    #[serde(default = "default_intake_delay")]
    pub intake_waveguide_delay: usize,
    #[serde(default = "default_extractor_delay")]
    pub extractor_waveguide_delay: usize,
    #[serde(default = "default_straight_pipe_delay")]
    pub straight_pipe_delay: usize,
    #[serde(default = "default_muffler_chambers")]
    pub muffler_chambers: Vec<usize>,
    #[serde(default = "default_exhaust_filter")]
    pub exhaust_filter_cutoff: f32,
}

fn default_engine_name() -> String {
    "Lamborghini 5.2L V10".to_string()
}
fn default_cylinders() -> usize {
    10
}
fn default_firing_order() -> Vec<f32> {
    vec![0.0, 0.5, 0.2, 0.7, 0.4, 0.9, 0.1, 0.6, 0.3, 0.8]
}
fn default_idle_rpm() -> f32 {
    850.0
}
fn default_redline_rpm() -> f32 {
    7500.0
}
fn default_intake_volume() -> f32 {
    0.35
}
fn default_exhaust_volume() -> f32 {
    1.50
}
fn default_vibrations_volume() -> f32 {
    1.25
}
fn default_intake_noise() -> f32 {
    0.0
}
fn default_exhaust_delay() -> usize {
    165
}
fn default_intake_delay() -> usize {
    135
}
fn default_extractor_delay() -> usize {
    195
}
fn default_straight_pipe_delay() -> usize {
    480
}
fn default_muffler_chambers() -> Vec<usize> {
    vec![320, 480, 680, 920]
}
fn default_exhaust_filter() -> f32 {
    850.0
}

impl Default for LamboConfig {
    fn default() -> Self {
        Self {
            engine_name: default_engine_name(),
            cylinders: default_cylinders(),
            firing_order: default_firing_order(),
            idle_rpm: default_idle_rpm(),
            redline_rpm: default_redline_rpm(),
            intake_volume: default_intake_volume(),
            exhaust_volume: default_exhaust_volume(),
            engine_vibrations_volume: default_vibrations_volume(),
            intake_noise_factor: default_intake_noise(),
            exhaust_waveguide_delay: default_exhaust_delay(),
            intake_waveguide_delay: default_intake_delay(),
            extractor_waveguide_delay: default_extractor_delay(),
            straight_pipe_delay: default_straight_pipe_delay(),
            muffler_chambers: default_muffler_chambers(),
            exhaust_filter_cutoff: default_exhaust_filter(),
        }
    }
}

/// Searches for `assets/lambo_v10.json` or falls back to built-in tuned defaults.
pub fn load_lambo_config() -> LamboConfig {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("assets").join("lambo_v10.json"),
        PathBuf::from("assets/lambo_v10.json"),
        PathBuf::from("crates/benilla-lambo/assets/lambo_v10.json"),
    ];

    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(cfg) = serde_json::from_str::<LamboConfig>(&content) {
                eprintln!("benilla-lambo audio: loaded config from {:?}", path);
                return cfg;
            }
        }
    }

    eprintln!("benilla-lambo audio: using built-in default Lambo V10 parameters");
    LamboConfig::default()
}

/// Constructs a tuned 5.2L V10 Lamborghini engine acoustic preset.
/// Staggers runner lengths across the 10 cylinders to mimic physical tubular headers,
/// preventing destructive phase cancellation and producing a rich, raw, throaty roar.
pub fn build_engine_from_config(cfg: &LamboConfig, samples_per_second: u32) -> Engine {
    let firing_offsets = if cfg.firing_order.is_empty() {
        default_firing_order()
    } else {
        cfg.firing_order.clone()
    };
    let num_cyl = firing_offsets.len();

    let base_ex = cfg.exhaust_waveguide_delay.max(30);
    let base_in = cfg.intake_waveguide_delay.max(30);
    let base_ext = cfg.extractor_waveguide_delay.max(30);

    // Staggered tubular runner offsets for real dual-bank V10 acoustic separation
    let runner_stagger = [0, 14, 28, 42, 56, 7, 21, 35, 49, 63];

    let mut cylinders = Vec::with_capacity(num_cyl);
    for (i, &offset) in firing_offsets.iter().enumerate() {
        let stag = runner_stagger[i % runner_stagger.len()];
        let ex_delay = base_ex + stag;
        let in_delay = base_in + (stag / 2);
        let ext_delay = base_ext + stag;

        cylinders.push(Cylinder {
            crank_offset: offset,
            exhaust_waveguide: WaveGuide::new(ex_delay, -0.22, -0.68, samples_per_second),
            intake_waveguide: WaveGuide::new(in_delay, 0.18, -0.52, samples_per_second),
            extractor_waveguide: WaveGuide::new(ext_delay, 0.16, -0.72, samples_per_second),
            intake_open_refl: 0.36,
            intake_closed_refl: -0.92,
            exhaust_open_refl: 0.40,
            exhaust_closed_refl: -0.95,
            piston_motion_factor: 1.25,
            ignition_factor: 3.2,
            ignition_time: 0.18,
            cyl_sound: 0.0,
            extractor_exhaust: 0.0,
        });
    }

    let straight_pipe =
        WaveGuide::new(cfg.straight_pipe_delay.max(60), 0.70, -0.70, samples_per_second);

    let muffler_elements = cfg
        .muffler_chambers
        .iter()
        .map(|&d| WaveGuide::new(d.max(40), 0.52, -0.42, samples_per_second))
        .collect();

    Engine {
        rpm: cfg.idle_rpm,
        intake_volume: cfg.intake_volume,
        exhaust_volume: cfg.exhaust_volume,
        engine_vibrations_volume: cfg.engine_vibrations_volume,
        cylinders,
        intake_noise: Noise::new(0x1337BEEF),
        intake_noise_factor: cfg.intake_noise_factor,
        intake_noise_lp: LowPassFilter::new(300.0, samples_per_second),
        engine_vibration_filter: LowPassFilter::new(90.0, samples_per_second),
        muffler: Muffler {
            straight_pipe,
            muffler_elements,
        },
        intake_valve_shift: 0.0,
        exhaust_valve_shift: 0.0,
        crankshaft_fluctuation: 0.03,
        crankshaft_fluctuation_lp: LowPassFilter::new(180.0, samples_per_second),
        crankshaft_noise: Noise::new(0xFEEDFACE),
        crankshaft_pos: 0.0,
        exhaust_collector: 0.0,
        intake_collector: 0.0,
        exhaust_filter_cutoff: cfg.exhaust_filter_cutoff,
    }
}

pub fn lambo_v10(samples_per_second: u32) -> Engine {
    let cfg = load_lambo_config();
    build_engine_from_config(&cfg, samples_per_second)
}

/// Kira `SoundData` for playing the real-time procedural engine synthesizer.
pub struct EngineSoundData {
    pub engine: Engine,
    pub volume: f32,
    pub sample_rate: u32,
}

impl EngineSoundData {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            engine: lambo_v10(sample_rate),
            volume: 0.38,
            sample_rate,
        }
    }
}

/// Control handle for the running procedural engine audio stream.
#[derive(Clone)]
pub struct EngineSoundHandle {
    rpm: Arc<AtomicU32>,
    throttle: Arc<AtomicU32>,
    volume: Arc<AtomicU32>,
    #[allow(dead_code)]
    stopped: Arc<AtomicBool>,
}

impl EngineSoundHandle {
    pub fn set_rpm(&self, rpm: f32) {
        self.rpm.store(
            (rpm.max(100.0).round()) as u32,
            Ordering::Relaxed,
        );
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
        let volume = Arc::new(AtomicU32::new((self.volume * 1000.0) as u32));
        let stopped = Arc::new(AtomicBool::new(false));

        let handle = EngineSoundHandle {
            rpm: rpm.clone(),
            throttle: throttle.clone(),
            volume: volume.clone(),
            stopped: stopped.clone(),
        };

        let sound = EngineSound {
            generator: Generator::new(
                self.sample_rate,
                self.engine,
                LowPassFilter::new(10.0, self.sample_rate),
            ),
            rpm,
            throttle,
            volume,
            stopped,
            current_rpm: 850.0,
            current_throttle: 0.0,
        };

        Ok((Box::new(sound), handle))
    }
}

/// An actively streaming procedural engine sound running on Kira's audio thread.
pub struct EngineSound {
    generator: Generator,
    rpm: Arc<AtomicU32>,
    throttle: Arc<AtomicU32>,
    volume: Arc<AtomicU32>,
    stopped: Arc<AtomicBool>,
    current_rpm: f32,
    current_throttle: f32,
}

impl Sound for EngineSound {
    fn process(&mut self, out: &mut [Frame], dt: f64, _info: &Info) {
        if self.stopped.load(Ordering::Relaxed) {
            for frame in out.iter_mut() {
                *frame = Frame::ZERO;
            }
            return;
        }

        let target_rpm = self.rpm.load(Ordering::Relaxed) as f32;
        let target_throttle = (self.throttle.load(Ordering::Relaxed) as f32) / 1000.0;
        let master_vol = (self.volume.load(Ordering::Relaxed) as f32) / 1000.0;

        self.generator.volume = master_vol;

        // Smooth RPM and throttle transitions
        let dt_f32 = dt as f32;
        let chunk_dt = dt_f32 * out.len() as f32;
        let smooth_rpm = 1.0 - (-12.0 * chunk_dt).exp();
        self.current_rpm += (target_rpm - self.current_rpm) * smooth_rpm;

        let smooth_thr = 1.0 - (-16.0 * chunk_dt).exp();
        self.current_throttle += (target_throttle - self.current_throttle) * smooth_thr;

        self.generator.engine.rpm = self.current_rpm.max(100.0);
        self.generator.throttle_smoothed = self.current_throttle;

        // Dynamic load scaling
        let thr = self.current_throttle.clamp(0.0, 1.0);
        self.generator.engine.intake_volume = 0.30 + 0.70 * thr;
        self.generator.engine.exhaust_volume = 0.85 + 0.55 * thr;

        let samples_per_second = self.generator.samples_per_second as f32 * 120.0;
        let inc = self.generator.engine.rpm / samples_per_second;

        for frame in out.iter_mut() {
            self.generator.engine.crankshaft_pos =
                (self.generator.engine.crankshaft_pos + inc).fract();
            let sample = self.generator.gen_sample();

            // Saturated soft clipping limiter for warm analog supercar character
            let clipped = (sample * 1.5).tanh() * 0.88;
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
    fn test_v10_creation() {
        let engine = lambo_v10(48000);
        assert_eq!(engine.cylinders.len(), 10);
        assert_eq!(engine.rpm, 850.0);
        assert!(engine.exhaust_volume > 0.0);
        assert_eq!(engine.muffler.muffler_elements.len(), 4);
    }

    #[test]
    fn test_synth_data_and_processing() {
        let sound_data = EngineSoundData::new(48000);
        let (mut sound, handle) = sound_data.into_sound().expect("into_sound");

        handle.set_rpm(6500.0);
        handle.set_throttle(0.85);

        let mut frames = vec![Frame::ZERO; 512];
        let dt = 1.0 / 48000.0;
        let info = kira::info::MockInfoBuilder::new().build();

        sound.process(&mut frames, dt, &info);

        let mut has_nonzero = false;
        for frame in &frames {
            assert!(!frame.left.is_nan());
            assert!(!frame.right.is_nan());
            assert!(frame.left.abs() <= 1.0);
            assert!(frame.right.abs() <= 1.0);
            if frame.left.abs() > 0.0001 {
                has_nonzero = true;
            }
        }
        assert!(has_nonzero, "synthesizer should generate audio samples");
        assert!(!sound.finished());

        handle.stop();
        assert!(sound.finished());
    }
}
