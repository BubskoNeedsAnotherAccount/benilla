//! Engine, skid, and nitro sound effects for benilla-lambo powered by Kira.
//! Uses a real-time procedural physical acoustic engine synthesizer inspired by `enginesound`.

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use kira::{
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
};

use crate::car::{smooth, Car, GEAR_BOUNDS, TOP_SPEED};
use crate::engine_synth::{EngineSoundData, EngineSoundHandle};

/// Holds Kira's audio manager and active sound handles.
#[derive(Resource)]
pub struct CarAudio {
    manager: AudioManager<DefaultBackend>,
    /// Procedural real-time physical acoustic engine synthesizer handle.
    synth_handle: Option<EngineSoundHandle>,
    /// Optional legacy static loop handle (fallback).
    static_engine_handle: Option<StaticSoundHandle>,
    skid_handle: Option<StaticSoundHandle>,
    nitro_data: Option<StaticSoundData>,
    current_engine_rate: f32,
    current_engine_db: f32,
    current_skid_db: f32,
    /// The gear the engine sound last ran in, to cut the throttle as the car's gear changes.
    current_gear: usize,
    simulated_rpm: f32,
    gear_shift_timer: f32,
    limiter_timer: f32,
}

/// One sound effect, by file name: from `assets/sounds/` on disk, or from the files the page
/// fetched ahead of the start in a browser.
fn load_effect(
    sounds_dir: Option<&std::path::Path>,
    name: &str,
) -> Result<StaticSoundData, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = sounds_dir;
        let bytes = crate::web::asset(&format!("sounds/{name}"))
            .ok_or_else(|| format!("{name} was not fetched"))?;
        StaticSoundData::from_cursor(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = sounds_dir.ok_or_else(|| "no assets/sounds directory".to_string())?;
        StaticSoundData::from_file(dir.join(name)).map_err(|e| e.to_string())
    }
}

impl CarAudio {
    /// Sets the master volume, 0 to 1 as an amplitude, on Kira's main track.
    pub fn set_master_volume(&mut self, volume: f32) {
        let db = if volume <= 0.001 {
            Decibels::SILENCE
        } else {
            Decibels(20.0 * volume.log10())
        };
        self.manager.main_track().set_volume(db, Tween::default());
        // The engine does not go through kira's mixer in a browser.
        #[cfg(target_arch = "wasm32")]
        crate::web::set_synth_master(volume.clamp(0.0, 1.0));
    }

    /// Turns the engine sound on or off. Off, nothing renders it: the page stops its worker, and
    /// natively the synth is stopped and a fresh one started when it comes back.
    pub fn set_engine_sound(&mut self, on: bool) {
        #[cfg(target_arch = "wasm32")]
        crate::web::set_synth_enabled(on);
        #[cfg(not(target_arch = "wasm32"))]
        match (on, self.synth_handle.is_some()) {
            (false, true) => {
                if let Some(synth) = self.synth_handle.take() {
                    synth.stop();
                }
            }
            (true, false) => match self.manager.play(EngineSoundData::new(48000)) {
                Ok(h) => self.synth_handle = Some(h),
                Err(e) => {
                    eprintln!("benilla-lambo audio: failed to start procedural synthesizer: {e}")
                }
            },
            _ => {}
        }
    }

    /// Attempts to locate `assets/sounds/` and load all sound effects.
    pub fn new() -> Option<Self> {
        // A browser has no directory to find; `load_effect` asks the page.
        #[cfg(target_arch = "wasm32")]
        let sounds_dir = Some(PathBuf::new());
        #[cfg(not(target_arch = "wasm32"))]
        let sounds_dir = {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let candidates = [
                manifest.join("assets").join("sounds"),
                PathBuf::from("assets/sounds"),
                PathBuf::from("crates/benilla-lambo/assets/sounds"),
            ];
            candidates.into_iter().find(|p| p.exists())
        };

        let mut manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("benilla-lambo audio: failed to initialize audio manager: {e}");
                return None;
            }
        };

        // 1. Initialize real-time procedural physical engine synthesizer (48 kHz)
        let synth_data = EngineSoundData::new(48000);
        // In a browser the page plays the engine on its own audio thread (`web/synth-worklet.js`),
        // so a frame that stalls cannot starve it; the handle only sends it the car's state.
        #[cfg(target_arch = "wasm32")]
        let synth_handle = Some(synth_data.handle());
        #[cfg(not(target_arch = "wasm32"))]
        let synth_handle = match manager.play(synth_data) {
            Ok(h) => {
                eprintln!("benilla-lambo audio: procedural V10 synthesizer online");
                Some(h)
            }
            Err(e) => {
                eprintln!("benilla-lambo audio: failed to start procedural synthesizer: {e}");
                None
            }
        };

        // 2. Load static sound effects if sounds directory is available
        let skid_handle = if let Some(ref dir) = sounds_dir {
            let skid_path = dir.join("skid_loop.wav");
            match load_effect(Some(dir), "skid_loop.wav") {
                Ok(data) => {
                    let looped = data.loop_region(..);
                    match manager.play(looped) {
                        Ok(mut h) => {
                            h.set_volume(Decibels::SILENCE, Tween::default());
                            Some(h)
                        }
                        Err(e) => {
                            eprintln!("benilla-lambo audio: failed to play skid loop: {e}");
                            None
                        }
                    }
                }
                Err(e) => {
                    eprintln!("benilla-lambo audio: failed to load {skid_path:?}: {e}");
                    None
                }
            }
        } else {
            None
        };

        let nitro_data = if let Some(ref dir) = sounds_dir {
            let nitro_path = dir.join("nitro.wav");
            match load_effect(Some(dir), "nitro.wav") {
                Ok(data) => Some(data),
                Err(e) => {
                    eprintln!("benilla-lambo audio: failed to load {nitro_path:?}: {e}");
                    None
                }
            }
        } else {
            None
        };

        eprintln!(
            "benilla-lambo audio: initialized (procedural_v10: {}, skid: {}, nitro: {})",
            synth_handle.is_some(),
            skid_handle.is_some(),
            nitro_data.is_some()
        );

        Some(Self {
            manager,
            synth_handle,
            static_engine_handle: None,
            skid_handle,
            nitro_data,
            current_engine_rate: 0.70,
            current_engine_db: -6.0,
            current_skid_db: -60.0,
            current_gear: 1,
            simulated_rpm: 850.0,
            gear_shift_timer: 0.0,
            limiter_timer: 0.0,
        })
    }
}

