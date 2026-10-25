use std::f32::consts::TAU;

use benilla_world::collision::WorldCollision;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::car::Car;

#[derive(Component)]
pub struct CarShadow;

#[derive(Resource)]
pub struct CarShadowAssets {
    pub mesh: Handle<Mesh>,
    pub materials: [Handle<StandardMaterial>; 8],
}

/// Builds a soft-edged radial oval mesh for ambient occlusion contact shadow.
/// Center has high alpha, middle ring has medium alpha, and outer perimeter fades to 0.
fn build_car_shadow_mesh() -> Mesh {
    const SEGMENTS: usize = 32;
    const HX: f32 = 2.15; // Half width (2.3 yards total)
    const HZ: f32 = 4.40; // Half length (4.8 yards total)

    let mut positions = Vec::with_capacity(1 + SEGMENTS * 2);
    let mut normals = Vec::with_capacity(1 + SEGMENTS * 2);
    let mut uvs = Vec::with_capacity(1 + SEGMENTS * 2);
    let mut colors = Vec::with_capacity(1 + SEGMENTS * 2);

    // Center vertex (index 0)
    positions.push([0.0, 0.0, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    colors.push([0.015, 0.015, 0.02, 0.68]);

    // Inner ring (indices 1..=32)
    for i in 0..SEGMENTS {
        let theta = TAU * (i as f32) / (SEGMENTS as f32);
        let x = 0.58 * HX * theta.cos();
        let z = 0.58 * HZ * theta.sin();
        positions.push([x, 0.0, z]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push([x / (2.0 * HX) + 0.5, z / (2.0 * HZ) + 0.5]);
        colors.push([0.015, 0.015, 0.02, 0.45]);
    }

    // Outer perimeter ring (indices 33..=64) - 0 alpha for soft feathered falloff
    for i in 0..SEGMENTS {
        let theta = TAU * (i as f32) / (SEGMENTS as f32);
        let x = HX * theta.cos();
        let z = HZ * theta.sin();
        positions.push([x, 0.0, z]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push([x / (2.0 * HX) + 0.5, z / (2.0 * HZ) + 0.5]);
        colors.push([0.015, 0.015, 0.02, 0.0]);
    }

    let mut indices = Vec::with_capacity(SEGMENTS * 3 + SEGMENTS * 6);

    // Center to inner ring
    for i in 0..SEGMENTS {
        let next = (i + 1) % SEGMENTS;
        indices.push(0u32);
        indices.push((1 + i) as u32);
        indices.push((1 + next) as u32);
    }

    // Inner ring to outer ring
    for i in 0..SEGMENTS {
        let next = (i + 1) % SEGMENTS;
        let inner_curr = (1 + i) as u32;
        let inner_next = (1 + next) as u32;
        let outer_curr = (1 + SEGMENTS + i) as u32;
        let outer_next = (1 + SEGMENTS + next) as u32;

        indices.push(inner_curr);
        indices.push(outer_curr);
        indices.push(inner_next);

        indices.push(inner_next);
        indices.push(outer_curr);
        indices.push(outer_next);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

pub fn setup_car_shadow(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(build_car_shadow_mesh());

    const FADES: [f32; 8] = [1.0, 0.85, 0.70, 0.55, 0.40, 0.25, 0.12, 0.0];
    let shadow_mats = FADES.map(|f| {
        materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 1.0, 1.0, f),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            cull_mode: None,
            depth_bias: 0.2,
            ..default()
        })
    });

    let initial_mat = shadow_mats[0].clone();

    commands.insert_resource(CarShadowAssets {
        mesh: mesh.clone(),
        materials: shadow_mats,
    });

    commands.spawn((
        CarShadow,
        Mesh3d(mesh),
        MeshMaterial3d(initial_mat),
        Transform::from_translation(Vec3::new(0.0, -9999.0, 0.0)),
        Visibility::Hidden,
    ));
}

pub fn update_car_shadow(
    world: WorldCollision,
    car_q: Query<(&Car, &Transform)>,
    mut shadow_q: Query<
        (
            &mut Transform,
            &mut MeshMaterial3d<StandardMaterial>,
            &mut Visibility,
        ),
        (With<CarShadow>, Without<Car>),
    >,
    shadow_assets: Option<Res<CarShadowAssets>>,
) {
    let (Ok((car, _car_tf)), Ok((mut shadow_tf, mut shadow_mat, mut shadow_vis))) =
        (car_q.single(), shadow_q.single_mut())
    else {
        return;
    };
    let Some(assets) = shadow_assets else {
        return;
    };

    if !car.placed {
        *shadow_vis = Visibility::Hidden;
        return;
    }

    // Downward raycast from car body to probe terrain beneath it
    let probe_origin = car.pos + Vec3::Y * 1.5;
    let Some(hit) = world.ray_body(probe_origin, Dir3::NEG_Y, 70.0) else {
        *shadow_vis = Visibility::Hidden;
        return;
    };

    let ground_point = probe_origin + Dir3::NEG_Y.as_vec3() * hit.distance;
    let altitude = (car.pos.y - ground_point.y).max(0.0);

    // Hide if soaring too high into the sky (> 35 yards)
    if altitude > 35.0 {
        *shadow_vis = Visibility::Hidden;
        return;
    }

    *shadow_vis = Visibility::Visible;

    // Normal on ground plane
    let ground_norm = if hit.normal.y > 0.15 {
        hit.normal.normalize_or(Vec3::Y)
    } else {
        car.up
    };

    // Sit slightly above ground plane to prevent z-fighting with terrain polygons
    shadow_tf.translation = ground_point + ground_norm * 0.035;

    // Orient shadow to ground normal and car heading
    let (fwd, _) = car.basis();
    let flat_fwd = (fwd - ground_norm * fwd.dot(ground_norm)).normalize_or(fwd);
    let shadow_right = flat_fwd.cross(ground_norm).normalize_or(Vec3::X);
    shadow_tf.rotation = Quat::from_mat3(&Mat3::from_cols(shadow_right, ground_norm, -flat_fwd));

    // Soft penumbra expansion as the car climbs into the air
    let penumbra = 1.0 + (altitude / 25.0).clamp(0.0, 0.6);
    shadow_tf.scale = Vec3::new(penumbra, 1.0, penumbra);

    // Fade out as altitude increases
    let fade = (1.0 - (altitude / 30.0)).clamp(0.0, 1.0);
    let stage = ((1.0 - fade) * 7.99) as usize;
    shadow_mat.0 = assets.materials[stage.min(7)].clone();
}
