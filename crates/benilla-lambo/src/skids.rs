//! Skid marks: dark tyre strips laid on the ground behind a sliding tyre. Each strip is projected
//! onto the terrain and WMO faces once, when it is laid, through the engine's ground-decal
//! projector (as the car's shadow is), so it hugs slopes and bumps; afterwards it only costs its
//! few cached triangles a frame. The oldest strips fade out and a fixed budget caps the total.

use std::collections::VecDeque;

use benilla_world::decal::{DecalFrame, WorldDecal};
use benilla_world::particles::buffer::{EffectVertex, WorldEffectDraw};
use benilla_world::sky_order::Rung;
use benilla_world::view::WorldCamera;
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::car::{Car, RIDE};
use crate::model::{SMOKE_X, SMOKE_Z};

/// A strip's width, yards (a 30 cm tyre), and the shortest strip laid.
const WIDTH: f32 = 0.38;
const STEP: f32 = 0.5;
/// Further than this between two frames is a teleport, not a slide.
const JUMP: f32 = 8.0;
/// Most strips kept, all tyres together, and when they fade: full until `FADE_FROM` seconds old,
/// gone at `LIFE`.
const BUDGET: usize = 900;
const FADE_FROM: f32 = 25.0;
const LIFE: f32 = 40.0;
/// How far above or below the strip's centre a face still takes it.
const HOLD: f32 = 0.45;
const FADE: f32 = 0.6;
/// The mark's colour, raw bytes, and its darkest cover.
const SHADE: [u8; 3] = [10, 10, 12];
const COVER: f32 = 0.55;
/// Texels across the tyre's width.
const TEX_W: usize = 32;

struct Strip {
    verts: Vec<EffectVertex>,
    born: f32,
}

#[derive(Resource)]
pub struct SkidMarks {
    texture: Handle<Image>,
    strips: VecDeque<Strip>,
    /// Each tyre's last laid point while it slides: front left, front right, rear left, rear right.
    last: [Option<Vec3>; 4],
    /// Vertex buffers of retired strips, reused.
    spare: Vec<Vec<EffectVertex>>,
    owner: Entity,
}

