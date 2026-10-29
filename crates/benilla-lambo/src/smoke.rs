use bevy::prelude::*;

use crate::car::{Car, RIDE};
use crate::model::{SMOKE_X, SMOKE_Z};

/// A single expanding, fading tire smoke puff.
#[derive(Component)]
pub struct TireSmokeParticle {
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub vel: Vec3,
    pub initial_scale: f32,
    pub max_scale: f32,
}

/// Pre-allocated mesh and tiered alpha materials for zero-allocation runtime smoke.
#[derive(Resource)]
pub struct TireSmokeAssets {
    pub mesh: Handle<Mesh>,
    pub materials: [Handle<StandardMaterial>; 8],
    pub spawn_timer: f32,
    rng_state: u64,
}

impl TireSmokeAssets {
    fn next_f32(&mut self) -> f32 {
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        (self.rng_state as f32) / (u64::MAX as f32)
    }
}

pub fn setup_tire_smoke(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(Sphere::new(0.35));

    // 8 stages of fading alpha from dense puff to transparent dissipation
    const ALPHAS: [f32; 8] = [0.45, 0.38, 0.30, 0.22, 0.15, 0.09, 0.04, 0.01];
    let smoke_mats = ALPHAS.map(|a| {
        materials.add(StandardMaterial {
            base_color: Color::srgba(0.92, 0.90, 0.88, a),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            cull_mode: None,
            ..default()
        })
    });

    commands.insert_resource(TireSmokeAssets {
        mesh,
        materials: smoke_mats,
        spawn_timer: 0.0,
        rng_state: 0x853c49e6748fea9b,
    });
}

/// How hard a car is smoking its tyres: 0 not at all, 1 drifting or braking hard (rear tyres), 2 a violent
/// drift or a fast handbrake turn (all four), 3 a burnout.
pub fn smoke_level(car: &Car) -> u8 {
    // Strictly no tire smoke while airborne or unplaced
    if !car.placed || !car.grounded {
        return 0;
    }
    let speed = car.speed();
    if car.burnout {
        3
    } else if car.lateral_speed > 6.5 || (car.handbrake_input && speed > 16.0) {
        2
    } else if car.lateral_speed > 3.2
        || (car.handbrake_input && speed > 5.0)
        || (car.brake_input > 0.0 && speed > 14.0)
    {
        1
    } else {
        0
    }
}

/// Seconds between puffs at a smoke level.
pub fn smoke_interval(level: u8) -> f32 {
    match level {
        3 => 0.024,
        2 => 0.028,
        _ => 0.038,
    }
}

pub fn spawn_tire_smoke(
    mut commands: Commands,
    time: Res<Time>,
    mut smoke_assets: ResMut<TireSmokeAssets>,
    car_q: Query<(&Car, &Transform)>,
) {
    let Ok((car, car_tf)) = car_q.single() else {
        return;
    };
    let level = smoke_level(car);
    if level == 0 {
        return;
    }

    smoke_assets.spawn_timer -= time.delta_secs();
    if smoke_assets.spawn_timer > 0.0 {
        return;
    }
    smoke_assets.spawn_timer = smoke_interval(level);
    puff(
        &mut commands,
        &mut smoke_assets,
        car_tf.translation,
        car_tf.rotation,
        car.vel,
        level,
    );
}

/// One round of puffs from a car at `pos` facing `rot`, moving at `vel`: both rear tyres, and the
/// front ones at level 2 and up.
pub fn puff(
    commands: &mut Commands,
    smoke_assets: &mut TireSmokeAssets,
    pos: Vec3,
    rot: Quat,
    vel: Vec3,
    level: u8,
) {
    let (fwd, right, up) = (rot * Vec3::NEG_Z, rot * Vec3::X, rot * Vec3::Y);
    let ground = -(RIDE - 0.05);
    let mut wheel_offsets = vec![
        Vec3::new(-SMOKE_X, ground, SMOKE_Z),
        Vec3::new(SMOKE_X, ground, SMOKE_Z),
    ];
    if level >= 2 {
        wheel_offsets.push(Vec3::new(-SMOKE_X, ground, -SMOKE_Z));
        wheel_offsets.push(Vec3::new(SMOKE_X, ground, -SMOKE_Z));
    }

    let mesh_handle = smoke_assets.mesh.clone();
    let initial_mat = smoke_assets.materials[0].clone();

    for offset in wheel_offsets {
        let r1 = smoke_assets.next_f32();
        let r2 = smoke_assets.next_f32();
        let r3 = smoke_assets.next_f32();

        let jitter = right * ((r1 - 0.5) * 0.35) + fwd * ((r2 - 0.5) * 0.35);
        let spawn_pos = pos + rot * offset + jitter;

        let outward = if offset.x < 0.0 { -right } else { right };
        let puff_vel = vel * 0.12
            + up * (0.8 + r2 * 0.8)
            + outward * (0.4 + r3 * 0.6)
            + fwd * ((r1 - 0.5) * 0.4);

        let initial_scale = 0.35 + r1 * 0.2;
        let max_scale = if level == 3 {
            2.2 + r2 * 0.8
        } else {
            1.6 + r2 * 0.7
        };
        let max_lifetime = 0.55 + r3 * 0.35;

        commands.spawn((
            TireSmokeParticle {
                lifetime: 0.0,
                max_lifetime,
                vel: puff_vel,
                initial_scale,
                max_scale,
            },
            Mesh3d(mesh_handle.clone()),
            MeshMaterial3d(initial_mat.clone()),
            Transform::from_translation(spawn_pos).with_scale(Vec3::splat(initial_scale)),
            Visibility::default(),
        ));
    }
}
pub fn update_tire_smoke(
    mut commands: Commands,
    time: Res<Time>,
    smoke_assets: Option<Res<TireSmokeAssets>>,
    mut particles: Query<(
        Entity,
        &mut Transform,
        &mut MeshMaterial3d<StandardMaterial>,
        &mut TireSmokeParticle,
    )>,
) {
    let Some(smoke_assets) = smoke_assets else {
        return;
    };
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    for (entity, mut tf, mut mat, mut particle) in particles.iter_mut() {
        particle.lifetime += dt;
        if particle.lifetime >= particle.max_lifetime {
            commands.entity(entity).despawn();
            continue;
        }

        let progress = (particle.lifetime / particle.max_lifetime).clamp(0.0, 1.0);

        tf.translation += particle.vel * dt;
        particle.vel.y += 0.35 * dt;
        particle.vel *= (-1.1 * dt).exp();

        let current_scale =
            particle.initial_scale + progress * (particle.max_scale - particle.initial_scale);
        tf.scale = Vec3::splat(current_scale);

        let stage = ((progress * 7.99) as usize).min(7);
        mat.0 = smoke_assets.materials[stage].clone();
    }
}
