//! Speed streaks: faint lines of air rushing past the edges of the view near top speed and under
//! nitro. A fixed set of thin additive strips rides on the camera, flying from far ahead toward it
//! at the car's speed, out at the edges of the frame so the car and the road ahead stay clear.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::camera::ChaseCam;
use crate::car::{Car, TOP_SPEED};

const COUNT: usize = 40;
/// The share of top speed streaks start to show at, and reach full strength.
const FROM: f32 = 0.72;
const FULL: f32 = 0.95;
/// Where a streak starts and ends, yards ahead of the camera.
const FAR: f32 = 40.0;
const NEAR: f32 = 1.5;
/// How far off the view's centre a streak flies, as the tangent of its angle there.
const SPREAD: (f32, f32) = (0.30, 0.62);
/// A streak's width and length at full strength, yards.
const WIDTH: f32 = 0.025;
const LENGTH: f32 = 3.5;

#[derive(Component)]
pub struct Streak {
    /// Its direction around the view's centre, and how far out (a tangent).
    angle: f32,
    spread: f32,
    /// Yards ahead of the camera (negative camera Z).
    depth: f32,
    /// Its own share of the car's speed, so the streaks do not move in step.
    pace: f32,
}

#[derive(Resource)]
pub struct Streaks {
    strength: f32,
    shown: bool,
    rng: u32,
}

impl Streaks {
    fn next_f32(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }

    fn reroll(&mut self, s: &mut Streak, depth: f32) {
        s.angle = self.next_f32() * std::f32::consts::TAU;
        s.spread = SPREAD.0 + (SPREAD.1 - SPREAD.0) * self.next_f32();
        s.pace = 0.8 + 0.5 * self.next_f32();
        s.depth = depth;
    }
}

/// A unit strip in the XZ plane, along Z, bright in its middle and dark at both ends.
fn strip_mesh() -> Mesh {
    let mut pos = Vec::new();
    let mut col = Vec::new();
    for (z, c) in [(-0.5, 0.0), (0.0, 1.0), (0.5, 0.0)] {
        for x in [-0.5, 0.5] {
            pos.push([x, 0.0, z]);
            col.push([c, c, c, 1.0]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 6])
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
    .with_inserted_indices(Indices::U32(vec![0, 2, 1, 1, 2, 3, 2, 4, 3, 3, 4, 5]))
}

pub fn setup_streaks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    cam: Query<Entity, With<ChaseCam>>,
) {
    let Ok(cam) = cam.single() else { return };
    let mesh = meshes.add(strip_mesh());
    let material = materials.add(StandardMaterial {
        base_color: Color::linear_rgb(0.30, 0.32, 0.36),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        cull_mode: None,
        ..default()
    });
    let mut streaks = Streaks {
        strength: 0.0,
        shown: false,
        rng: 0x1B87_3593,
    };
    for i in 0..COUNT {
        let mut s = Streak {
            angle: 0.0,
            spread: 0.0,
            depth: 0.0,
            pace: 1.0,
        };
        let depth = NEAR + (FAR - NEAR) * (i as f32 + 0.5) / COUNT as f32;
        streaks.reroll(&mut s, depth);
        commands.spawn((
            s,
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::default(),
            Visibility::Hidden,
            ChildOf(cam),
        ));
    }
    commands.insert_resource(streaks);
}

/// Flies the streaks toward the camera at the car's speed, and fades them with it.
pub fn fly_streaks(
    time: Res<Time>,
    car: Query<&Car>,
    streaks: Option<ResMut<Streaks>>,
    mut lines: Query<(&mut Streak, &mut Transform, &mut Visibility)>,
) {
    let (Ok(car), Some(mut streaks)) = (car.single(), streaks) else {
        return;
    };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let speed = car.speed();
    let fast = ((speed / TOP_SPEED - FROM) / (FULL - FROM)).clamp(0.0, 1.0);
    let target = if car.placed {
        fast.max(car.nitro_timer.min(1.0))
    } else {
        0.0
    };
    streaks.strength += (target - streaks.strength) * crate::car::smooth(dt, 4.0);
    let strength = streaks.strength;
    if strength < 0.02 {
        if streaks.shown {
            streaks.shown = false;
            for (_, _, mut vis) in &mut lines {
                *vis = Visibility::Hidden;
            }
        }
        return;
    }
    streaks.shown = true;
    // Nitro streaks fly even when the car is slower.
    let rush = speed.max(TOP_SPEED * 0.8);
    for (mut s, mut tf, mut vis) in &mut lines {
        s.depth -= rush * s.pace * dt;
        if s.depth < NEAR {
            let depth = FAR - streaks.next_f32() * 6.0;
            streaks.reroll(&mut s, depth);
        }
        let (sin, cos) = s.angle.sin_cos();
        // Wider than tall, as the frame is.
        let out = Vec2::new(cos * 1.5, sin) * s.spread * s.depth;
        tf.translation = Vec3::new(out.x, out.y, -s.depth);
        // The strip's normal points out from the view's centre, its width across it.
        tf.rotation = Quat::from_rotation_z(out.y.atan2(out.x) - std::f32::consts::FRAC_PI_2);
        tf.scale = Vec3::new(WIDTH * strength, 1.0, LENGTH * strength);
        *vis = Visibility::Inherited;
    }
}
