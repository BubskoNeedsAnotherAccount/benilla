//! Engine, skid, and nitro sound effects for benilla-lambo powered by Kira.

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use kira::{
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
};

use crate::car::{smooth, Car, TOP_SPEED};

/// Holds Kira's audio manager and active sound handles.
#[derive(Resource)]
pub struct CarAudio {
    manager: AudioManager<DefaultBackend>,
    engine_handle: Option<StaticSoundHandle>,
    skid_handle: Option<StaticSoundHandle>,
    nitro_data: Option<StaticSoundData>,
    current_engine_rate: f32,
    current_engine_db: f32,
    current_skid_db: f32,
}

impl CarAudio {
    /// Attempts to locate `assets/sounds/` and load all three sound effects.
    pub fn new() -> Option<Self> {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidates = [
            manifest.join("assets").join("sounds"),
            PathBuf::from("assets/sounds"),
            PathBuf::from("crates/benilla-lambo/assets/sounds"),
        ];
        let sounds_dir = candidates.into_iter().find(|p| p.exists())?;

        let engine_path = sounds_dir.join("engine_loop.wav");
        let skid_path = sounds_dir.join("skid_loop.wav");
        let nitro_path = sounds_dir.join("nitro.wav");

        let mut manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("benilla-lambo audio: failed to initialize audio manager: {e}");
                return None;
            }
        };

        let engine_handle = match StaticSoundData::from_file(&engine_path) {
            Ok(data) => {
                let looped = data.loop_region(..);
                match manager.play(looped) {
                    Ok(mut h) => {
                        h.set_volume(Decibels(-6.0), Tween::default());
                        h.set_playback_rate(0.70, Tween::default());
                        Some(h)
                    }
                    Err(e) => {
                        eprintln!("benilla-lambo audio: failed to play engine loop: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                eprintln!("benilla-lambo audio: failed to load {engine_path:?}: {e}");
                None
            }
        };

        let skid_handle = match StaticSoundData::from_file(&skid_path) {
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
        };

        let nitro_data = match StaticSoundData::from_file(&nitro_path) {
            Ok(data) => Some(data),
            Err(e) => {
                eprintln!("benilla-lambo audio: failed to load {nitro_path:?}: {e}");
                None
            }
        };

        eprintln!(
            "benilla-lambo audio: initialized (engine: {}, skid: {}, nitro: {})",
            engine_handle.is_some(),
            skid_handle.is_some(),
            nitro_data.is_some()
        );

        Some(Self {
            manager,
            engine_handle,
            skid_handle,
            nitro_data,
            current_engine_rate: 0.70,
            current_engine_db: -6.0,
            current_skid_db: -60.0,
        })
    }
}

/// Updates engine pitch/volume and skid loop playback from car state, and plays nitro on trigger.
pub fn update_car_audio(time: Res<Time>, car: Query<&Car>, mut audio: Option<ResMut<CarAudio>>) {
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
            let _ = audio.manager.play(sound);
        }
    }

    let speed = car.speed();

    // 2. Engine sound loop modulation
    if let Some(ref mut engine) = audio.engine_handle {
        if !car.placed {
            engine.set_volume(Decibels::SILENCE, Tween::default());
        } else {
            // Speed component
            let speed_ratio = (speed / TOP_SPEED).clamp(0.0, 1.8);
            let throttle_rev = if car.throttle_input > 0.0 {
                0.22 * car.throttle_input
            } else if car.throttle_input < 0.0 {
                0.12 * car.throttle_input.abs()
            } else {
                0.0
            };
            let nitro_rev = if car.nitro_timer > 0.0 {
                0.55 * (car.nitro_timer / 1.0)
            } else {
                0.0
            };

            // Base rate: 0.75 idle -> up to ~2.4x at top speed + throttle + nitro
            let target_rate =
                (0.75 + speed_ratio * 1.35 + throttle_rev + nitro_rev).clamp(0.6, 2.8);
            audio.current_engine_rate +=
                (target_rate - audio.current_engine_rate) * smooth(dt, 8.0);
            engine.set_playback_rate(audio.current_engine_rate as f64, Tween::default());

            // Volume: idle is -6 dB, driving accelerates up to 0 dB, nitro up to +2.0 dB
            let target_db =
                -6.0 + speed_ratio * 4.5 + (if car.nitro_timer > 0.0 { 3.5 } else { 0.0 });
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
                || (car.handbrake_input && speed > 5.0)
                || (car.throttle_input < 0.0 && v_f > 12.0));

        if is_skidding {
            let lateral_intensity = ((car.lateral_speed - 2.5) / 10.0).clamp(0.0, 1.0);
            let handbrake_intensity = if car.handbrake_input && speed > 5.0 {
                (speed / 25.0).clamp(0.4, 1.0)
            } else {
                0.0
            };
            let brake_intensity = if car.throttle_input < 0.0 && v_f > 12.0 {
                (v_f / 35.0).clamp(0.2, 0.8)
            } else {
                0.0
            };

            let intensity = lateral_intensity
                .max(handbrake_intensity)
                .max(brake_intensity);
            // Map 0..1 to -16 dB .. 0 dB
            let target_db = -16.0 + intensity * 16.0;
            audio.current_skid_db += (target_db - audio.current_skid_db) * smooth(dt, 14.0);
            skid.set_volume(Decibels(audio.current_skid_db), Tween::default());

            let skid_rate = 0.95 + (speed / TOP_SPEED).clamp(0.0, 1.0) * 0.25;
            skid.set_playback_rate(skid_rate as f64, Tween::default());
        } else {
            // Fade out quickly to silence
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
