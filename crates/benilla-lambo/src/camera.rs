//! The chase camera and the speed readout.

use std::f32::consts::{PI, TAU};

use bevy::camera::{PerspectiveProjection, Projection};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::render::view::Hdr;
use bevy::window::PrimaryWindow;
use bevy_egui::EguiContexts;

use benilla_assets::coords::wow_to_bevy;
use benilla_world::collision::WorldCollision;
use benilla_world::ffx_glow::FfxGlow;
use benilla_world::view::{MsaaSetting, WorldCamera, CAM_FAR, NEARCLIP_DEFAULT};

use crate::car::{smooth, Car, TOP_SPEED};

const DISTANCE: f32 = 5.5;
const HEIGHT: f32 = 3.0;
const BASE_FOV: f32 = 1.05;
/// Radians of orbit per pixel of mouse travel while dragging.
const DRAG_SENSITIVITY: f32 = 0.005;
/// How far the drag may tilt the camera from its resting angle, down and up.
const DRAG_PITCH_MIN: f32 = -0.45;
const DRAG_PITCH_MAX: f32 = 0.9;
/// How fast the camera swings back behind the car once the button is let go.
const DRAG_RETURN_RATE: f32 = 6.0;

/// The camera's own heading, which trails the car's, plus a mouse-drag orbit offset.
#[derive(Component)]
pub struct ChaseCam {
    pub yaw: f32,
    pub snapped: bool,
    /// Orbit offset from the chase position while the left button drags; eases to zero on release.
    pub drag_yaw: f32,
    pub drag_pitch: f32,
    pub dragging: bool,
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
            drag_yaw: 0.0,
            drag_pitch: 0.0,
            dragging: false,
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

/// Left-drag orbits the camera around the car; a drag that starts on the egui UI is left to it.
pub fn mouse_look(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut contexts: EguiContexts,
    mut cam: Query<&mut ChaseCam>,
) {
    let Ok(mut chase) = cam.single_mut() else {
        return;
    };
    if buttons.just_pressed(MouseButton::Left) {
        let over_ui = contexts
            .ctx_mut()
            .is_ok_and(|ctx| ctx.is_pointer_over_area() || ctx.wants_pointer_input());
        chase.dragging = !over_ui;
    }
    if !buttons.pressed(MouseButton::Left) {
        chase.dragging = false;
    }
    if chase.dragging {
        let d = motion.delta * DRAG_SENSITIVITY;
        chase.drag_yaw = (chase.drag_yaw - d.x + PI).rem_euclid(TAU) - PI;
        chase.drag_pitch = (chase.drag_pitch + d.y).clamp(DRAG_PITCH_MIN, DRAG_PITCH_MAX);
    }
}

/// Follows the car from behind, trailing its heading, widening the view with speed; C looks back,
/// and a left-drag orbit (`mouse_look`) springs back once released.
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

    // Once the button is let go, the drag offset eases back to the plain chase position.
    if !chase.dragging {
        let k = smooth(dt, DRAG_RETURN_RATE);
        chase.drag_yaw -= chase.drag_yaw * k;
        chase.drag_pitch -= chase.drag_pitch * k;
    }

    let speed = car.speed();
    let height = HEIGHT + speed * 0.01;
    let reach = DISTANCE.hypot(height);
    let pitch = height.atan2(DISTANCE) + chase.drag_pitch;
    let yaw = chase.yaw + chase.drag_yaw;
    let back = Vec3::new(yaw.sin(), 0.0, yaw.cos());
    let ideal_cam = car.pos + (back * pitch.cos() + Vec3::Y * pitch.sin()) * reach;

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
    // Looking ahead of the car only makes sense from behind it; an orbit looks at the car itself.
    let look_ahead = (cam_dist * 0.35).clamp(0.5, 3.0) * chase.drag_yaw.cos().max(0.0);
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