/// Updates engine pitch/volume and skid loop playback from car state, and plays nitro on trigger.
pub fn update_car_audio(
    time: Res<Time>,
    pop: Res<crate::exhaust::Pop>,
    car: Query<&Car>,
    mut audio: Option<ResMut<CarAudio>>,
) {
    let (Ok(car), Some(audio)) = (car.single(), audio.as_deref_mut()) else {
        return;
    };
    let dt = time.delta_secs().min(1.0 / 20.0);
    if dt <= 0.0 {
        return;
    }

    // 1. One-time Nitro sound trigger
    if car.just_used_nitro {
        if let Some(ref nitro_data) = audio.nitro_data {
            let mut sound = nitro_data.clone();
            sound.settings.volume = Decibels(1.5).into();
            audio.manager.play(sound).ok();
        }
    }

    let speed = car.speed();
    let throttle_pos = car.throttle_input.max(0.0);

    // 2. Procedural Engine Sound Synthesis (V10 physical model)
    if let Some(ref synth) = audio.synth_handle {
        if !car.placed {
            synth.set_volume(0.0);
            synth.set_rpm(850.0);
        } else {
            audio.gear_shift_timer = (audio.gear_shift_timer - dt).max(0.0);
            audio.limiter_timer += dt;

            // Handbrake held / neutral revving
            if car.handbrake_input {
                let target_rpm = if throttle_pos > 0.05 {
                    850.0 + throttle_pos * 6400.0
                } else {
                    850.0
                };
                let rate = if throttle_pos > 0.05 { 16.0 } else { 8.0 };
                audio.simulated_rpm += (target_rpm - audio.simulated_rpm) * smooth(dt, rate);

                synth.set_rpm(audio.simulated_rpm);
                synth.set_throttle(throttle_pos);
                synth.set_volume(0.36 + throttle_pos * 0.18);
                synth.set_drivetrain(0.0, 1);
            } else {
                // Dual-clutch quick-shift cut effect; an upshift's cut lights the unburnt
                // charge in the exhaust: a pop.
                if car.gear != audio.current_gear {
                    audio.current_gear = car.gear;
                    audio.gear_shift_timer = 0.08;
                }
                if pop.now {
                    synth.pop();
                }

                let gear_idx = (audio.current_gear - 1).min(6);
                let (gear_low, gear_high) = GEAR_BOUNDS[gear_idx];
                let gear_progress =
                    ((speed - gear_low) / (gear_high - gear_low).max(1.0)).clamp(0.0, 1.0);

                // Realistic gear RPM scaling for loud, high-revving sports car powerband
                const GEAR_RPM_RANGES: [(f32, f32); 7] = [
                    (850.0, 4200.0),  // Gear 1: 0..16 yd/s (~0..53 km/h)
                    (2000.0, 4500.0), // Gear 2: 16..30 yd/s (~53..100 km/h)
                    (2300.0, 4900.0), // Gear 3: 30..46 yd/s (~100..152 km/h)
                    (2600.0, 5400.0), // Gear 4: 46..64 yd/s (~152..211 km/h)
                    (3000.0, 5900.0), // Gear 5: 64..82 yd/s (~211..270 km/h)
                    (3400.0, 6400.0), // Gear 6: 82..100 yd/s (~270..330 km/h)
                    (3800.0, 7000.0), // Gear 7: 100+ yd/s (~330..445+ km/h)
                ];

                let (rpm_min, rpm_max) = GEAR_RPM_RANGES[gear_idx];

                let base_rpm = if audio.current_gear == 1 {
                    if car.burnout {
                        5200.0
                    } else if speed < 1.0 {
                        if throttle_pos > 0.05 {
                            850.0 + throttle_pos * 1600.0 // launch slip (up to ~2,450 RPM)
                        } else {
                            850.0
                        }
                    } else {
                        850.0 + gear_progress * (rpm_max - 850.0)
                    }
                } else {
                    // Under partial throttle / coasting, engine cruises lower; floored throttle revs higher
                    let throttle_rpm_lift = (throttle_pos - 0.5) * 450.0;
                    (rpm_min + gear_progress * (rpm_max - rpm_min) + throttle_rpm_lift)
                        .clamp(rpm_min, rpm_max)
                };

                // Nitro surge: adds high-speed howl without over-revving into ultrasonic frequencies
                let nitro_rpm = if car.nitro_timer > 0.0 {
                    700.0 * (car.nitro_timer / 1.0)
                } else {
                    0.0
                };

                let mut target_rpm = (base_rpm + nitro_rpm).min(7500.0);

                // Rev limiter bounce near top speed (> 108 yd/s)
                if speed >= TOP_SPEED * 0.98 {
                    let bounce = (audio.limiter_timer * 28.0).sin();
                    target_rpm = if bounce > 0.15 { 6600.0 } else { 7000.0 };
                }

                let rpm_smooth_rate = if throttle_pos > 0.05 { 10.0 } else { 5.0 };
                audio.simulated_rpm +=
                    (target_rpm - audio.simulated_rpm) * smooth(dt, rpm_smooth_rate);

                let effective_throttle = if audio.gear_shift_timer > 0.0 {
                    0.0
                } else {
                    throttle_pos
                };

                let speed_ratio = (speed / TOP_SPEED).clamp(0.0, 1.0);
                let target_vol =
                    0.38 + speed_ratio * 0.16 + (if car.nitro_timer > 0.0 { 0.14 } else { 0.0 });

                synth.set_rpm(audio.simulated_rpm);
                synth.set_throttle(effective_throttle);
                synth.set_volume(target_vol);
                synth.set_drivetrain(speed, audio.current_gear);
            }
        }
        #[cfg(target_arch = "wasm32")]
        synth.flush();
    } else if let Some(ref mut engine) = audio.static_engine_handle {
        // Fallback for static loop if procedural synth was unavailable
        if !car.placed {
            engine.set_volume(Decibels::SILENCE, Tween::default());
        } else {
            let speed_ratio = (speed / TOP_SPEED).clamp(0.0, 1.8);
            let target_rate = (0.75 + speed_ratio * 1.35).clamp(0.6, 2.8);
            audio.current_engine_rate +=
                (target_rate - audio.current_engine_rate) * smooth(dt, 8.0);
            engine.set_playback_rate(audio.current_engine_rate as f64, Tween::default());

            let target_db = -6.0 + speed_ratio * 4.5;
            audio.current_engine_db += (target_db - audio.current_engine_db) * smooth(dt, 6.0);
            engine.set_volume(Decibels(audio.current_engine_db), Tween::default());
        }
    }

    // 3. Skid sound loop modulation
    if let Some(ref mut skid) = audio.skid_handle {
        let (fwd, _) = car.basis();
        let v_f = car.vel.dot(fwd);
        let is_skidding = car.grounded
            && (car.lateral_speed > 3.0
                || car.burnout
                || (car.handbrake_input && speed > 5.0)
                || (car.brake_input > 0.0 && v_f > 10.0));

        if is_skidding {
            let lateral_intensity = ((car.lateral_speed - 2.5) / 10.0).clamp(0.0, 1.0);
            let handbrake_intensity = if car.handbrake_input && speed > 5.0 {
                (speed / 25.0).clamp(0.4, 1.0)
            } else {
                0.0
            };
            let brake_intensity = if car.brake_input > 0.0 && v_f > 10.0 {
                (v_f / 35.0).clamp(0.2, 0.8)
            } else {
                0.0
            };
            let burnout_intensity = if car.burnout { 0.85 } else { 0.0 };

            let intensity = lateral_intensity
                .max(handbrake_intensity)
                .max(brake_intensity)
                .max(burnout_intensity);
            let target_db = -16.0 + intensity * 16.0;
            audio.current_skid_db += (target_db - audio.current_skid_db) * smooth(dt, 14.0);
            skid.set_volume(Decibels(audio.current_skid_db), Tween::default());

            let skid_rate = 0.95 + (speed / TOP_SPEED).clamp(0.0, 1.0) * 0.25;
            skid.set_playback_rate(skid_rate as f64, Tween::default());
        } else {
            audio.current_skid_db += (-60.0 - audio.current_skid_db) * smooth(dt, 16.0);
            if audio.current_skid_db <= -56.0 {
                skid.set_volume(
                    Decibels::SILENCE,
                    Tween {
                        duration: Duration::from_millis(50),
                        ..default()
                    },
                );
            } else {
                skid.set_volume(Decibels(audio.current_skid_db), Tween::default());
            }
        }
    }
}
