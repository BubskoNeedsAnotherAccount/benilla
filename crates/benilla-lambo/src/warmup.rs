//! Pipeline warm-up: draws one sub-pixel triangle per model-material variant in front of the
//! camera at startup, so the shader and pipeline compile before the drive and not the frame a
//! model first shows.
//!
//! A browser compiles a pipeline on the main thread in the frame a mesh with a new key first
//! draws: a skinned M2, a blended glow card or a multiply batch cost 60 to 160 ms the first time a
//! course shows one. A pipeline is keyed by the mesh's vertex layout (skin joints, vertex colours),
//! the alpha mode, the cull mode and `WowModelKey`; the variants below are the ones a drive meets.
//! They go in a few per frame, so the compile is spread across the first second.

use std::collections::{HashMap, VecDeque};

use benilla_assets::materials::{WowModelExt, WowModelMaterial};
use benilla_assets::{ATTRIBUTE_WOW_JOINT_INDEX, ATTRIBUTE_WOW_JOINT_WEIGHT};
use benilla_world::lighting::SharedLightBuffer;
use benilla_world::view::WorldCamera;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshTag, PrimitiveTopology, VertexAttributeValues};
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::render::render_resource::Face;

/// Variants put on screen each frame.
const PER_FRAME: usize = 2;
/// Frames a variant stays up: the render world specializes it a frame behind the main world.
const LIFETIME: u8 = 3;
/// Yards in front of the camera.
const DISTANCE: f32 = 3.0;
/// A triangle's legs in yards; under a pixel at that distance.
const SIZE: f32 = 0.0005;

/// `clutter_fade.z` marker bits `WowModelKey` reads.
const NO_DEPTH_WRITE: u16 = 1;
const ADDITIVE: u16 = 1 << 2;
const MODULATE: u16 = 1 << 7;
const MODULATE_2X: u16 = 1 << 8;

#[derive(Clone, Copy)]
pub struct Variant {
    alpha: AlphaMode,
    markers: u16,
    fade: bool,
    clutter: bool,
    two_sided: bool,
}

/// A variant on a vertex layout: skin joints, vertex colours.
pub type Planned = (Variant, bool, bool);

#[derive(Component)]
pub struct WarmupProp {
    left: u8,
}

/// What a drive draws: opaque and cutout batches in both cull modes, ground clutter, and the
/// blended kinds (plain, doodad fade, additive glow, the multiply blends), each with depth write
/// off or on, on every layout a model comes in.
fn plan() -> VecDeque<Planned> {
    let v = |alpha, markers, fade, clutter, two_sided| Variant {
        alpha,
        markers,
        fade,
        clutter,
        two_sided,
    };
    let blend = AlphaMode::Blend;
    let mask = AlphaMode::Mask(0.5);
    let variants = [
        v(AlphaMode::Opaque, 0, false, false, false),
        v(AlphaMode::Opaque, 0, false, false, true),
        v(mask, 0, false, false, false),
        v(mask, 0, false, false, true),
        v(mask, 0, false, true, true),
        v(blend, 0, false, false, false),
        v(blend, 0, false, false, true),
        v(blend, NO_DEPTH_WRITE, false, false, false),
        v(blend, NO_DEPTH_WRITE, false, false, true),
        v(blend, 0, true, false, false),
        v(blend, 0, true, false, true),
        v(blend, ADDITIVE | NO_DEPTH_WRITE, false, false, false),
        v(blend, ADDITIVE | NO_DEPTH_WRITE, false, false, true),
        v(blend, MODULATE | NO_DEPTH_WRITE, false, false, false),
        v(blend, MODULATE_2X | NO_DEPTH_WRITE, false, false, false),
    ];
    let mut out = VecDeque::new();
    for variant in variants {
        for skin in [false, true] {
            for colors in [false, true] {
                out.push_back((variant, skin, colors));
            }
        }
    }
    out
}

/// One triangle on the layout `skin`/`colors` name.
fn triangle(skin: bool, colors: bool) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[0.0, 0.0, 0.0], [SIZE, 0.0, 0.0], [0.0, SIZE, 0.0]],
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; 3]);
    if colors {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32; 4]; 3]);
    }
    if skin {
        mesh.insert_attribute(
            ATTRIBUTE_WOW_JOINT_INDEX,
            VertexAttributeValues::Uint16x4(vec![[0; 4]; 3]),
        );
        mesh.insert_attribute(
            ATTRIBUTE_WOW_JOINT_WEIGHT,
            VertexAttributeValues::Float32x4(vec![[1.0, 0.0, 0.0, 0.0]; 3]),
        );
    }
    mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
    mesh
}

fn material(v: Variant, light: &SharedLightBuffer) -> WowModelMaterial {
    ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            alpha_mode: v.alpha,
            double_sided: v.two_sided,
            cull_mode: if v.two_sided { None } else { Some(Face::Back) },
            ..default()
        },
        extension: WowModelExt {
            clutter_fade: Vec4::new(
                0.0,
                0.0,
                f32::from(v.markers),
                f32::from(u8::from(v.clutter)),
            ),
            model_flags: Vec4::new(0.0, f32::from(u8::from(v.fade)), 0.0, 0.0),
            sun_scale: Vec4::ZERO,
            tint: Vec4::new(1.0, 1.0, 1.0, 0.0),
            sidn: Vec4::ZERO,
            anim_slots: Vec4::ZERO,
            stage1: Vec4::ZERO,
            stage1_texture: None,
            light_buf: light.0.clone(),
        },
    }
}

/// Puts the next variants in front of the camera, keeps the ones up with it, and takes them down
/// after [`LIFETIME`] frames.
pub fn warm_pipelines(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    light: Option<Res<SharedLightBuffer>>,
    camera: Query<&GlobalTransform, With<WorldCamera>>,
    mut props: Query<(Entity, &mut Transform, &mut WarmupProp)>,
    mut todo: Local<Option<VecDeque<Planned>>>,
    mut layouts: Local<HashMap<(bool, bool), Handle<Mesh>>>,
) {
    let todo = todo.get_or_insert_with(plan);
    if todo.is_empty() && props.is_empty() {
        layouts.clear();
        return;
    }
    let Ok(cam) = camera.single() else { return };
    let at = cam.translation() + cam.forward().as_vec3() * DISTANCE;
    for (entity, mut transform, mut prop) in &mut props {
        transform.translation = at;
        prop.left -= 1;
        if prop.left == 0 {
            commands.entity(entity).despawn();
        }
    }
    let Some(light) = light else { return };
    for _ in 0..PER_FRAME {
        let Some((variant, skin, colors)) = todo.pop_front() else {
            break;
        };
        let mesh = layouts
            .entry((skin, colors))
            .or_insert_with(|| meshes.add(triangle(skin, colors)))
            .clone();
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(materials.add(material(variant, &light))),
            Transform::from_translation(at),
            MeshTag(0),
            WarmupProp { left: LIFETIME },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_covers_every_layout_of_every_variant() {
        let plan = plan();
        let layouts: std::collections::HashSet<_> = plan.iter().map(|p| (p.1, p.2)).collect();
        assert_eq!(layouts.len(), 4);
        assert_eq!(plan.len() % 4, 0);
    }
}
