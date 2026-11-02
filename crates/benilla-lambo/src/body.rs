//! The body's weight and its brake lights. The body (not the wheels) pitches back under throttle and
//! dives under the brakes, rolls out of a turn and squats on a landing, on springs driven by the
//! car's own acceleration. The tail lights glow, and brighten with a halo under the brakes.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::car::Car;

/// The point in car space the body leans about: low, near the axles' height.
pub const LEAN_PIVOT: Vec3 = Vec3::new(0.0, -0.2, 0.0);
/// Lean per yd/s² of acceleration, radians, and the most it leans each way.
const PITCH_PER_ACCEL: f32 = 0.0010;
const PITCH_DIVE: f32 = 0.045;
const PITCH_SQUAT: f32 = 0.035;
const ROLL_PER_ACCEL: f32 = 0.0011;
const ROLL_MAX: f32 = 0.06;
/// The lean spring and damping: settles in about half a second with a little overshoot.
const LEAN_K: f32 = 110.0;
const LEAN_C: f32 = 13.0;
/// The landing squat: spring, damping, how hard a fall kicks it and how far it may sink, yards.
const SINK_K: f32 = 260.0;
const SINK_C: f32 = 16.0;
const SINK_PER_FALL: f32 = 0.03;
const SINK_MAX: f32 = 0.18;

/// The lean's springs, on the local car's body pivot.
#[derive(Component, Default)]
pub struct BodyLean {
    pitch: f32,
    pitch_v: f32,
    roll: f32,
    roll_v: f32,
    sink: f32,
    sink_v: f32,
    last_vel: Vec3,
    was_grounded: bool,
    primed: bool,
}

/// One tail light of the local car: the light itself, or its halo, shown only under the brakes.
#[derive(Component)]
pub struct BrakeLight {
    halo: bool,
}

/// The tail lights in model space (metres, +Z front, +X left): centre and size. The centre strip
/// is where the model's rear-facing red texels are; the two side lamps sit on the rear corners.
/// Each is set a few centimetres behind the body so the body never hides it.
const LIGHTS: [(Vec3, Vec2); 3] = [
    (Vec3::new(0.0, 1.05, -2.61), Vec2::new(0.62, 0.07)),
    (Vec3::new(0.80, 1.02, -2.42), Vec2::new(0.22, 0.08)),
    (Vec3::new(-0.80, 1.02, -2.42), Vec2::new(0.22, 0.08)),
];
/// The halo's size over its light's.
const HALO_SCALE: Vec2 = Vec2::new(2.2, 5.0);

/// The lights' mesh and paints, shared by every car.
#[derive(Clone)]
pub struct LightAssets {
    glow: Handle<Mesh>,
    dim: Handle<StandardMaterial>,
    lit: Handle<StandardMaterial>,
    halo: Handle<StandardMaterial>,
}

impl LightAssets {
    pub fn new(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> Self {
        // Added light: the colour is the brightness, HDR past 1 for the lit lamp.
        let mut glow = |r: f32, g: f32, b: f32| {
            materials.add(StandardMaterial {
                base_color: Color::linear_rgb(r, g, b),
                alpha_mode: AlphaMode::Add,
                unlit: true,
                cull_mode: None,
                ..default()
            })
        };
        Self {
            glow: meshes.add(glow_mesh()),
            dim: glow(0.35, 0.02, 0.02),
            lit: glow(3.0, 0.18, 0.10),
            halo: glow(0.55, 0.03, 0.02),
        }
    }
}

/// A unit disc facing ±Z, white across its inner 60% and fading to black at the rim: under
/// additive blending a soft-edged lamp.
fn glow_mesh() -> Mesh {
    const RIM: usize = 16;
    let mut pos = vec![[0.0, 0.0, 0.0]];
    let mut col = vec![[1.0, 1.0, 1.0, 1.0]];
    for ring in [0.3, 0.5] {
        let c = if ring < 0.4 { 1.0 } else { 0.0 };
        for i in 0..RIM {
            let a = i as f32 / RIM as f32 * std::f32::consts::TAU;
            pos.push([a.cos() * ring, a.sin() * ring, 0.0]);
            col.push([c, c, c, 1.0]);
        }
    }
    let mut idx = Vec::new();
    for i in 0..RIM as u32 {
        let j = (i + 1) % RIM as u32;
        idx.extend([0, 1 + i, 1 + j]);
        let (a, b, c, d) = (1 + i, 1 + j, 1 + RIM as u32 + i, 1 + RIM as u32 + j);
        idx.extend([a, c, b, b, c, d]);
    }
    let n = pos.len();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, -1.0]; n])
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
    .with_inserted_indices(Indices::U32(idx))
}