/// The tyre's print across its width: soft edges and a few tread grooves.
fn print_texture() -> Vec<u8> {
    let mut data = Vec::with_capacity(TEX_W * 4 * 4);
    for _ in 0..4 {
        for x in 0..TEX_W {
            let u = (x as f32 + 0.5) / TEX_W as f32;
            let edge = smoothstep(0.0, 0.18, u) * smoothstep(0.0, 0.18, 1.0 - u);
            let tread = 0.8 + 0.2 * (u * std::f32::consts::TAU * 5.0).cos();
            data.extend_from_slice(&SHADE);
            data.push((edge * tread * COVER * 255.0).round() as u8);
        }
    }
    data
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn setup_skid_marks(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut image = Image::new(
        Extent3d {
            width: TEX_W as u32,
            height: 4,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        print_texture(),
        // Raw bytes, as the shadow's: the multiply runs on them unconverted.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    let owner = commands.spawn(Name::new("skid-marks")).id();
    commands.insert_resource(SkidMarks {
        texture: images.add(image),
        strips: VecDeque::new(),
        last: [None; 4],
        spare: Vec::new(),
        owner,
    });
}

/// Which tyres are sliding: the rear pair in a slide or under the brakes, all four in a violent
/// slide. Unlike the smoke, plain speed leaves no mark.
fn sliding(car: &Car) -> [bool; 4] {
    if !car.placed || !car.grounded {
        return [false; 4];
    }
    let speed = car.speed();
    let rear = car.lateral_speed > 3.2
        || (car.handbrake_input && speed > 5.0)
        || (car.brake_input > 0.0 && speed > 14.0)
        || car.burnout;
    let front = car.lateral_speed > 6.5;
    [front, front, rear, rear]
}

/// Lays a strip behind every sliding tyre that has moved far enough since its last one.
pub fn lay_skid_marks(
    time: Res<Time>,
    decals: WorldDecal,
    car: Query<(&Car, &Transform)>,
    marks: Option<ResMut<SkidMarks>>,
) {
    let (Ok((car, tf)), Some(mut marks)) = (car.single(), marks) else {
        return;
    };
    let marks = &mut *marks;
    let now = time.elapsed_secs();
    while marks
        .strips
        .front()
        .is_some_and(|s| now - s.born > LIFE || marks.strips.len() > BUDGET)
    {
        if let Some(old) = marks.strips.pop_front() {
            marks.spare.push(old.verts);
        }
    }

    // Where each tyre meets the road, in the order of `sliding`; the car faces −Z.
    let tyres = [
        Vec3::new(-SMOKE_X, -RIDE, -SMOKE_Z),
        Vec3::new(SMOKE_X, -RIDE, -SMOKE_Z),
        Vec3::new(-SMOKE_X, -RIDE, SMOKE_Z),
        Vec3::new(SMOKE_X, -RIDE, SMOKE_Z),
    ];
    for (i, slides) in sliding(car).into_iter().enumerate() {
        if !slides {
            marks.last[i] = None;
            continue;
        }
        let at = tf.translation + tf.rotation * tyres[i];
        let Some(from) = marks.last[i] else {
            marks.last[i] = Some(at);
            continue;
        };
        let d = at - from;
        let len = Vec2::new(d.x, d.z).length();
        if len > JUMP {
            marks.last[i] = Some(at);
            continue;
        }
        if len < STEP {
            continue;
        }
        marks.last[i] = Some(at);
        // The box's long side along the tyre's travel: `in_frame` puts (dx, dz) on +z' at this angle.
        let (sin, cos) = d.x.atan2(d.z).sin_cos();
        let frame = DecalFrame {
            center: (from + at) * 0.5,
            sin,
            cos,
            min_x: -WIDTH * 0.5,
            max_x: WIDTH * 0.5,
            min_z: -len * 0.5,
            max_z: len * 0.5,
            min_y: -(HOLD + FADE + d.y.abs() * 0.5),
            max_y: HOLD + FADE + d.y.abs() * 0.5,
        };
        let mut verts = marks.spare.pop().unwrap_or_default();
        verts.clear();
        let slope = d.y / len.max(1e-3);
        let landed = decals.project(
            &mut verts,
            &frame,
            // Faces off the strip's own slope fade out: a wall or a ledge below takes none.
            |p| {
                let h = (p.y - p.z * slope).abs();
                (1.0 - (h - HOLD) / FADE).clamp(0.0, 1.0)
            },
            |x, z| frame.rect_uv(x, z),
        );
        if landed {
            marks.strips.push_back(Strip { verts, born: now });
        } else {
            marks.spare.push(verts);
        }
    }
}

/// Copies the marks onto the effect stream, the old ones fading: a multiply on the shadow's rung.
pub fn push_skid_marks(
    time: Res<Time>,
    marks: Option<Res<SkidMarks>>,
    car: Query<&Car>,
    cam: Query<Entity, With<WorldCamera>>,
    mut draw: WorldEffectDraw,
) {
    let (Some(marks), Ok(cam)) = (marks, cam.single()) else {
        return;
    };
    if marks.strips.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    let anchor = car.single().map_or(Vec3::ZERO, |c| c.pos);
    let mut batch = draw
        .batch(cam, marks.texture.id())
        .multiply()
        .anchored(anchor)
        .rung(Rung::SHADOW_SORT, Rung::DECAL_RASTER)
        .owner(marks.owner);
    let out = batch.verts_mut();
    for strip in &marks.strips {
        let fade = (1.0 - (now - strip.born - FADE_FROM) / (LIFE - FADE_FROM)).clamp(0.0, 1.0);
        if fade <= 0.0 {
            continue;
        }
        out.extend(strip.verts.iter().map(|v| EffectVertex {
            color: [1.0, 1.0, 1.0, v.color[3] * fade],
            ..*v
        }));
    }
    batch.tris();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_print_is_clear_at_its_edges_and_dark_in_the_middle() {
        let tex = print_texture();
        let alpha = |x: usize| tex[x * 4 + 3];
        assert!(alpha(0) < 20 && alpha(TEX_W - 1) < 20);
        let mid = (TEX_W / 3..2 * TEX_W / 3).map(alpha).max().unwrap();
        assert!(mid as f32 > COVER * 255.0 * 0.9, "{mid}");
    }

    #[test]
    fn a_strip_lies_along_the_tyres_travel() {
        // `DecalFrame`'s map: travel (dx, dz) lands on z' only.
        for d in [
            Vec2::new(3.0, 1.0),
            Vec2::new(-2.0, -5.0),
            Vec2::new(0.0, 4.0),
        ] {
            let (sin, cos) = d.x.atan2(d.y).sin_cos();
            let x = d.x * cos - d.y * sin;
            let z = d.y * cos + d.x * sin;
            assert!(x.abs() < 1e-5, "{d}: x' {x}");
            assert!((z - d.length()).abs() < 1e-5, "{d}: z' {z}");
        }
    }
}
