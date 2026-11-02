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
/// Extra trailing distance at top speed, and at the start of a nitro burst.
const SPEED_PULLBACK: f32 = 1.6;
const NITRO_PULLBACK: f32 = 1.1;
/// The share of top speed the road starts to shake the camera at.
const SHAKE_FROM: f32 = 0.55;
/// Shake at full strength: position in yards, roll in radians.
const SHAKE_POS: f32 = 0.07;
const SHAKE_ROLL: f32 = 0.006;
/// How fast a jolt (landing, nitro) dies away, per second.
const TRAUMA_DECAY: f32 = 1.8;
/// Most the camera tilts into a corner, radians (2°), and the tilt per yd/s² of turn.
const TILT_MAX: f32 = 0.035;
const TILT_PER_ACCEL: f32 = 0.0007;

/// The camera's own heading, which trails the car's, plus a mouse-drag orbit offset.
#[derive(Component)]
pub struct ChaseCam {
    pub yaw: f32,
    pub snapped: bool,
    /// Orbit offset from the chase position while the left button drags; eases to zero on release.
    pub drag_yaw: f32,
    pub drag_pitch: f32,
    pub dragging: bool,
    /// The followed position before shake; the smoothing runs on it, never on the shaken one.
    pub base: Vec3,
    /// A jolt's strength, 0 to 1, from landings and nitro; the shake goes as its square.
    pub trauma: f32,
    /// The current tilt into the corner.
    pub tilt: f32,
    /// Last frame's car state, for the landing and the turn rate.
    pub was_grounded: bool,
    pub last_fall: f32,
    pub last_yaw: f32,
    /// Seconds of shake noise, its phase.
    pub clock: f32,
}

/// A smooth wobble in −1..1 at about `rate` Hz, from two sines that never line up.
fn wobble(t: f32, rate: f32, phase: f32) -> f32 {
    ((t * rate * TAU + phase).sin() + 0.6 * (t * rate * 2.37 * TAU + phase * 1.9).sin()) / 1.6
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
            base: Vec3::ZERO,
            trauma: 0.0,
            tilt: 0.0,
            was_grounded: false,
            last_fall: 0.0,
            last_yaw: crate::car::SPAWN_YAW_DEG.to_radians(),
            clock: 0.0,
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
    settings: Res<crate::settings::Settings>,
    car: Query<&Car>,
    mut cam: Query<(&mut Transform, &mut Projection, &mut ChaseCam), Without<Car>>,
) {
    let (Ok(car), Ok((mut tf, mut proj, mut chase))) = (car.single(), cam.single_mut()) else {
        return;
    };
    let dt = time.delta_secs().min(1.0 / 20.0);
    // A teleport (reset, a race start) is no turn and no landing.
    if !chase.snapped {
        chase.last_yaw = car.yaw;
        chase.was_grounded = car.grounded;
        chase.trauma = 0.0;
        chase.tilt = 0.0;
    }

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
    let speed_frac = (speed / TOP_SPEED).clamp(0.0, 1.0);
    // Speed stretches the camera away from the car; a nitro burst kicks it back further.
    let distance = DISTANCE + speed_frac * SPEED_PULLBACK + car.nitro_timer * NITRO_PULLBACK;
    let height = HEIGHT + speed * 0.01;
    let reach = distance.hypot(height);
    let pitch = height.atan2(distance) + chase.drag_pitch;
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
    chase.base = if chase.snapped && !occluded {
        chase.base.lerp(want, smooth(dt, 10.0))
    } else {
        want
    };
    chase.snapped = true;

    let cam_dist = (chase.base - focus).length();
    // Looking ahead of the car only makes sense from behind it; an orbit looks at the car itself.
    let look_ahead = (cam_dist * 0.35).clamp(0.5, 3.0) * chase.drag_yaw.cos().max(0.0);
    let look = car.pos + car.up * 0.8 + car.basis().0 * look_ahead;
    tf.translation = chase.base;
    tf.look_at(look, Vec3::Y);

    // Jolts: a hard landing by how fast the car came down, and the nitro's kick.
    if car.grounded && !chase.was_grounded {
        chase.trauma += ((chase.last_fall - 4.0) / 25.0).clamp(0.0, 0.8);
    }
    if car.just_used_nitro {
        chase.trauma += 0.45;
    }
    chase.trauma = (chase.trauma - TRAUMA_DECAY * dt).clamp(0.0, 1.0);
    chase.was_grounded = car.grounded;
    chase.last_fall = -car.vel.y;

    // The turn's sideways pull, yaw rate times speed; right turns are falling yaw.
    let yaw_rate = if dt > 0.0 {
        ((car.yaw - chase.last_yaw + PI).rem_euclid(TAU) - PI) / dt
    } else {
        0.0
    };
    chase.last_yaw = car.yaw;
    let pull = if car.grounded {
        yaw_rate * car.vel.dot(car.basis().0)
    } else {
        0.0
    };
    // The camera leans into the turn, its top toward the corner's inside.
    let tilt = (pull * TILT_PER_ACCEL).clamp(-TILT_MAX, TILT_MAX);
    chase.tilt += (tilt - chase.tilt) * smooth(dt, 4.0);

    if settings.camera_effects {
        chase.clock += dt;
        let t = chase.clock;
        // A fine road buzz near top speed, and a slower heavier shake for a jolt.
        let road = if car.grounded {
            ((speed_frac - SHAKE_FROM) / (1.0 - SHAKE_FROM)).clamp(0.0, 1.0) * 0.35
        } else {
            0.0
        };
        let jolt = chase.trauma * chase.trauma;
        let (right, up) = (*tf.right(), *tf.up());
        let offset = right * (wobble(t, 17.0, 0.0) * road + wobble(t, 7.0, 3.1) * jolt)
            + up * (wobble(t, 19.0, 1.3) * road + wobble(t, 8.0, 0.4) * jolt);
        tf.translation += offset * SHAKE_POS;
        let roll = wobble(t, 6.0, 2.2) * (road * 0.5 + jolt) * SHAKE_ROLL;
        tf.rotate_local_z(chase.tilt + roll);
    }

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
                crate::trial::TrialState::Replay => {
                    window.title = format!(
                        "benilla lambo — [{}] REPLAY — Space pause, arrows skip/speed, ESC menu",
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
