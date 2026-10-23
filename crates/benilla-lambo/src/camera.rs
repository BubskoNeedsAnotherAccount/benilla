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

const DISTANCE: f32 = 8.5;
const HEIGHT: f32 = 3.0;
const BASE_FOV: f32 = 1.05;

/// The camera's own heading, which trails the car's.
#[derive(Component)]
pub struct ChaseCam {
    yaw: f32,
    snapped: bool,
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
        Transform::from_translation(wow_to_bevy([-437.0, -2596.0, 400.0]) + Vec3::Y * 20.0),
        ChaseCam {
            yaw: 0.0,
            snapped: false,
        },
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
    let mut want = car.pos + back * DISTANCE + Vec3::Y * (HEIGHT + speed * 0.01);
    // Stay above the ground behind the car, a hill included.
    if let Some(hit) = world.ray_body(want + Vec3::Y * 40.0, Dir3::NEG_Y, 80.0) {
        want.y = want.y.max(want.y + 40.0 - hit.distance + 1.2);
    }
    tf.translation = if chase.snapped {
        tf.translation.lerp(want, smooth(dt, 10.0))
    } else {
        want
    };
    chase.snapped = true;
    let look = car.pos + car.up + car.basis().0 * 3.0;
    tf.look_at(look, Vec3::Y);

    if let Projection::Perspective(p) = &mut *proj {
        let fov = BASE_FOV + (speed / TOP_SPEED).clamp(0.0, 1.0) * 0.25;
        p.fov += (fov - p.fov) * smooth(dt, 3.0);
    }
}

/// Shows the speed in the window title, five times a second.
pub fn speedometer(
    time: Res<Time>,
    car: Query<&Car>,
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
    window.title = format!("benilla lambo — {kmh:.0} km/h — WASD drive, Space handbrake, Shift boost, C look back, R reset");
}