/// Spawns the tail lights under a model's body (model space): the lamps, then their halos, hidden.
pub fn spawn_lights(
    commands: &mut Commands,
    assets: &LightAssets,
    body: Entity,
) -> ([Entity; 3], [Entity; 3]) {
    let lamp = |commands: &mut Commands, at: Vec3, size: Vec2, halo: bool| {
        let (material, scale, vis, at) = if halo {
            let back = Vec3::new(0.0, 0.0, -0.02);
            (
                &assets.halo,
                size * HALO_SCALE,
                Visibility::Hidden,
                at + back,
            )
        } else {
            (&assets.dim, size, Visibility::Inherited, at)
        };
        commands
            .spawn((
                Mesh3d(assets.glow.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(at).with_scale(scale.extend(1.0)),
                vis,
                ChildOf(body),
            ))
            .id()
    };
    (
        LIGHTS.map(|(at, size)| lamp(commands, at, size, false)),
        LIGHTS.map(|(at, size)| lamp(commands, at, size, true)),
    )
}

/// Marks the local car's lamps and halos for `brake_lights`.
pub fn mark_local_lights(commands: &mut Commands, lamps: [Entity; 3], halos: [Entity; 3]) {
    for e in lamps {
        commands.entity(e).insert(BrakeLight { halo: false });
    }
    for e in halos {
        commands.entity(e).insert(BrakeLight { halo: true });
    }
}

/// Springs the body toward the lean the car's acceleration asks for, and squats it on a landing.
pub fn lean_body(
    time: Res<Time>,
    car: Query<&Car>,
    mut body: Query<(&mut BodyLean, &mut Transform)>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    let (Ok(car), Ok((mut lean, mut tf))) = (car.single(), body.single_mut()) else {
        return;
    };
    if dt <= 0.0 {
        return;
    }
    if !car.placed || !lean.primed {
        *lean = BodyLean {
            last_vel: car.vel,
            was_grounded: car.grounded,
            primed: car.placed,
            ..default()
        };
        tf.translation = LEAN_PIVOT;
        tf.rotation = Quat::IDENTITY;
        return;
    }
    let accel = (car.vel - lean.last_vel) / dt;
    let fall = -lean.last_vel.y;
    lean.last_vel = car.vel;
    let (fwd, right) = car.basis();
    // Accelerating lifts the nose (+X turns −Z up); a pull to the right lifts the right side.
    let (pitch, roll) = if car.grounded {
        (
            (accel.dot(fwd) * PITCH_PER_ACCEL).clamp(-PITCH_DIVE, PITCH_SQUAT),
            (accel.dot(right) * ROLL_PER_ACCEL).clamp(-ROLL_MAX, ROLL_MAX),
        )
    } else {
        (0.0, 0.0)
    };
    if car.grounded && !lean.was_grounded {
        lean.sink_v -= (fall * SINK_PER_FALL).clamp(0.0, 1.2);
    }
    lean.was_grounded = car.grounded;

    lean.pitch_v += (LEAN_K * (pitch - lean.pitch) - LEAN_C * lean.pitch_v) * dt;
    lean.pitch += lean.pitch_v * dt;
    lean.roll_v += (LEAN_K * (roll - lean.roll) - LEAN_C * lean.roll_v) * dt;
    lean.roll += lean.roll_v * dt;
    lean.sink_v += (-SINK_K * lean.sink - SINK_C * lean.sink_v) * dt;
    lean.sink = (lean.sink + lean.sink_v * dt).clamp(-SINK_MAX, SINK_MAX);

    tf.translation = LEAN_PIVOT + Vec3::Y * lean.sink;
    tf.rotation = Quat::from_rotation_x(lean.pitch) * Quat::from_rotation_z(lean.roll);
}

/// Brightens the local car's tail lights and shows their halos while it brakes.
pub fn brake_lights(
    car: Query<&Car>,
    assets: Option<Res<crate::car::CarAssets>>,
    mut lights: Query<(
        &BrakeLight,
        &mut MeshMaterial3d<StandardMaterial>,
        &mut Visibility,
    )>,
    mut shown: Local<Option<bool>>,
) {
    let (Ok(car), Some(assets)) = (car.single(), assets) else {
        return;
    };
    let forward = car.vel.dot(car.basis().0);
    let on = car.placed
        && (car.handbrake_input || car.burnout || (car.brake_input > 0.0 && forward > 0.5));
    if *shown == Some(on) {
        return;
    }
    *shown = Some(on);
    let paints = assets.lights();
    for (light, mut material, mut vis) in &mut lights {
        if light.halo {
            *vis = if on {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        } else {
            material.0 = if on { &paints.lit } else { &paints.dim }.clone();
        }
    }
}
