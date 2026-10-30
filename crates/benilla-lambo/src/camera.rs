//! The chase camera and the speed readout.

use std::f32::consts::{PI, TAU};

use bevy::camera::{PerspectiveProjection, Projection};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;
use bevy::render::view::Hdr;
use bevy::window::PrimaryWindow;

use benilla_assets::coords::wow_to_bevy;
use benilla_world::collision::WorldCollision;
use benilla_world::ffx_glow::FfxGlow;
use benilla_world::view::{MsaaSetting, WorldCamera, CAM_FAR, NEARCLIP_DEFAULT};

use crate::car::{smooth, Car, TOP_SPEED};

const DISTANCE: f32 = 5.5;
const HEIGHT: f32 = 3.0;
const BASE_FOV: f32 = 1.05;

/// The camera's own heading, which trails the car's.
#[derive(Component)]
pub struct ChaseCam {
    pub yaw: f32,
    pub snapped: bool,
}

/// The world camera, as the world viewer spawns it, with the chase state on top.
pub fn spawn_camera(mut commands: Commands, msaa: Res<MsaaSetting>) {
    commands.spawn((
        Camera3d::default(),
        WorldCamera,
        msaa.level(),
        Projection::from(PerspectiveProjection {
            far: CAM_FAR,
            near: NEARCLIP_DEFAULT,
            fov: BASE_FOV,
            ..default()
        }),
        Hdr,
        Tonemapping::None,
        FfxGlow::WORLD,
        Transform::from_translation(wow_to_bevy(crate::car::SPAWN_WOW) + Vec3::Y * 20.0),
        ChaseCam {
            yaw: crate::car::SPAWN_YAW_DEG.to_radians(),
            snapped: false,
        },
        AmbientLight {
            color: Color::WHITE,
            brightness: 500.0,
            ..default()
        },
    ));

    // Directional light so glTF PBR materials (which default to lit) render under Bevy's PBR pipeline.
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(30.0, 100.0, 30.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

/// Follows the car from behind, trailing its heading, widening the view with speed; C looks back.
pub fn chase(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    world: WorldCollision,
    car: Query<&Car>,
    mut cam: Query<(&mut Transform, &mut Projection, &mut ChaseCam), Without<Car>>,
) {
    let (Ok(car), Ok((mut tf, mut proj, mut chase))) = (car.single(), cam.single_mut()) else {
        return;
    };
    let dt = time.delta_secs().min(1.0 / 20.0);

    let target_yaw = car.yaw + if keys.pressed(KeyCode::KeyC) { PI } else { 0.0 };
    let diff = (target_yaw - chase.yaw + PI).rem_euclid(TAU) - PI;
    chase.yaw += if chase.snapped {
        diff * smooth(dt, 5.0)
    } else {
        diff
    };

    let speed = car.speed();
    let back = Vec3::new(chase.yaw.sin(), 0.0, chase.yaw.cos());
    let ideal_cam = car.pos + back * DISTANCE + Vec3::Y * (HEIGHT + speed * 0.01);

    // Car focus point for line-of-sight raycasting (just above the car center)
    let focus = car.pos + Vec3::Y * 0.9;
    let to_cam = ideal_cam - focus;
    let max_dist = to_cam.length();

    // Check if direct line-of-sight between car and camera is blocked (walls, cave ceiling, etc.)
    let (want, occluded) = if max_dist > 0.1 {
        let dir = Dir3::new(to_cam / max_dist).unwrap_or(Dir3::Y);
        if let Some(hit) = world.ray_body(focus, dir, max_dist) {
            // Direct line of sight is blocked: move closer to the car along the ray, never up
            let hit_dist = (hit.distance - 0.3).max(1.2);
            (focus + dir.as_vec3() * hit_dist, true)
        } else {
            (ideal_cam, false)
        }
    } else {
        (ideal_cam, false)
    };

    // When direct LoS is blocked, jump immediately closer to avoid clipping inside geometry.
    // Otherwise, smoothly follow the desired camera position.
    tf.translation = if chase.snapped && !occluded {
        tf.translation.lerp(want, smooth(dt, 10.0))
    } else {
        want
    };
    chase.snapped = true;

    let cam_dist = (tf.translation - focus).length();
    let look_ahead = (cam_dist * 0.35).clamp(0.5, 3.0);
    let look = car.pos + car.up * 0.8 + car.basis().0 * look_ahead;
    tf.look_at(look, Vec3::Y);

    if let Projection::Perspective(p) = &mut *proj {
        let nitro_fov = if car.nitro_timer > 0.0 {
            (car.nitro_timer / 1.0) * 0.35
        } else {
            0.0
        };
        let fov = BASE_FOV + (speed / TOP_SPEED).clamp(0.0, 1.0) * 0.25 + nitro_fov;
        let rate = if car.nitro_timer > 0.0 { 8.0 } else { 3.0 };
        p.fov += (fov - p.fov) * smooth(dt, rate);
    }
}

/// Shows the speed, nitro cooldown, and time trial progress in the window title, five times a second.
pub fn speedometer(
    time: Res<Time>,
    car: Query<&Car>,
    manager: Option<Res<crate::trial::TimeTrialManager>>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    mut since: Local<f32>,
) {
    *since += time.delta_secs();
    if *since < 0.2 {
        return;
    }
    *since = 0.0;
    let (Ok(car), Ok(mut window)) = (car.single(), window.single_mut()) else {
        return;
    };
    // Yards to metres, then m/s to km/h.
    let kmh = car.speed() * 0.9144 * 3.6;
    let nitro_str = if car.nitro_cooldown <= 0.0 {
        " [NITRO READY (Shift)]".to_string()
    } else {
        format!(" [NITRO {:.1}s]", car.nitro_cooldown)
    };

    if let Some(ref m) = manager {
        if m.menu_open {
            window.title =
                "benilla lambo — [ESC MENU OPEN] Up/Down or W/S + Enter to select, ESC to resume"
                    .to_string();
            return;
        }

        if let Some(trial) = m.current_trial() {
            let target_bevy = wow_to_bevy(m.target_gate().unwrap_or(trial.end));
            let dist = Vec2::new(car.pos.x - target_bevy.x, car.pos.z - target_bevy.z).length();
            let race = m.race_label();
            let race = if race.is_empty() {
                race
            } else {
                format!(" {race} |")
            };

            match m.state {
                crate::trial::TrialState::WaitingToStart => {
                    window.title = format!(
                        "benilla lambo — [{}] READY (Press W) — {kmh:.0} km/h{nitro_str} — ESC menu, R restart",
                        trial.name
                    );
                }
                crate::trial::TrialState::Countdown { remaining } => {
                    window.title = format!(
                        "benilla lambo — [{}] VERSUS — starting in {:.0}",
                        trial.name,
                        remaining.ceil()
                    );
                }
                crate::trial::TrialState::Racing { elapsed } => {
                    window.title = format!(
                        "benilla lambo — [{}]{race} TIME: {elapsed:.2}s | Next: {dist:.0}yd — {kmh:.0} km/h{nitro_str} — ESC menu, R restart",
                        trial.name
                    );
                }
                crate::trial::TrialState::Finished {
                    final_time,
                    is_best,
                } => {
                    let record_badge = if is_best { " ★ NEW RECORD! ★" } else { "" };
                    window.title = format!(
                        "benilla lambo — [{}] FINISHED: {final_time:.2}s!{record_badge} — {kmh:.0} km/h — ESC menu, R restart",
                        trial.name
                    );
                }
                crate::trial::TrialState::FreeDriving => {}
            }
            return;
        }
    }

    window.title = format!("benilla lambo — Free Driving — {kmh:.0} km/h{nitro_str} — WASD drive, Space handbrake, Shift nitro, ESC menu, R reset");
}
